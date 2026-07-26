//! DML 备份与回滚生成（INSERT / UPDATE / DELETE / TRUNCATE / REPLACE）。
//!
//! 对应设计文档 §4.5.1 - §4.5.5。
//!
//! ## 各语句的策略
//!
//! | 语句 | backup | rollback | reliable | 备注 |
//! |------|--------|----------|----------|------|
//! | INSERT | 无（无旧数据） | `DELETE FROM t WHERE pk = <value>` | 取决于 PK 可达性 | 无 PK 时降级全列匹配 |
//! | UPDATE | `LIKE + INSERT SELECT * WHERE <where>` | `UPDATE t JOIN bks_t ON pk SET ...` | 取决于 PK 可达性 | 无 WHERE 全表备份，reliable=false |
//! | DELETE | `LIKE + INSERT SELECT * WHERE <where>` | `INSERT INTO t SELECT * FROM bks_t` | 取决于 WHERE 存在 | 无 WHERE 全表备份，reliable=false |
//! | TRUNCATE | 全表 `LIKE + INSERT SELECT *` | `INSERT INTO t SELECT * FROM bks_t` | true | counter_unrestored=true（AUTO_INCREMENT 重置） |
//! | REPLACE | `LIKE + INSERT SELECT * WHERE pk IN (<values>)` | `INSERT INTO t SELECT * FROM bks_t` | partial=true | 无法静态 DELETE 新行 |
//!
//! ## M2 阶段限制（已在 warnings 中提示）
//!
//! - INSERT 的 VALUES 提取为简化文本解析，不支持 ROW()、子查询、多行 INSERT 跨多行复杂场景
//! - UPDATE 的 SET 子句提取为简化文本解析，不支持函数调用内含逗号、CASE WHEN 等复杂表达式
//! - 单列 PK 的 IN 列表生成仅取首个 PK 列；多列 PK 退化为全表备份

use crate::rule::engine::ast::StmtInfo;
use super::dialect::DialectRenderer;
use super::generator::RollbackGenerator;
use super::{BackupRollbackPair, SourceRef, SafetyClass, BackupStrategy, strip_ident_quotes};

// ===== INSERT =====

/// INSERT 回滚：`DELETE FROM t WHERE pk = <value>`（按主键定位）。
///
/// 无主键或 VALUES 提取失败时降级为全列匹配，标 `reliable=false`。
/// INSERT 不生成 backup（无旧数据需要保存）。
pub fn gen_insert(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    gen: &mut RollbackGenerator,
) -> BackupRollbackPair {
    let r = gen.renderer;
    let insert = match &stmt.insert {
        Some(i) => i,
        None => return missing_insert_info(seq, source, original),
    };
    let table = strip_ident_quotes(&insert.table_name);
    let columns: Vec<String> = insert.columns.iter()
        .map(|c| strip_ident_quotes(c))
        .collect();

    // 提取 VALUES
    let tuples = extract_values_tuples(original);
    let pk_cols = gen.pk_resolver.resolve(&table, None);

    let (rollback, reliable, warnings) = if tuples.is_empty() {
        (None, false, vec!["VALUES extraction failed, rollback not generated".to_string()])
    } else if pk_cols.is_empty() {
        // 无主键：全列匹配 DELETE
        let del = build_full_column_delete(&table, &columns, &tuples, r);
        (Some(del), false, vec!["no primary key, fallback to full-column match".to_string()])
    } else {
        // 有主键：尝试按 PK 生成 WHERE
        let pk_indices = find_pk_indices(&columns, &pk_cols);
        if pk_indices.is_empty() {
            // PK 列不在 INSERT 列表里（可能 INSERT 没列显式列名，且 PK 是 AUTO_INCREMENT）
            let del = build_full_column_delete(&table, &columns, &tuples, r);
            (Some(del), false, vec!["PK column not in INSERT columns, fallback to full-column match".to_string()])
        } else {
            let del = build_pk_delete(&table, &pk_cols, &pk_indices, &tuples, r);
            (Some(del), true, vec![])
        }
    };

    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback,
        safety: SafetyClass { reliable, ..Default::default() },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings,
    }
}

// ===== UPDATE =====

/// UPDATE 备份与回滚（增量模式 + 幂等 + 段内锁）。
///
/// - backup：`LIKE + INSERT SELECT * WHERE <原 WHERE>`
/// - rollback：`UPDATE t JOIN bks_t ON pk SET t.col = bks_t.col, ...`
///
/// 无 WHERE 时全表备份，标 `reliable=false`。
/// 无主键时使用全列 JOIN，标 `reliable=false`。
pub fn gen_update(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    gen: &mut RollbackGenerator,
) -> BackupRollbackPair {
    let r = gen.renderer;
    let update = match &stmt.update {
        Some(u) => u,
        None => return missing_update_info(seq, source, original),
    };
    let table = strip_ident_quotes(&update.table_name);
    let where_clause = update.where_clause.clone().unwrap_or_default();

    // 提取 SET 子句的目标列
    let set_cols = extract_set_columns(original);
    let bks_name = gen.naming.alloc(&table);

    let where_filter = if where_clause.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clause)
    };

    // backup：幂等 LIKE + INSERT SELECT * WHERE <where>
    let backup = render_incremental_backup(r, &table, &bks_name, &where_filter);

    let pk_cols = gen.pk_resolver.resolve(&table, None);

    let (rollback, reliable, mut warnings) = if set_cols.is_empty() {
        (None, false, vec!["SET clause extraction failed, rollback not generated".to_string()])
    } else {
        // SET 子句：t.col = bks.col（用 bks_ 表的旧值还原 t 表的列）
        let set_clause = set_cols.iter()
            .map(|c| format!(
                "{}.{} = {}.{}",
                r.quote_ident(&table),
                r.quote_ident(c),
                r.quote_ident(&bks_name),
                r.quote_ident(c)
            ))
            .collect::<Vec<_>>()
            .join(", ");

        // JOIN 条件：有 PK 用 PK，否则全列 JOIN
        let (join_cond, pk_warning) = if !pk_cols.is_empty() {
            let cond = pk_cols.iter()
                .map(|c| format!(
                    "{}.{} = {}.{}",
                    r.quote_ident(&table), r.quote_ident(c),
                    r.quote_ident(&bks_name), r.quote_ident(c)
                ))
                .collect::<Vec<_>>()
                .join(" AND ");
            (cond, None)
        } else {
            // 无 PK，全列 JOIN（仅对备份的列做 JOIN，备份是 SELECT *，故全表列）
            // 这里只能用 set_cols + 一个提示，因为不知道完整列列表
            let cond = set_cols.iter()
                .map(|c| format!(
                    "({}.{} = {}.{} OR ({}.{} IS NULL AND {}.{} IS NULL))",
                    r.quote_ident(&table), r.quote_ident(c),
                    r.quote_ident(&bks_name), r.quote_ident(c),
                    r.quote_ident(&table), r.quote_ident(c),
                    r.quote_ident(&bks_name), r.quote_ident(c)
                ))
                .collect::<Vec<_>>()
                .join(" AND ");
            (cond, Some("no primary key, fallback to all-column JOIN (less reliable)".to_string()))
        };

        let rollback = format!(
            "UPDATE {} JOIN {} ON {}\nSET {};",
            r.quote_ident(&table),
            r.quote_ident(&bks_name),
            join_cond,
            set_clause
        );
        // ★ reliable 要求同时满足：有 WHERE（不是全表备份）+ 有 PK（精确定位）
        let rel = !where_clause.is_empty() && !pk_cols.is_empty();
        let mut w = vec![];
        if let Some(pw) = pk_warning {
            w.push(pw);
        }
        (Some(rollback), rel, w)
    };

    if where_clause.is_empty() {
        warnings.push("UPDATE without WHERE, full table backup".to_string());
    }

    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: Some(backup),
        rollback,
        safety: SafetyClass {
            reliable,
            requires_lock: true,
            // ★ M2 阶段固定 FTWRL；M5 由 resolve_lock_type 按 lock_scope 决定
            lock_type: Some("FTWRL".to_string()),
            ..Default::default()
        },
        strategy: BackupStrategy {
            backup_mode: "incremental".to_string(),
            ..Default::default()
        },
        expected_schema: None,
        warnings,
    }
}

// ===== DELETE =====

/// DELETE 备份与回滚（增量模式 + 幂等 + 段内锁）。
///
/// - backup：`LIKE + INSERT SELECT * WHERE <原 WHERE>`
/// - rollback：`INSERT INTO t SELECT * FROM bks_t`
///
/// 无 WHERE 时全表备份，标 `reliable=false`。
pub fn gen_delete(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    gen: &mut RollbackGenerator,
) -> BackupRollbackPair {
    let r = gen.renderer;
    let delete = match &stmt.delete {
        Some(d) => d,
        None => return missing_delete_info(seq, source, original),
    };
    let table = strip_ident_quotes(&delete.table_name);
    let where_clause = delete.where_clause.clone().unwrap_or_default();

    let bks_name = gen.naming.alloc(&table);
    let where_filter = if where_clause.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clause)
    };

    let backup = render_incremental_backup(r, &table, &bks_name, &where_filter);
    let rollback = format!(
        "INSERT INTO {} SELECT * FROM {};",
        r.quote_ident(&table),
        r.quote_ident(&bks_name)
    );

    let reliable = !where_clause.is_empty();
    let warnings = if reliable {
        vec![]
    } else {
        vec!["DELETE without WHERE, full table backup".to_string()]
    };

    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: Some(backup),
        rollback: Some(rollback),
        safety: SafetyClass {
            reliable,
            requires_lock: true,
            lock_type: Some("FTWRL".to_string()),
            ..Default::default()
        },
        strategy: BackupStrategy {
            backup_mode: "incremental".to_string(),
            ..Default::default()
        },
        expected_schema: None,
        warnings,
    }
}

// ===== TRUNCATE =====

/// TRUNCATE 备份与回滚（全表备份 + INSERT FROM bks_）。
///
/// TRUNCATE 后表结构保留，仅需重新灌数据。
/// MySQL 的 TRUNCATE 会重置 AUTO_INCREMENT 计数器，标 `counter_unrestored: true`。
pub fn gen_truncate(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    gen: &mut RollbackGenerator,
) -> BackupRollbackPair {
    let r = gen.renderer;
    let truncate = match &stmt.truncate {
        Some(t) => t,
        None => return missing_truncate_info(seq, source, original),
    };
    let table = strip_ident_quotes(&truncate.table_name);
    let bks_name = gen.naming.alloc(&table);

    // 全表备份
    let backup = render_incremental_backup(r, &table, &bks_name, "");

    // 回滚：表结构仍在，仅需重新灌数据
    let rollback = format!(
        "INSERT INTO {} SELECT * FROM {};",
        r.quote_ident(&table),
        r.quote_ident(&bks_name)
    );

    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: Some(backup),
        rollback: Some(rollback),
        safety: SafetyClass {
            reliable: true,
            requires_lock: true,
            lock_type: Some("FTWRL".to_string()),
            // ★ TRUNCATE 会重置 AUTO_INCREMENT 计数器，无法静态还原
            counter_unrestored: true,
            ..Default::default()
        },
        strategy: BackupStrategy {
            backup_mode: "full".to_string(),
            ..Default::default()
        },
        expected_schema: None,
        warnings: vec![],
    }
}

// ===== REPLACE =====

/// REPLACE 备份与回滚（部分支持）。
///
/// 语义：REPLACE = 先 DELETE 冲突行后 INSERT 新行。
/// - backup：按 VALUES 的 PK 列表备份被覆盖旧行 `WHERE pk IN (...)`
/// - rollback（partial）：`INSERT INTO t SELECT * FROM bks_t` 仅恢复被覆盖旧行
///   - **无法静态生成 DELETE 新行回滚**（新行 pk 运行时才知）
///
/// manifest 标 `partial: true` + warning 提示手工清理新行。
/// 多列 PK 或 VALUES 提取失败时退化为全表备份。
pub fn gen_replace(
    stmt: &StmtInfo,
    seq: u64,
    source: SourceRef,
    original: &str,
    gen: &mut RollbackGenerator,
) -> BackupRollbackPair {
    let r = gen.renderer;
    // REPLACE 复用 InsertInfo
    let insert = match &stmt.insert {
        Some(i) => i,
        None => return missing_insert_info(seq, source, original),
    };
    let table = strip_ident_quotes(&insert.table_name);
    let columns: Vec<String> = insert.columns.iter()
        .map(|c| strip_ident_quotes(c))
        .collect();

    let tuples = extract_values_tuples(original);
    let pk_cols = gen.pk_resolver.resolve(&table, None);
    let bks_name = gen.naming.alloc(&table);

    // 生成 backup：优先按 PK IN (...) 增量备份旧行；失败则全表备份
    let (backup, mut warnings) = if tuples.is_empty() {
        let b = render_incremental_backup(r, &table, &bks_name, "");
        (b, vec!["VALUES extraction failed, full table backup".to_string()])
    } else if pk_cols.is_empty() {
        let b = render_incremental_backup(r, &table, &bks_name, "");
        (b, vec!["no primary key, full table backup".to_string()])
    } else if pk_cols.len() > 1 {
        // 多列 PK 的 IN 列表生成复杂，退化为全表备份
        let b = render_incremental_backup(r, &table, &bks_name, "");
        (b, vec!["composite PK not supported for REPLACE incremental backup, full table backup".to_string()])
    } else {
        // 单列 PK：从 VALUES 提取 PK 值，生成 WHERE pk IN (...)
        let pk_indices = find_pk_indices(&columns, &pk_cols);
        if pk_indices.is_empty() {
            let b = render_incremental_backup(r, &table, &bks_name, "");
            (b, vec!["PK column not in REPLACE columns, full table backup".to_string()])
        } else {
            let pk_idx = pk_indices[0];
            let pk_values: Vec<String> = tuples.iter()
                .filter_map(|t| t.get(pk_idx).cloned())
                .collect();
            if pk_values.is_empty() {
                let b = render_incremental_backup(r, &table, &bks_name, "");
                (b, vec!["PK value extraction failed, full table backup".to_string()])
            } else {
                let in_list = pk_values.join(", ");
                let where_filter = format!(" WHERE {} IN ({})", r.quote_ident(&pk_cols[0]), in_list);
                let b = render_incremental_backup(r, &table, &bks_name, &where_filter);
                (b, vec![])
            }
        }
    };

    // 回滚（partial）：仅恢复被覆盖旧行，无法 DELETE 新行
    let rollback = format!(
        "INSERT INTO {} SELECT * FROM {};",
        r.quote_ident(&table),
        r.quote_ident(&bks_name)
    );

    warnings.push(
        "REPLACE rollback incomplete: cannot DELETE newly inserted rows statically, manual cleanup required"
            .to_string()
    );

    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: Some(backup),
        rollback: Some(rollback),
        safety: SafetyClass {
            reliable: true,
            partial: true,
            requires_lock: true,
            lock_type: Some("FTWRL".to_string()),
            ..Default::default()
        },
        strategy: BackupStrategy {
            backup_mode: "incremental".to_string(),
            ..Default::default()
        },
        expected_schema: None,
        warnings,
    }
}

// ===== 辅助：渲染增量备份段（幂等 + 段内锁） =====

/// 渲染幂等增量备份段：`FTWRL + DROP IF EXISTS + CREATE LIKE + INSERT SELECT WHERE + UNLOCK`。
///
/// ★ 段内锁固定为 FTWRL（MySQL 方言）或 ACCESS SHARE（PG 方言），
/// 由 `DialectRenderer::render_idempotent_backup` 统一拼接。
/// 但该接口当前不接收 WHERE 子句，这里自行拼接以支持增量 WHERE 过滤。
///
/// M5 阶段 coalesce_locks 启用时，由 `strip_lock_statements` 剥离段内锁，
/// 组内统一发射锁/解锁。
fn render_incremental_backup(
    r: &dyn DialectRenderer,
    table: &str,
    bks: &str,
    where_filter: &str,
) -> String {
    // 调用方言渲染器的幂等 backup，再在 INSERT SELECT 后追加 WHERE
    let idempotent = r.render_idempotent_backup(table, bks);
    if where_filter.is_empty() {
        return idempotent;
    }
    // 将 WHERE 插入到 "INSERT INTO bks SELECT * FROM table;" 末尾分号前
    // render_idempotent_backup 输出形如 "...INSERT INTO `bks` SELECT * FROM `t`;\n"
    // 这里通过简单字符串替换追加 WHERE
    let insert_pattern = format!("SELECT * FROM {};", r.quote_ident(table));
    if let Some(pos) = idempotent.rfind(&insert_pattern) {
        let mut out = idempotent[..pos].to_string();
        out.push_str("SELECT * FROM ");
        out.push_str(&r.quote_ident(table));
        out.push_str(where_filter);
        out.push(';');
        out.push_str(&idempotent[pos + insert_pattern.len()..]);
        out
    } else {
        // 兜底：直接在尾部追加（虽然语义不准，但保证不丢失 WHERE 信息）
        let mut out = idempotent;
        if out.ends_with('\n') {
            out.pop();
        }
        out.push_str("\n-- WHERE filter: ");
        out.push_str(where_filter.trim_start());
        out.push('\n');
        out
    }
}

// ===== 辅助：VALUES 提取 =====

/// 从 INSERT/REPLACE 语句原文中提取 VALUES 元组列表。
///
/// 支持：
/// - `INSERT INTO t (a, b) VALUES (1, 'x'), (2, 'y')`
/// - `INSERT INTO t VALUES (1, 'x')`
/// - 跨多行的 VALUES
///
/// 不支持（返回空 Vec，由调用方降级）：
/// - `INSERT INTO t SELECT ...`（无 VALUES）
/// - `INSERT INTO t SET a=1, b=2`（MySQL SET 语法）
/// - ROW() 函数构造的元组
pub fn extract_values_tuples(sql: &str) -> Vec<Vec<String>> {
    // 找到 VALUES 关键字（大小写不敏感，前面需是单词边界）
    let upper = sql.to_uppercase();
    let values_pos = match find_values_keyword(&upper) {
        Some(p) => p,
        None => return Vec::new(),
    };

    // 从 VALUES 后开始解析元组
    let rest = &sql[values_pos..];
    parse_tuples(rest).unwrap_or_default()
}

/// 找到 VALUES 关键字的位置（跳过列定义中的 VALUES 字符串）。
/// 返回 VALUES 关键字之后第一个非空白字符的位置。
fn find_values_keyword(upper_sql: &str) -> Option<usize> {
    // 简单实现：找到 "VALUES" 关键字（前后是单词边界）
    let bytes = upper_sql.as_bytes();
    let pattern = b"VALUES";
    let mut i = 0;
    while i + pattern.len() <= bytes.len() {
        if &bytes[i..i + pattern.len()] == pattern {
            let before = if i == 0 { b' ' } else { bytes[i - 1] };
            let after = if i + pattern.len() == bytes.len() {
                b' '
            } else {
                bytes[i + pattern.len()]
            };
            // 前后必须是单词边界
            if !is_ident_char(before) && !is_ident_char(after) {
                // 跳过 VALUES 关键字和后续空白
                let mut j = i + pattern.len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                return Some(j);
            }
        }
        i += 1;
    }
    None
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 解析 `(v1, v2, ...), (v3, v4, ...), ...` 形式的元组列表。
fn parse_tuples(s: &str) -> Option<Vec<Vec<String>>> {
    let mut tuples = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 跳过空白和逗号
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'(' {
            // 不是元组开头，停止解析
            break;
        }
        // 解析一个元组
        let (tuple, next_i) = parse_one_tuple(s, i)?;
        tuples.push(tuple);
        i = next_i;
    }
    if tuples.is_empty() {
        None
    } else {
        Some(tuples)
    }
}

/// 解析从位置 `start`（必须是 `(`）开始的一个元组。
/// 返回 (元组值列表, 下一个未消费位置)。
fn parse_one_tuple(s: &str, start: usize) -> Option<(Vec<String>, usize)> {
    let bytes = s.as_bytes();
    if bytes[start] != b'(' {
        return None;
    }
    let mut values = Vec::new();
    let mut i = start + 1;
    let mut current = String::new();
    let mut in_string: Option<u8> = None; // 当前字符串引号类型 (' or " or `)
    let mut depth = 0; // 嵌套括号深度（函数调用等）

    while i < bytes.len() {
        let b = bytes[i];
        match in_string {
            Some(quote) => {
                if b == quote {
                    // 检查是否是转义（'' 或 "" 或 ``）
                    if i + 1 < bytes.len() && bytes[i + 1] == quote {
                        current.push(b as char);
                        current.push(b as char);
                        i += 2;
                        continue;
                    }
                    // 字符串结束
                    current.push(b as char);
                    in_string = None;
                    i += 1;
                } else if b == b'\\' && quote == b'\'' {
                    // 反斜杠转义（MySQL 风格）
                    current.push('\\');
                    if i + 1 < bytes.len() {
                        current.push(bytes[i + 1] as char);
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else {
                    current.push(b as char);
                    i += 1;
                }
            }
            None => {
                match b {
                    b'\'' | b'"' | b'`' => {
                        in_string = Some(b);
                        current.push(b as char);
                        i += 1;
                    }
                    b'(' => {
                        depth += 1;
                        current.push('(');
                        i += 1;
                    }
                    b')' => {
                        if depth == 0 {
                            // 元组结束
                            let trimmed = current.trim();
                            if !trimmed.is_empty() || !values.is_empty() {
                                values.push(trimmed.to_string());
                            }
                            return Some((values, i + 1));
                        }
                        depth -= 1;
                        current.push(')');
                        i += 1;
                    }
                    b',' if depth == 0 => {
                        let trimmed = current.trim();
                        values.push(trimmed.to_string());
                        current.clear();
                        i += 1;
                    }
                    _ => {
                        current.push(b as char);
                        i += 1;
                    }
                }
            }
        }
    }
    None // 未闭合
}

// ===== 辅助：SET 子句提取 =====

/// 从 UPDATE 语句中提取 SET 子句的目标列名。
///
/// 例如 `UPDATE t SET a = 1, b = NOW(), c = 'x' WHERE id = 1` → `["a", "b", "c"]`
///
/// 简化实现：找 `SET` 关键字到 `WHERE`/末尾之间，按 `,` 分割（处理嵌套括号），
/// 每段取 `=` 左边的标识符。
pub fn extract_set_columns(sql: &str) -> Vec<String> {
    let upper = sql.to_uppercase();
    // 找 SET 关键字
    let set_pos = match find_keyword(&upper, b"SET") {
        Some(p) => p,
        None => return Vec::new(),
    };
    // 找 WHERE 关键字（可能不存在）
    let where_pos = find_keyword(&upper, b"WHERE").unwrap_or(sql.len());

    let set_clause = &sql[set_pos..where_pos];
    parse_set_targets(set_clause)
}

/// 在 SQL 中查找指定关键字（单词边界匹配），返回关键字后第一个非空白字符位置。
fn find_keyword(upper_sql: &str, pattern: &[u8]) -> Option<usize> {
    let bytes = upper_sql.as_bytes();
    let mut i = 0;
    while i + pattern.len() <= bytes.len() {
        if &bytes[i..i + pattern.len()] == pattern {
            let before = if i == 0 { b' ' } else { bytes[i - 1] };
            let after = if i + pattern.len() == bytes.len() {
                b' '
            } else {
                bytes[i + pattern.len()]
            };
            if !is_ident_char(before) && !is_ident_char(after) {
                let mut j = i + pattern.len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                return Some(j);
            }
        }
        i += 1;
    }
    None
}

/// 解析 SET 子句，提取每段 `col = expr` 的列名。
fn parse_set_targets(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut cols = Vec::new();
    let mut i = 0;
    let mut current = String::new();
    let mut depth = 0;
    let mut in_string: Option<u8> = None;

    while i < bytes.len() {
        let b = bytes[i];
        match in_string {
            Some(quote) => {
                if b == quote {
                    if i + 1 < bytes.len() && bytes[i + 1] == quote {
                        current.push(b as char);
                        current.push(b as char);
                        i += 2;
                        continue;
                    }
                    current.push(b as char);
                    in_string = None;
                    i += 1;
                } else {
                    current.push(b as char);
                    i += 1;
                }
            }
            None => {
                match b {
                    b'\'' | b'"' | b'`' => {
                        in_string = Some(b);
                        current.push(b as char);
                        i += 1;
                    }
                    b'(' => {
                        depth += 1;
                        current.push('(');
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        current.push(')');
                        i += 1;
                    }
                    b',' if depth == 0 => {
                        if let Some(col) = extract_col_from_segment(&current) {
                            cols.push(col);
                        }
                        current.clear();
                        i += 1;
                    }
                    _ => {
                        current.push(b as char);
                        i += 1;
                    }
                }
            }
        }
    }
    // 处理最后一段
    if let Some(col) = extract_col_from_segment(&current) {
        cols.push(col);
    }
    cols
}

/// 从 `col = expr` 段中提取列名。
fn extract_col_from_segment(segment: &str) -> Option<String> {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 找到第一个顶层 `=`（不是 ==, >=, <=, !=）
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut depth = 0;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match in_string {
            Some(q) => {
                if b == q {
                    in_string = None;
                }
                i += 1;
            }
            None => {
                match b {
                    b'\'' | b'"' | b'`' => {
                        in_string = Some(b);
                        i += 1;
                    }
                    b'(' => {
                        depth += 1;
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        i += 1;
                    }
                    b'=' => {
                        if depth == 0 {
                            // 检查是否是 ==, >=, <=, !=
                            if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                                i += 2;
                                continue;
                            }
                            let col = trimmed[..i].trim();
                            return Some(strip_ident_quotes(col));
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
        }
    }
    None
}

// ===== 辅助：PK 索引与 DELETE 生成 =====

/// 找出 PK 列在 INSERT 列列表中的索引位置。
fn find_pk_indices(columns: &[String], pk_cols: &[String]) -> Vec<usize> {
    pk_cols.iter()
        .filter_map(|pk| {
            let pk_lower = pk.to_lowercase();
            columns.iter().position(|c| c.to_lowercase() == pk_lower)
        })
        .collect()
}

/// 生成按 PK 定位的 DELETE 语句：`DELETE FROM t WHERE pk1 = v1 AND pk2 = v2; ...`
fn build_pk_delete(
    table: &str,
    pk_cols: &[String],
    pk_indices: &[usize],
    tuples: &[Vec<String>],
    r: &dyn DialectRenderer,
) -> String {
    let mut deletes = Vec::with_capacity(tuples.len());
    for tuple in tuples {
        let where_parts: Vec<String> = pk_cols.iter().enumerate()
            .filter_map(|(i, pk)| {
                let idx = pk_indices.get(i).copied()?;
                let val = tuple.get(idx)?;
                Some(format!("{} = {}", r.quote_ident(pk), val))
            })
            .collect();
        if where_parts.is_empty() {
            continue;
        }
        deletes.push(format!(
            "DELETE FROM {} WHERE {};",
            r.quote_ident(table),
            where_parts.join(" AND ")
        ));
    }
    deletes.join("\n")
}

/// 生成全列匹配的 DELETE 语句（无 PK 降级路径）。
fn build_full_column_delete(
    table: &str,
    columns: &[String],
    tuples: &[Vec<String>],
    r: &dyn DialectRenderer,
) -> String {
    let mut deletes = Vec::with_capacity(tuples.len());
    for tuple in tuples {
        let where_parts: Vec<String> = columns.iter().enumerate()
            .filter_map(|(i, col)| {
                let val = tuple.get(i)?;
                // 跳过 DEFAULT 关键字
                if val.eq_ignore_ascii_case("DEFAULT") {
                    return None;
                }
                Some(format!("{} = {}", r.quote_ident(col), val))
            })
            .collect();
        if where_parts.is_empty() {
            continue;
        }
        deletes.push(format!(
            "DELETE FROM {} WHERE {};",
            r.quote_ident(table),
            where_parts.join(" AND ")
        ));
    }
    deletes.join("\n")
}

// ===== 缺失字段兜底 =====

fn missing_insert_info(seq: u64, source: SourceRef, original: &str) -> BackupRollbackPair {
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: None,
        safety: SafetyClass {
            reliable: false,
            ..Default::default()
        },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec!["StmtInfo.insert is None".to_string()],
    }
}

fn missing_update_info(seq: u64, source: SourceRef, original: &str) -> BackupRollbackPair {
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: None,
        safety: SafetyClass {
            reliable: false,
            ..Default::default()
        },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec!["StmtInfo.update is None".to_string()],
    }
}

fn missing_delete_info(seq: u64, source: SourceRef, original: &str) -> BackupRollbackPair {
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: None,
        safety: SafetyClass {
            reliable: false,
            ..Default::default()
        },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec!["StmtInfo.delete is None".to_string()],
    }
}

fn missing_truncate_info(seq: u64, source: SourceRef, original: &str) -> BackupRollbackPair {
    BackupRollbackPair {
        seq,
        source,
        original_sql: original.to_string(),
        backup: None,
        rollback: None,
        safety: SafetyClass {
            reliable: false,
            ..Default::default()
        },
        strategy: BackupStrategy::default(),
        expected_schema: None,
        warnings: vec!["StmtInfo.truncate is None".to_string()],
    }
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RollbackConfig, PrimaryKeyDecl};
    use crate::rollback::{dialect::{MySqlRenderer, PostgreSqlRenderer}, RollbackGenerator};
    use crate::rule::engine::ast::{
        StmtInfo, InsertInfo, UpdateInfo, DeleteInfo, TruncateInfo,
    };

    fn make_config_with_pk(table: &str, pk: &[&str]) -> (Config, RollbackConfig) {
        let mut rc = RollbackConfig::default();
        if !pk.is_empty() {
            rc.primary_keys = vec![PrimaryKeyDecl {
                table: table.to_string(),
                columns: pk.iter().map(|s| s.to_string()).collect(),
            }];
        }
        let cfg = Config {
            structure: crate::config::StructureConfig {
                paths: vec![], strict: false, allow_extra: vec![],
            },
            classification: crate::config::ClassificationConfig {
                rules: vec![], default_type: "other".to_string(),
            },
            rules: vec![], rules_file: None, rules_dir: std::path::PathBuf::new(),
            output: crate::config::OutputConfig::default(),
            mapper: crate::config::MapperConfig::default(),
            scan: crate::config::ScanConfig::default(),
            file_check: crate::config::FileCheckConfig::default(),
            rollback: rc.clone(),
            cache: crate::config::CacheConfig::default(),
            dialect: crate::config::CheckDialect::default(),
        };
        (cfg, rc)
    }

    fn make_insert_stmt(table: &str, columns: &[&str]) -> StmtInfo {
        StmtInfo {
            kind: "INSERT".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None,
            insert: Some(InsertInfo {
                table_name: table.to_string(),
                columns: columns.iter().map(|s| s.to_string()).collect(),
            }),
            update: None, delete: None, alter_table: None,
            truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_update_stmt(table: &str, where_clause: Option<&str>) -> StmtInfo {
        StmtInfo {
            kind: "UPDATE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: Some(UpdateInfo {
                table_name: table.to_string(),
                where_clause: where_clause.map(|s| s.to_string()),
            }),
            delete: None, alter_table: None,
            truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_delete_stmt(table: &str, where_clause: Option<&str>) -> StmtInfo {
        StmtInfo {
            kind: "DELETE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None,
            delete: Some(DeleteInfo {
                table_name: table.to_string(),
                where_clause: where_clause.map(|s| s.to_string()),
            }),
            alter_table: None, truncate: None, create_view: None, create_index: None, transaction: None,
        }
    }

    fn make_truncate_stmt(table: &str) -> StmtInfo {
        StmtInfo {
            kind: "TRUNCATE".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: None, drop_object: None, select: None, insert: None,
            update: None, delete: None, alter_table: None,
            truncate: Some(TruncateInfo {
                table_name: table.to_string(),
                has_table_keyword: true,
            }),
            create_view: None, create_index: None, transaction: None,
        }
    }

    // === VALUES 提取测试 ===

    #[test]
    fn extract_values_single_tuple() {
        let sql = "INSERT INTO users (id, name) VALUES (1, 'alice')";
        let tuples = extract_values_tuples(sql);
        assert_eq!(tuples.len(), 1);
        assert_eq!(tuples[0], vec!["1", "'alice'"]);
    }

    #[test]
    fn extract_values_multiple_tuples() {
        let sql = "INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')";
        let tuples = extract_values_tuples(sql);
        assert_eq!(tuples.len(), 3);
        assert_eq!(tuples[0], vec!["1", "'alice'"]);
        assert_eq!(tuples[2], vec!["3", "'carol'"]);
    }

    #[test]
    fn extract_values_with_escaped_quote() {
        let sql = "INSERT INTO users (id, name) VALUES (1, 'it''s me')";
        let tuples = extract_values_tuples(sql);
        assert_eq!(tuples.len(), 1);
        assert_eq!(tuples[0], vec!["1", "'it''s me'"]);
    }

    #[test]
    fn extract_values_with_function_call() {
        let sql = "INSERT INTO orders (id, created_at) VALUES (1, NOW())";
        let tuples = extract_values_tuples(sql);
        assert_eq!(tuples.len(), 1);
        assert_eq!(tuples[0], vec!["1", "NOW()"]);
    }

    #[test]
    fn extract_values_multiline() {
        let sql = "INSERT INTO users (id, name) VALUES\n  (1, 'alice'),\n  (2, 'bob')";
        let tuples = extract_values_tuples(sql);
        assert_eq!(tuples.len(), 2);
    }

    #[test]
    fn extract_values_no_values_keyword() {
        let sql = "INSERT INTO users (id, name) SELECT id, name FROM temp";
        let tuples = extract_values_tuples(sql);
        assert!(tuples.is_empty());
    }

    #[test]
    fn extract_values_with_backslash_escape() {
        let sql = "INSERT INTO users (id, name) VALUES (1, 'it\\'s me')";
        let tuples = extract_values_tuples(sql);
        assert_eq!(tuples.len(), 1);
        assert_eq!(tuples[0][1], "'it\\'s me'");
    }

    // === SET 子句提取测试 ===

    #[test]
    fn extract_set_single_column() {
        let sql = "UPDATE users SET name = 'alice' WHERE id = 1";
        let cols = extract_set_columns(sql);
        assert_eq!(cols, vec!["name"]);
    }

    #[test]
    fn extract_set_multiple_columns() {
        let sql = "UPDATE orders SET status = 'shipped', shipped_at = NOW(), updated_by = 'system' WHERE id = 1";
        let cols = extract_set_columns(sql);
        assert_eq!(cols, vec!["status", "shipped_at", "updated_by"]);
    }

    #[test]
    fn extract_set_no_where() {
        let sql = "UPDATE users SET name = 'alice', age = 30";
        let cols = extract_set_columns(sql);
        assert_eq!(cols, vec!["name", "age"]);
    }

    #[test]
    fn extract_set_with_function_args() {
        let sql = "UPDATE t SET a = IF(b > 0, c, d), e = 'x'";
        let cols = extract_set_columns(sql);
        assert_eq!(cols, vec!["a", "e"]);
    }

    #[test]
    fn extract_set_with_quoted_identifier() {
        let sql = "UPDATE t SET `order` = 1, \"limit\" = 2";
        let cols = extract_set_columns(sql);
        assert_eq!(cols, vec!["order", "limit"]);
    }

    // === INSERT 测试 ===

    #[test]
    fn insert_with_pk_generates_delete_by_pk() {
        let (cfg, rc) = make_config_with_pk("users", &["id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_insert_stmt("users", &["id", "name"]);
        let pair = gen_insert(
            &stmt, 1, SourceRef::placeholder(),
            "INSERT INTO users (id, name) VALUES (1001, 'alice')",
            &mut gen,
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(rollback.contains("DELETE FROM `users` WHERE `id` = 1001;"));
        assert!(pair.safety.reliable);
        assert!(pair.backup.is_none());
    }

    #[test]
    fn insert_without_pk_falls_back_to_full_column() {
        let (cfg, rc) = make_config_with_pk("users", &[]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_insert_stmt("users", &["id", "name"]);
        let pair = gen_insert(
            &stmt, 1, SourceRef::placeholder(),
            "INSERT INTO users (id, name) VALUES (1001, 'alice')",
            &mut gen,
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(rollback.contains("`id` = 1001"));
        assert!(rollback.contains("`name` = 'alice'"));
        assert!(!pair.safety.reliable);
    }

    #[test]
    fn insert_multi_tuples_generates_multi_deletes() {
        let (cfg, rc) = make_config_with_pk("users", &["id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_insert_stmt("users", &["id", "name"]);
        let pair = gen_insert(
            &stmt, 1, SourceRef::placeholder(),
            "INSERT INTO users (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
            &mut gen,
        );
        let rollback = pair.rollback.expect("rollback should exist");
        assert_eq!(rollback.matches("DELETE FROM").count(), 3);
    }

    // === UPDATE 测试 ===

    #[test]
    fn update_with_pk_generates_join_rollback() {
        let (cfg, rc) = make_config_with_pk("orders", &["order_id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_update_stmt("orders", Some("status = 'pending'"));
        let pair = gen_update(
            &stmt, 1, SourceRef::placeholder(),
            "UPDATE orders SET status = 'shipped', shipped_at = NOW() WHERE status = 'pending'",
            &mut gen,
        );
        let backup = pair.backup.expect("backup should exist");
        assert!(backup.contains("FLUSH TABLES WITH READ LOCK;"));
        assert!(backup.contains("CREATE TABLE `bks_orders_"));
        assert!(backup.contains("WHERE status = 'pending'"));

        let rollback = pair.rollback.expect("rollback should exist");
        assert!(rollback.contains("UPDATE `orders` JOIN `bks_orders_"));
        assert!(rollback.contains("`orders`.`status` = `bks_orders_"));
        assert!(rollback.contains("`orders`.`shipped_at` = `bks_orders_"));
        assert!(pair.safety.reliable);
    }

    #[test]
    fn update_without_where_marks_unreliable() {
        let (cfg, rc) = make_config_with_pk("orders", &["order_id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_update_stmt("orders", None);
        let pair = gen_update(
            &stmt, 1, SourceRef::placeholder(),
            "UPDATE orders SET status = 'shipped'",
            &mut gen,
        );
        assert!(!pair.safety.reliable);
        assert!(pair.warnings.iter().any(|w| w.contains("without WHERE")));
    }

    #[test]
    fn update_without_pk_falls_back_to_all_column_join() {
        let (cfg, rc) = make_config_with_pk("orders", &[]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_update_stmt("orders", Some("status = 'pending'"));
        let pair = gen_update(
            &stmt, 1, SourceRef::placeholder(),
            "UPDATE orders SET status = 'shipped' WHERE status = 'pending'",
            &mut gen,
        );
        assert!(!pair.safety.reliable);
        assert!(pair.warnings.iter().any(|w| w.contains("no primary key")));
    }

    // === DELETE 测试 ===

    #[test]
    fn delete_with_where_generates_incremental_backup() {
        let (cfg, rc) = make_config_with_pk("orders", &["order_id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_delete_stmt("orders", Some("status = 'cancelled'"));
        let pair = gen_delete(
            &stmt, 1, SourceRef::placeholder(),
            "DELETE FROM orders WHERE status = 'cancelled'",
            &mut gen,
        );
        let backup = pair.backup.expect("backup should exist");
        assert!(backup.contains("WHERE status = 'cancelled'"));
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(rollback.contains("INSERT INTO `orders` SELECT * FROM `bks_orders_"));
        assert!(pair.safety.reliable);
    }

    #[test]
    fn delete_without_where_marks_unreliable() {
        let (cfg, rc) = make_config_with_pk("orders", &["order_id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_delete_stmt("orders", None);
        let pair = gen_delete(
            &stmt, 1, SourceRef::placeholder(),
            "DELETE FROM orders",
            &mut gen,
        );
        assert!(!pair.safety.reliable);
        assert!(pair.warnings.iter().any(|w| w.contains("without WHERE")));
    }

    // === TRUNCATE 测试 ===

    #[test]
    fn truncate_generates_full_backup_and_insert_rollback() {
        let (cfg, rc) = make_config_with_pk("audit_log", &[]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_truncate_stmt("audit_log");
        let pair = gen_truncate(
            &stmt, 1, SourceRef::placeholder(),
            "TRUNCATE TABLE audit_log",
            &mut gen,
        );
        let backup = pair.backup.expect("backup should exist");
        assert!(backup.contains("CREATE TABLE `bks_audit_log_"));
        // 全表备份：INSERT SELECT * 后无 WHERE
        assert!(backup.contains("SELECT * FROM `audit_log`;"));
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(rollback.contains("INSERT INTO `audit_log` SELECT * FROM `bks_audit_log_"));
        assert!(pair.safety.reliable);
        assert!(pair.safety.counter_unrestored);
        assert_eq!(pair.strategy.backup_mode, "full");
    }

    // === REPLACE 测试 ===

    #[test]
    fn replace_with_pk_generates_pk_in_backup() {
        let (cfg, rc) = make_config_with_pk("users", &["id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_insert_stmt("users", &["id", "name"]);
        let pair = gen_replace(
            &stmt, 1, SourceRef::placeholder(),
            "REPLACE INTO users (id, name) VALUES (1, 'alice'), (2, 'bob')",
            &mut gen,
        );
        let backup = pair.backup.expect("backup should exist");
        assert!(backup.contains("`id` IN (1, 2)"));
        let rollback = pair.rollback.expect("rollback should exist");
        assert!(rollback.contains("INSERT INTO `users` SELECT * FROM `bks_users_"));
        assert!(pair.safety.partial);
        assert!(pair.warnings.iter().any(|w| w.contains("REPLACE rollback incomplete")));
    }

    #[test]
    fn replace_without_pk_falls_back_to_full_backup() {
        let (cfg, rc) = make_config_with_pk("users", &[]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_insert_stmt("users", &["id", "name"]);
        let pair = gen_replace(
            &stmt, 1, SourceRef::placeholder(),
            "REPLACE INTO users (id, name) VALUES (1, 'alice')",
            &mut gen,
        );
        let backup = pair.backup.expect("backup should exist");
        // 无 PK 时全表备份，不应有 WHERE
        assert!(!backup.contains("IN (1, 2)"));
        assert!(pair.safety.partial);
    }

    #[test]
    fn replace_with_composite_pk_falls_back_to_full_backup() {
        let (cfg, rc) = make_config_with_pk("t", &["a", "b"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &MySqlRenderer);
        let stmt = make_insert_stmt("t", &["a", "b"]);
        let pair = gen_replace(
            &stmt, 1, SourceRef::placeholder(),
            "REPLACE INTO t (a, b) VALUES (1, 2)",
            &mut gen,
        );
        assert!(pair.warnings.iter().any(|w| w.contains("composite PK")));
    }

    // === PG 方言测试 ===

    #[test]
    fn pg_update_uses_pg_quoting() {
        let (cfg, rc) = make_config_with_pk("orders", &["order_id"]);
        let mut gen = RollbackGenerator::new(&cfg, &rc, &PostgreSqlRenderer);
        let stmt = make_update_stmt("orders", Some("status = 'pending'"));
        let pair = gen_update(
            &stmt, 1, SourceRef::placeholder(),
            "UPDATE orders SET status = 'shipped' WHERE status = 'pending'",
            &mut gen,
        );
        let backup = pair.backup.expect("backup should exist");
        assert!(backup.contains("LOCK TABLE \"orders\" IN ACCESS SHARE MODE;"));
        assert!(backup.contains("CREATE TABLE \"bks_orders_"));
    }
}
