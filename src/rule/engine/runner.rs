use std::fs;
use std::path::Path;

use rhai::{Array, Dynamic, Engine, Map, Scope};

use crate::config::{Config, RuleConfig};
use crate::error::{SqlGuardError, Violation};

use super::ast::*;
use super::parser::parse_sql_to_ast_fb;

/// 规则脚本公共辅助函数。在每条规则脚本执行前自动 prepend，
/// 规则脚本无需 import 即可直接调用 guard_parse_error / for_each_statement / report 等。
const HELPERS_SCRIPT: &str = include_str!("../../../config/rules/lib/helpers.rhai");

/// 对单个文件应用所有适用规则。AST 在此解析一次，供该文件的所有规则复用。
///
/// `filter` 用于按 CLI 传入的 id/分组筛选规则；`RuleFilter::default()` 表示不筛选。
///
/// `engine` 由调用方构建一次后传入复用，避免每条规则重建 Rhai 引擎。
///
/// `line_offset` 用于把规则产出的行号偏移到目标坐标系：
/// - 脚本模式传 `0`
/// - Mapper 模式传 `raw_xml_line - 1`（即 `<select>` 标签起始行号减 1），
///   使违规行号指向 XML 文件中的实际行
///
/// **方言**：使用 `config.dialect` 解析 SQL。解析失败时插入一条 `PARSE` violation
/// （warning），并在 stderr 输出提示，避免静默跳过所有 AST 规则。
pub fn run_rules_for_file(
    engine: &Engine,
    file_path: &Path,
    sql_content: &str,
    script_type: &str,
    config: &Config,
    config_dir: &Path,
    filter: &RuleFilter,
    line_offset: usize,
) -> Result<Vec<Violation>, SqlGuardError> {
    let mut violations = Vec::new();

    let ast = parse_sql_to_ast_fb(sql_content, config.dialect, config.dialect_fallback);

    // 解析失败显式上报：避免 AST 规则全部静默跳过导致用户误以为合规
    // 两种失败模式：
    //   1) 顶层 tokenize 失败 → ast.parse_error = Some(...)
    //   2) 单条语句 parse_statement 失败 → 该语句 kind = "PARSE_ERROR"
    let has_parse_error_stmt = ast.statements.iter().any(|s| s.kind == "PARSE_ERROR");
    if let Some(err) = &ast.parse_error {
        eprintln!(
            "Warning: {} failed to tokenize with {} dialect ({}); AST rules will be skipped",
            file_path.display(),
            config.dialect.as_str(),
            err
        );
        violations.push(Violation {
            rule_id: "PARSE".to_string(),
            rule_name: "parse_error".to_string(),
            rule_group: Some("engine".to_string()),
            severity: "warning".to_string(),
            message: format!(
                "SQL tokenize error with {} dialect: {}",
                config.dialect.as_str(),
                err
            ),
            file_path: file_path.to_path_buf(),
            script_type: script_type.to_string(),
            // tokenize 整体失败没有语句级行号，退化到语句起始行（mapper 模式即
            // `<select>` 标签所在行）；脚本模式 line_offset=0 → None 语义不变。
            line: if line_offset > 0 {
                Some(line_offset + 1)
            } else {
                None
            },
            end_line: None,
            column: None,
        });
    } else if has_parse_error_stmt {
        let count = ast
            .statements
            .iter()
            .filter(|s| s.kind == "PARSE_ERROR")
            .count();
        eprintln!(
            "Warning: {} has {} statement(s) failed to parse with {} dialect; those statements are skipped",
            file_path.display(),
            count,
            config.dialect.as_str()
        );
        // 为每条 PARSE_ERROR 语句生成一条 violation（带行号）
        for s in ast.statements.iter().filter(|s| s.kind == "PARSE_ERROR") {
            violations.push(Violation {
                rule_id: "PARSE".to_string(),
                rule_name: "parse_error".to_string(),
                rule_group: Some("engine".to_string()),
                severity: "warning".to_string(),
                message: format!(
                    "SQL parse error with {} dialect at line {} (statement skipped)",
                    config.dialect.as_str(),
                    s.line as usize + line_offset
                ),
                file_path: file_path.to_path_buf(),
                script_type: script_type.to_string(),
                // ★ 必须叠加 line_offset：mapper 模式下 SQL 是从 XML 里抽出来的
                // 片段，行号相对片段起始。不叠加会让所有 PARSE 违规都堆在
                // 文件开头（表现为「全部定位在 <!DOCTYPE mapper> 行」）。
                line: Some(s.line as usize + line_offset),
                end_line: Some(s.end_line as usize + line_offset),
                column: Some(s.column as usize),
            });
        }
    }

    let context = RuleContext {
        sql_content: sql_content.to_string(),
        file_path: file_path.to_string_lossy().to_string(),
        file_name: file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        script_type: script_type.to_string(),
        line_count: sql_content.lines().count(),
        ast,
    };

    for rule_config in &config.rules {
        if !rule_config.enabled {
            continue;
        }
        if !rule_config.applies_to.iter().any(|t| t == script_type) {
            continue;
        }
        if !filter.matches(rule_config) {
            continue;
        }

        let script_path = config.resolve_script_path(&rule_config.script_path, config_dir);
        if !script_path.exists() {
            violations.push(Violation {
                rule_id: rule_config.id.clone(),
                rule_name: rule_config.name.clone(),
                rule_group: rule_config.group.clone(),
                severity: rule_config.severity.clone(),
                message: format!("Rule script not found: {}", script_path.display()),
                file_path: file_path.to_path_buf(),
                script_type: script_type.to_string(),
                line: None,
                end_line: None,
                column: None,
            });
            continue;
        }

        match run_single_rule(engine, &context, rule_config, &script_path, line_offset) {
            Ok(rule_violations) => violations.extend(rule_violations),
            Err(e) => {
                violations.push(Violation {
                    rule_id: rule_config.id.clone(),
                    rule_name: rule_config.name.clone(),
                    rule_group: rule_config.group.clone(),
                    severity: rule_config.severity.clone(),
                    message: format!("Rule execution error: {}", e),
                    file_path: file_path.to_path_buf(),
                    script_type: script_type.to_string(),
                    line: None,
                    end_line: None,
                    column: None,
                });
            }
        }
    }

    // 应用行内豁免：解析 -- sqlguard-disable-* 注释，过滤命中的 violation
    let exemptions = parse_exemptions(sql_content);
    if !exemptions.is_empty() {
        // Mapper 模式下 violation 行号已加 line_offset，豁免行号也需对齐
        let offset_exemptions: std::collections::HashMap<usize, Vec<String>> = exemptions
            .into_iter()
            .map(|(line, ids)| (line + line_offset, ids))
            .collect();
        let before = violations.len();
        violations.retain(|v| !is_exempted(v, &offset_exemptions));
        let filtered = before - violations.len();
        if filtered > 0 {
            eprintln!(
                "Info: {} violation(s) exempted by inline comments in {}",
                filtered,
                file_path.display()
            );
        }
    }

    Ok(violations)
}

/// 解析 SQL 内容中的行内豁免注释，返回 (行号 → 豁免的规则 ID 集合)。
///
/// 支持三种语法（不区分大小写）：
/// - `-- sqlguard-disable-next-line RULE_ID,RULE_ID`：豁免下一行的指定规则
///   （`*` 表示豁免全部规则）
/// - `-- sqlguard-disable-line RULE_ID,RULE_ID`：豁免当前行的指定规则
/// - `/* sqlguard-disable RULE_ID,RULE_ID */`：块注释，可跨行，豁免到
///   `*/` 结束（实现简化：仅识别单行块注释，与下一行/同行等效）
///
/// 行号为 1-indexed。返回的 HashMap 中 `*` 表示通配（豁免所有规则）。
fn parse_exemptions(sql_content: &str) -> std::collections::HashMap<usize, Vec<String>> {
    let mut exemptions: std::collections::HashMap<usize, Vec<String>> =
        std::collections::HashMap::new();
    let lines: Vec<&str> = sql_content.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let lower = line.to_lowercase();

        // -- sqlguard-disable-next-line RULE_ID,RULE_ID
        if let Some(pos) = lower.find("sqlguard-disable-next-line") {
            let after = &line[pos + "sqlguard-disable-next-line".len()..];
            let rule_ids = parse_rule_ids(after);
            if !rule_ids.is_empty() {
                exemptions.entry(line_no + 1).or_default().extend(rule_ids);
            }
            continue;
        }

        // -- sqlguard-disable-line RULE_ID,RULE_ID
        if let Some(pos) = lower.find("sqlguard-disable-line") {
            let after = &line[pos + "sqlguard-disable-line".len()..];
            let rule_ids = parse_rule_ids(after);
            if !rule_ids.is_empty() {
                exemptions.entry(line_no).or_default().extend(rule_ids);
            }
            continue;
        }

        // /* sqlguard-disable RULE_ID,RULE_ID */ （单行块注释）
        if let Some(pos) = lower.find("sqlguard-disable") {
            // 排除已匹配的 -next-line / -line（它们包含 sqlguard-disable 前缀）
            if !lower[pos..].starts_with("sqlguard-disable-next-line")
                && !lower[pos..].starts_with("sqlguard-disable-line")
            {
                let after = &line[pos + "sqlguard-disable".len()..];
                let rule_ids = parse_rule_ids(after);
                if !rule_ids.is_empty() {
                    exemptions.entry(line_no).or_default().extend(rule_ids);
                }
            }
        }
    }

    exemptions
}

/// 从注释文本中提取规则 ID 列表。
/// 输入形如 ` DML001, DML002 */` 或 ` DML001, *`，返回 `["DML001", "DML002"]`。
/// `*` 表示通配。识别到第一个非规则 ID 字符（如 `*/`、`--`、行尾）停止。
fn parse_rule_ids(s: &str) -> Vec<String> {
    let mut ids = Vec::new();
    // 取到 `*/` 或行尾为止
    let end = s.find("*/").unwrap_or(s.len());
    let segment = &s[..end];
    for part in segment.split(|c: char| c == ',' || c.is_whitespace()) {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 规则 ID 形如 DML001 / DDL* / *，仅接受字母数字与通配符
        if trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '*' || c == '_' || c == '-')
        {
            ids.push(trimmed.to_uppercase());
        }
    }
    ids
}

/// 判断某条 violation 是否被豁免。
fn is_exempted(v: &Violation, exemptions: &std::collections::HashMap<usize, Vec<String>>) -> bool {
    let line = match v.line {
        Some(l) => l,
        None => return false, // 无行号的 violation 不豁免（如规则脚本错误）
    };
    match exemptions.get(&line) {
        Some(ids) => {
            // 通配 * 豁免所有规则
            if ids.iter().any(|id| id == "*") {
                return true;
            }
            // 精确匹配规则 ID
            if ids.iter().any(|id| id == &v.rule_id) {
                return true;
            }
            // 前缀通配匹配（如 DML* 匹配 DML001）
            for id in ids {
                if id.ends_with('*') {
                    let prefix = &id[..id.len() - 1];
                    if v.rule_id.starts_with(prefix) {
                        return true;
                    }
                }
            }
            false
        }
        None => false,
    }
}

fn run_single_rule(
    engine: &Engine,
    context: &RuleContext,
    rule_config: &RuleConfig,
    script_path: &Path,
    line_offset: usize,
) -> Result<Vec<Violation>, SqlGuardError> {
    let script = fs::read_to_string(script_path).map_err(|e| {
        SqlGuardError::ScriptError(format!(
            "Failed to read script '{}': {}",
            script_path.display(),
            e
        ))
    })?;

    // 注入 helper 函数：将公共辅助函数 prepend 到规则脚本前，
    // 使规则脚本可以直接调用 guard_parse_error / for_each_statement / report 等。
    // helper 函数通过参数接收 context / violations，不依赖全局变量。
    let full_script = format!("{}\n{}", HELPERS_SCRIPT, script);

    let mut scope = Scope::new();

    let mut ctx_map = Map::new();
    ctx_map.insert(
        "sql_content".into(),
        Dynamic::from(context.sql_content.clone()),
    );
    ctx_map.insert("file_path".into(), Dynamic::from(context.file_path.clone()));
    ctx_map.insert("file_name".into(), Dynamic::from(context.file_name.clone()));
    ctx_map.insert(
        "script_type".into(),
        Dynamic::from(context.script_type.clone()),
    );
    ctx_map.insert(
        "line_count".into(),
        Dynamic::from_int(context.line_count as i64),
    );
    ctx_map.insert("ast".into(), Dynamic::from(context.ast.clone()));
    scope.push("context", Dynamic::from(ctx_map));

    let violations_array: Array = Array::new();
    scope.push("violations", Dynamic::from(violations_array));

    let result = engine.eval_with_scope::<Dynamic>(&mut scope, &full_script);

    match result {
        Ok(_) => {
            let final_violations = scope.get_value::<Array>("violations").unwrap_or_default();

            let violations: Vec<Violation> = final_violations
                .iter()
                .map(|v| {
                    let (msg, line, column) = if v.is_string() {
                        (v.clone_cast::<String>(), None, None)
                    } else if v.is_map() {
                        let m = v.clone_cast::<Map>();
                        let message = m
                            .get("message")
                            .and_then(|d| d.clone().try_cast::<String>())
                            .unwrap_or_else(|| "Unknown violation".to_string());
                        let line = extract_int(&m, "line");
                        let column = extract_int(&m, "column");
                        (message, line, column)
                    } else {
                        (v.to_string(), None, None)
                    };
                    Violation {
                        rule_id: rule_config.id.clone(),
                        rule_name: rule_config.name.clone(),
                        rule_group: rule_config.group.clone(),
                        severity: rule_config.severity.clone(),
                        message: msg,
                        file_path: Path::new(&context.file_path).to_path_buf(),
                        script_type: context.script_type.clone(),
                        // 应用行号偏移：脚本模式 line_offset=0 无影响；
                        // Mapper 模式把规则产出的"SQL 内行号"映射到 XML 行号
                        line: line.map(|l| (l as usize) + line_offset),
                        // end_line 自动从 AST 回填：找到包含 violation.line 的语句范围。
                        // 此处先用 line_offset 映射 line 到目标坐标系，再回查 AST
                        // （AST 中的 line 是 SQL 内行号，所以回查用原始 line，映射后写入 end_line）
                        end_line: line.and_then(|l| {
                            context
                                .ast
                                .statement_range_at(l)
                                .map(|(_, end)| (end as usize) + line_offset)
                        }),
                        column: column.map(|c| c as usize),
                    }
                })
                .collect();

            Ok(violations)
        }
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.to_lowercase().contains("runtime error") || err_msg.contains("throw") {
                Ok(vec![Violation {
                    rule_id: rule_config.id.clone(),
                    rule_name: rule_config.name.clone(),
                    rule_group: rule_config.group.clone(),
                    severity: rule_config.severity.clone(),
                    message: err_msg,
                    file_path: Path::new(&context.file_path).to_path_buf(),
                    script_type: context.script_type.clone(),
                    line: None,
                    end_line: None,
                    column: None,
                }])
            } else {
                Err(SqlGuardError::ScriptError(err_msg))
            }
        }
    }
}

/// 从 Rhai Map 中提取整数字段。Rhai 默认 INT 为 i64，开启 `only_i32` 时为 i32，这里都兼容。
fn extract_int(m: &Map, key: &str) -> Option<i64> {
    let d = m.get(key)?;
    d.clone()
        .try_cast::<i64>()
        .or_else(|| d.clone().try_cast::<i32>().map(|i| i as i64))
}

/// 构建一个 Rhai 引擎并注册所有 AST 包装类型的方法。
///
/// 引擎本身可重入，注册方法只在构建时执行一次；
/// 调用方应在外层构建一次，传 `&Engine` 给 [`run_rules_for_file`] 复用，
/// 避免每个文件/每条规则都重建引擎。
pub fn build_engine() -> Engine {
    let mut engine = Engine::new();

    // Helper 函数通过 prepend 注入每条规则脚本，增加了脚本长度和嵌套深度。
    // 提高表达式深度限制（Rhai 默认 40），避免 "Expression exceeds maximum complexity" 错误。
    engine.set_max_expr_depths(64, 64);

    // 注册所有自定义类型，使 Rhai 能正确识别和迭代包含它们的 Array
    engine.register_type_with_name::<SqlAst>("SqlAst");
    engine.register_type_with_name::<StmtInfo>("StmtInfo");
    engine.register_type_with_name::<CreateInfo>("CreateInfo");
    engine.register_type_with_name::<DropInfo>("DropInfo");
    engine.register_type_with_name::<SelectInfo>("SelectInfo");
    engine.register_type_with_name::<ColumnInfo>("ColumnInfo");
    engine.register_type_with_name::<InsertInfo>("InsertInfo");
    engine.register_type_with_name::<UpdateInfo>("UpdateInfo");
    engine.register_type_with_name::<DeleteInfo>("DeleteInfo");
    engine.register_type_with_name::<JoinInfo>("JoinInfo");
    engine.register_type_with_name::<AlterTableInfo>("AlterTableInfo");
    engine.register_type_with_name::<AlterOpInfo>("AlterOpInfo");
    engine.register_type_with_name::<TruncateInfo>("TruncateInfo");
    engine.register_type_with_name::<ViewInfo>("ViewInfo");
    engine.register_type_with_name::<CreateIndexInfo>("CreateIndexInfo");
    engine.register_type_with_name::<TransactionInfo>("TransactionInfo");
    engine.register_type_with_name::<CteInfo>("CteInfo");
    engine.register_type_with_name::<WindowFuncInfo>("WindowFuncInfo");
    engine.register_type_with_name::<ExprInfo>("ExprInfo");
    engine.register_type_with_name::<ForeignKeyInfo>("ForeignKeyInfo");
    engine.register_type_with_name::<CheckInfo>("CheckInfo");
    engine.register_type_with_name::<IndexInfo>("IndexInfo");
    engine.register_type_with_name::<UniqueInfo>("UniqueInfo");
    engine.register_type_with_name::<CommentInfo>("CommentInfo");

    // SqlAst 方法
    // 返回 Vec<CustomType> 的闭包需显式转为 Array，否则 Rhai for 循环无法迭代
    engine.register_fn("statements", |ast: &mut SqlAst| -> Array {
        ast.statements()
            .into_iter()
            .map(|s| Dynamic::from(s))
            .collect()
    });
    engine.register_fn("has_parse_error", |ast: &mut SqlAst| ast.has_parse_error());
    engine.register_fn("parse_error", |ast: &mut SqlAst| ast.parse_error());
    engine.register_fn("kinds", |ast: &mut SqlAst| -> Array {
        ast.kinds().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("create_tables", |ast: &mut SqlAst| -> Array {
        ast.create_tables()
            .into_iter()
            .map(|c| Dynamic::from(c))
            .collect()
    });
    engine.register_fn("drop_objects", |ast: &mut SqlAst| -> Array {
        ast.drop_objects()
            .into_iter()
            .map(|d| Dynamic::from(d))
            .collect()
    });
    engine.register_fn("selects", |ast: &mut SqlAst| -> Array {
        ast.selects()
            .into_iter()
            .map(|s| Dynamic::from(s))
            .collect()
    });
    engine.register_fn("has_create_table", |ast: &mut SqlAst| {
        ast.has_create_table()
    });
    engine.register_fn("has_drop_table", |ast: &mut SqlAst| ast.has_drop_table());
    engine.register_fn("has_comma_join_anywhere", |ast: &mut SqlAst| {
        ast.has_comma_join_anywhere()
    });
    engine.register_fn("comments", |ast: &mut SqlAst| -> Array {
        ast.comments()
            .into_iter()
            .map(|c| Dynamic::from(c))
            .collect()
    });

    // StmtInfo 方法
    engine.register_fn("kind", |s: &mut StmtInfo| s.kind());
    engine.register_fn("line", |s: &mut StmtInfo| s.line());
    engine.register_fn("end_line", |s: &mut StmtInfo| s.end_line());
    engine.register_fn("line_range", |s: &mut StmtInfo| s.line_range());
    engine.register_fn("column", |s: &mut StmtInfo| s.column());
    engine.register_fn("has_create_table", |s: &mut StmtInfo| s.has_create_table());
    engine.register_fn("has_drop_object", |s: &mut StmtInfo| s.has_drop_object());
    engine.register_fn("has_select", |s: &mut StmtInfo| s.has_select());
    engine.register_fn("has_insert", |s: &mut StmtInfo| s.has_insert());
    engine.register_fn("has_update", |s: &mut StmtInfo| s.has_update());
    engine.register_fn("has_delete", |s: &mut StmtInfo| s.has_delete());
    engine.register_fn("has_alter_table", |s: &mut StmtInfo| s.has_alter_table());
    engine.register_fn("has_truncate", |s: &mut StmtInfo| s.has_truncate());
    engine.register_fn("has_create_view", |s: &mut StmtInfo| s.has_create_view());
    engine.register_fn("has_create_index", |s: &mut StmtInfo| s.has_create_index());
    engine.register_fn("has_transaction", |s: &mut StmtInfo| s.has_transaction());
    engine.register_fn("create_table", |s: &mut StmtInfo| s.create_table());
    engine.register_fn("drop_object", |s: &mut StmtInfo| s.drop_object());
    engine.register_fn("select", |s: &mut StmtInfo| s.select());
    engine.register_fn("insert", |s: &mut StmtInfo| s.insert());
    engine.register_fn("update", |s: &mut StmtInfo| s.update());
    engine.register_fn("delete", |s: &mut StmtInfo| s.delete());
    engine.register_fn("alter_table", |s: &mut StmtInfo| s.alter_table());
    engine.register_fn("truncate", |s: &mut StmtInfo| s.truncate());
    engine.register_fn("create_view", |s: &mut StmtInfo| s.create_view());
    engine.register_fn("create_index", |s: &mut StmtInfo| s.create_index());
    engine.register_fn("transaction", |s: &mut StmtInfo| s.transaction());

    // CreateInfo 方法
    engine.register_fn("table_name", |c: &mut CreateInfo| c.table_name());
    engine.register_fn("columns", |c: &mut CreateInfo| -> Array {
        c.columns()
            .into_iter()
            .map(|col| Dynamic::from(col))
            .collect()
    });
    engine.register_fn("has_primary_key", |c: &mut CreateInfo| c.has_primary_key());
    engine.register_fn("primary_key_columns", |c: &mut CreateInfo| -> Array {
        c.primary_key_columns()
            .into_iter()
            .map(Dynamic::from)
            .collect()
    });
    engine.register_fn("primary_key_name", |c: &mut CreateInfo| {
        c.primary_key_name()
    });
    engine.register_fn("if_not_exists", |c: &mut CreateInfo| c.if_not_exists());
    engine.register_fn("is_create_as", |c: &mut CreateInfo| c.is_create_as());
    engine.register_fn("column_names", |c: &mut CreateInfo| -> Array {
        c.column_names().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("foreign_keys", |c: &mut CreateInfo| -> Array {
        c.foreign_keys()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("checks", |c: &mut CreateInfo| -> Array {
        c.checks().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("indexes", |c: &mut CreateInfo| -> Array {
        c.indexes().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("uniques", |c: &mut CreateInfo| -> Array {
        c.uniques().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("has_foreign_key", |c: &mut CreateInfo| c.has_foreign_key());
    engine.register_fn("has_check", |c: &mut CreateInfo| c.has_check());
    engine.register_fn("has_index", |c: &mut CreateInfo| c.has_index());
    engine.register_fn("has_unique", |c: &mut CreateInfo| c.has_unique());

    // ColumnInfo 方法
    engine.register_fn("name", |c: &mut ColumnInfo| c.name());
    engine.register_fn("data_type", |c: &mut ColumnInfo| c.data_type());
    engine.register_fn("is_primary_key", |c: &mut ColumnInfo| c.is_primary_key());
    engine.register_fn("is_not_null", |c: &mut ColumnInfo| c.is_not_null());
    engine.register_fn("is_unique", |c: &mut ColumnInfo| c.is_unique());
    engine.register_fn("default_value", |c: &mut ColumnInfo| c.default_value());
    engine.register_fn("has_default_value", |c: &mut ColumnInfo| {
        c.has_default_value()
    });
    engine.register_fn("is_auto_increment", |c: &mut ColumnInfo| {
        c.is_auto_increment()
    });
    engine.register_fn("comment", |c: &mut ColumnInfo| c.comment());
    engine.register_fn("has_comment", |c: &mut ColumnInfo| c.has_comment());
    engine.register_fn("has_check", |c: &mut ColumnInfo| c.has_check());
    engine.register_fn("references_table", |c: &mut ColumnInfo| {
        c.references_table()
    });
    engine.register_fn("has_foreign_key", |c: &mut ColumnInfo| c.has_foreign_key());
    engine.register_fn("line", |c: &mut ColumnInfo| c.line());
    engine.register_fn("column", |c: &mut ColumnInfo| c.column());

    // DropInfo 方法
    engine.register_fn("object_type", |d: &mut DropInfo| d.object_type());
    engine.register_fn("name", |d: &mut DropInfo| d.name());
    engine.register_fn("if_exists", |d: &mut DropInfo| d.if_exists());

    // SelectInfo 方法
    engine.register_fn("has_wildcard", |s: &mut SelectInfo| s.has_wildcard());
    engine.register_fn("projection", |s: &mut SelectInfo| -> Array {
        s.projection().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("has_from_table", |s: &mut SelectInfo| s.has_from_table());
    engine.register_fn("from_table", |s: &mut SelectInfo| s.from_table());
    engine.register_fn("from_table_alias", |s: &mut SelectInfo| {
        s.from_table_alias()
    });
    engine.register_fn("has_from_table_alias", |s: &mut SelectInfo| {
        s.has_from_table_alias()
    });
    engine.register_fn("joins", |s: &mut SelectInfo| -> Array {
        s.joins().into_iter().map(|j| Dynamic::from(j)).collect()
    });
    engine.register_fn("has_joins", |s: &mut SelectInfo| s.has_joins());
    engine.register_fn("has_cross_join", |s: &mut SelectInfo| s.has_cross_join());
    engine.register_fn("has_join_without_condition", |s: &mut SelectInfo| {
        s.has_join_without_condition()
    });
    engine.register_fn("has_subquery_in_from", |s: &mut SelectInfo| {
        s.has_subquery_in_from()
    });
    engine.register_fn("from_subquery_has_alias", |s: &mut SelectInfo| {
        s.from_subquery_has_alias()
    });
    engine.register_fn("has_unqualified_column", |s: &mut SelectInfo| {
        s.has_unqualified_column()
    });
    engine.register_fn("is_union", |s: &mut SelectInfo| s.is_union());
    engine.register_fn("is_union_all", |s: &mut SelectInfo| s.is_union_all());
    engine.register_fn("is_intersect", |s: &mut SelectInfo| s.is_intersect());
    engine.register_fn("is_except", |s: &mut SelectInfo| s.is_except());
    engine.register_fn("has_where", |s: &mut SelectInfo| s.has_where());
    engine.register_fn("where_clause", |s: &mut SelectInfo| s.where_clause());
    engine.register_fn("has_group_by", |s: &mut SelectInfo| s.has_group_by());
    engine.register_fn("has_having", |s: &mut SelectInfo| s.has_having());
    engine.register_fn("has_qualify", |s: &mut SelectInfo| s.has_qualify());
    engine.register_fn("has_order_by", |s: &mut SelectInfo| s.has_order_by());
    engine.register_fn("has_limit", |s: &mut SelectInfo| s.has_limit());
    engine.register_fn("has_offset", |s: &mut SelectInfo| s.has_offset());
    engine.register_fn("has_fetch", |s: &mut SelectInfo| s.has_fetch());
    engine.register_fn("has_distinct", |s: &mut SelectInfo| s.has_distinct());
    engine.register_fn("ctes", |s: &mut SelectInfo| -> Array {
        s.ctes().into_iter().map(|x| Dynamic::from(x)).collect()
    });
    engine.register_fn("has_cte", |s: &mut SelectInfo| s.has_cte());
    engine.register_fn("is_recursive", |s: &mut SelectInfo| s.is_recursive());
    engine.register_fn("subqueries", |s: &mut SelectInfo| -> Array {
        s.subqueries()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("has_subquery", |s: &mut SelectInfo| s.has_subquery());
    engine.register_fn("has_window_function", |s: &mut SelectInfo| {
        s.has_window_function()
    });
    engine.register_fn("window_functions", |s: &mut SelectInfo| -> Array {
        s.window_functions()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("where_expr", |s: &mut SelectInfo| s.where_expr());
    engine.register_fn("has_where_expr", |s: &mut SelectInfo| s.has_where_expr());
    engine.register_fn("having_expr", |s: &mut SelectInfo| s.having_expr());
    engine.register_fn("has_having_expr", |s: &mut SelectInfo| s.has_having_expr());
    engine.register_fn("projection_exprs", |s: &mut SelectInfo| -> Array {
        s.projection_exprs()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });
    engine.register_fn("has_comma_join", |s: &mut SelectInfo| s.has_comma_join());

    // JoinInfo 方法
    engine.register_fn("table_name", |j: &mut JoinInfo| j.table_name());
    engine.register_fn("join_type", |j: &mut JoinInfo| j.join_type());
    engine.register_fn("has_condition", |j: &mut JoinInfo| j.has_condition());
    engine.register_fn("alias", |j: &mut JoinInfo| j.alias());
    engine.register_fn("has_alias", |j: &mut JoinInfo| j.has_alias());
    engine.register_fn("condition_text", |j: &mut JoinInfo| j.condition_text());
    engine.register_fn("has_condition_text", |j: &mut JoinInfo| {
        j.has_condition_text()
    });
    engine.register_fn("line", |j: &mut JoinInfo| j.line());
    engine.register_fn("column", |j: &mut JoinInfo| j.column());

    // InsertInfo 方法
    engine.register_fn("table_name", |i: &mut InsertInfo| i.table_name());
    engine.register_fn("columns", |i: &mut InsertInfo| -> Array {
        i.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("has_columns", |i: &mut InsertInfo| i.has_columns());

    // UpdateInfo 方法
    engine.register_fn("table_name", |u: &mut UpdateInfo| u.table_name());
    engine.register_fn("has_where", |u: &mut UpdateInfo| u.has_where());
    engine.register_fn("where_clause", |u: &mut UpdateInfo| u.where_clause());

    // DeleteInfo 方法
    engine.register_fn("table_name", |d: &mut DeleteInfo| d.table_name());
    engine.register_fn("has_where", |d: &mut DeleteInfo| d.has_where());
    engine.register_fn("where_clause", |d: &mut DeleteInfo| d.where_clause());

    // AlterTableInfo 方法
    engine.register_fn("table_name", |a: &mut AlterTableInfo| a.table_name());
    engine.register_fn("adds_primary_key", |a: &mut AlterTableInfo| {
        a.adds_primary_key()
    });
    engine.register_fn("drops_primary_key", |a: &mut AlterTableInfo| {
        a.drops_primary_key()
    });
    engine.register_fn(
        "added_primary_key_columns",
        |a: &mut AlterTableInfo| -> Array {
            a.added_primary_key_columns()
                .into_iter()
                .map(Dynamic::from)
                .collect()
        },
    );
    engine.register_fn("operations", |a: &mut AlterTableInfo| -> Array {
        a.operations()
            .into_iter()
            .map(|x| Dynamic::from(x))
            .collect()
    });

    // AlterOpInfo 方法
    engine.register_fn("operation_type", |a: &mut AlterOpInfo| a.operation_type());
    engine.register_fn("column_name", |a: &mut AlterOpInfo| a.column_name());
    engine.register_fn("table_name", |a: &mut AlterOpInfo| a.table_name());
    engine.register_fn("constraint_name", |a: &mut AlterOpInfo| a.constraint_name());
    engine.register_fn("detail", |a: &mut AlterOpInfo| a.detail());

    // TruncateInfo 方法
    engine.register_fn("table_name", |t: &mut TruncateInfo| t.table_name());
    engine.register_fn("has_table_keyword", |t: &mut TruncateInfo| {
        t.has_table_keyword()
    });

    // ViewInfo 方法
    engine.register_fn("name", |v: &mut ViewInfo| v.name());
    engine.register_fn("materialized", |v: &mut ViewInfo| v.materialized());
    engine.register_fn("is_replace", |v: &mut ViewInfo| v.is_replace());
    engine.register_fn("column_count", |v: &mut ViewInfo| v.column_count());

    // CreateIndexInfo 方法
    engine.register_fn("name", |c: &mut CreateIndexInfo| c.name());
    engine.register_fn("table_name", |c: &mut CreateIndexInfo| c.table_name());
    engine.register_fn("columns", |c: &mut CreateIndexInfo| -> Array {
        c.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("is_unique", |c: &mut CreateIndexInfo| c.is_unique());

    // TransactionInfo 方法
    engine.register_fn("kind", |t: &mut TransactionInfo| t.kind());

    // CteInfo 方法
    engine.register_fn("name", |c: &mut CteInfo| c.name());
    engine.register_fn("column_count", |c: &mut CteInfo| c.column_count());
    engine.register_fn("is_recursive", |c: &mut CteInfo| c.is_recursive());

    // WindowFuncInfo 方法
    engine.register_fn("function_name", |w: &mut WindowFuncInfo| w.function_name());
    engine.register_fn("has_partition_by", |w: &mut WindowFuncInfo| {
        w.has_partition_by()
    });
    engine.register_fn("has_order_by", |w: &mut WindowFuncInfo| w.has_order_by());
    engine.register_fn("has_window_frame", |w: &mut WindowFuncInfo| {
        w.has_window_frame()
    });

    // ExprInfo 方法
    engine.register_fn("kind", |e: &mut ExprInfo| e.kind());
    engine.register_fn("text", |e: &mut ExprInfo| e.text());
    engine.register_fn("function_name", |e: &mut ExprInfo| e.function_name());
    engine.register_fn("is_literal", |e: &mut ExprInfo| e.is_literal());
    engine.register_fn("is_column", |e: &mut ExprInfo| e.is_column());
    engine.register_fn("is_subquery", |e: &mut ExprInfo| e.is_subquery());
    engine.register_fn("operator", |e: &mut ExprInfo| e.operator());
    engine.register_fn("column_name", |e: &mut ExprInfo| e.column_name());
    engine.register_fn("has_null_test", |e: &mut ExprInfo| e.has_null_test());
    engine.register_fn("line", |e: &mut ExprInfo| e.line());
    engine.register_fn("column", |e: &mut ExprInfo| e.column());

    // ForeignKeyInfo 方法
    engine.register_fn("name", |f: &mut ForeignKeyInfo| f.name());
    engine.register_fn("columns", |f: &mut ForeignKeyInfo| -> Array {
        f.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("foreign_table", |f: &mut ForeignKeyInfo| f.foreign_table());
    engine.register_fn("referred_columns", |f: &mut ForeignKeyInfo| -> Array {
        f.referred_columns()
            .into_iter()
            .map(Dynamic::from)
            .collect()
    });
    engine.register_fn("on_delete", |f: &mut ForeignKeyInfo| f.on_delete());
    engine.register_fn("on_update", |f: &mut ForeignKeyInfo| f.on_update());
    engine.register_fn("line", |f: &mut ForeignKeyInfo| f.line());
    engine.register_fn("column", |f: &mut ForeignKeyInfo| f.column());

    // CheckInfo 方法
    engine.register_fn("name", |c: &mut CheckInfo| c.name());
    engine.register_fn("expr_text", |c: &mut CheckInfo| c.expr_text());
    engine.register_fn("line", |c: &mut CheckInfo| c.line());
    engine.register_fn("column", |c: &mut CheckInfo| c.column());

    // IndexInfo 方法
    engine.register_fn("name", |i: &mut IndexInfo| i.name());
    engine.register_fn("columns", |i: &mut IndexInfo| -> Array {
        i.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("is_unique", |i: &mut IndexInfo| i.is_unique());
    engine.register_fn("line", |i: &mut IndexInfo| i.line());
    engine.register_fn("column", |i: &mut IndexInfo| i.column());

    // UniqueInfo 方法
    engine.register_fn("name", |u: &mut UniqueInfo| u.name());
    engine.register_fn("columns", |u: &mut UniqueInfo| -> Array {
        u.columns().into_iter().map(Dynamic::from).collect()
    });
    engine.register_fn("line", |u: &mut UniqueInfo| u.line());
    engine.register_fn("column", |u: &mut UniqueInfo| u.column());

    // CommentInfo 方法
    engine.register_fn("text", |c: &mut CommentInfo| c.text());
    engine.register_fn("line", |c: &mut CommentInfo| c.line());
    engine.register_fn("kind", |c: &mut CommentInfo| c.kind());

    engine
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // ===== 辅助函数 =====

    fn make_violation(rule_id: &str, line: Option<usize>) -> Violation {
        Violation {
            rule_id: rule_id.to_string(),
            rule_name: "test_rule".to_string(),
            rule_group: None,
            severity: "warning".to_string(),
            message: "test message".to_string(),
            file_path: PathBuf::new(),
            script_type: "sql".to_string(),
            line,
            end_line: None,
            column: None,
        }
    }

    // ===== parse_rule_ids =====

    #[test]
    fn test_parse_rule_ids_single() {
        assert_eq!(parse_rule_ids("DML001"), vec!["DML001".to_string()]);
    }

    #[test]
    fn test_parse_rule_ids_multiple_comma() {
        assert_eq!(
            parse_rule_ids("DML001, DML002"),
            vec!["DML001".to_string(), "DML002".to_string()]
        );
    }

    #[test]
    fn test_parse_rule_ids_wildcard() {
        assert_eq!(parse_rule_ids("*"), vec!["*".to_string()]);
    }

    #[test]
    fn test_parse_rule_ids_prefix_wildcard() {
        assert_eq!(parse_rule_ids("DML*"), vec!["DML*".to_string()]);
    }

    #[test]
    fn test_parse_rule_ids_block_comment_terminator() {
        assert_eq!(parse_rule_ids(" DML001 */"), vec!["DML001".to_string()]);
    }

    #[test]
    fn test_parse_rule_ids_empty_mixed() {
        assert!(parse_rule_ids("  ").is_empty());
    }

    #[test]
    fn test_parse_rule_ids_lowercase_normalized() {
        assert_eq!(parse_rule_ids("dml001"), vec!["DML001".to_string()]);
    }

    // ===== parse_exemptions =====

    #[test]
    fn test_parse_exemptions_next_line() {
        let sql = "-- sqlguard-disable-next-line DML001\nSELECT *";
        let exemptions = parse_exemptions(sql);
        assert_eq!(exemptions.get(&2), Some(&vec!["DML001".to_string()]));
    }

    #[test]
    fn test_parse_exemptions_current_line() {
        let sql = "SELECT * -- sqlguard-disable-line DML001";
        let exemptions = parse_exemptions(sql);
        assert_eq!(exemptions.get(&1), Some(&vec!["DML001".to_string()]));
    }

    #[test]
    fn test_parse_exemptions_block_comment() {
        let sql = "/* sqlguard-disable DML001 */";
        let exemptions = parse_exemptions(sql);
        assert_eq!(exemptions.get(&1), Some(&vec!["DML001".to_string()]));
    }

    #[test]
    fn test_parse_exemptions_multiple_rules() {
        let sql = "-- sqlguard-disable-next-line DML001,DML002";
        let exemptions = parse_exemptions(sql);
        assert_eq!(
            exemptions.get(&2),
            Some(&vec!["DML001".to_string(), "DML002".to_string()])
        );
    }

    #[test]
    fn test_parse_exemptions_wildcard() {
        let sql = "-- sqlguard-disable-next-line *";
        let exemptions = parse_exemptions(sql);
        assert_eq!(exemptions.get(&2), Some(&vec!["*".to_string()]));
    }

    #[test]
    fn test_parse_exemptions_plain_sql_empty() {
        let sql = "SELECT * FROM users;\nINSERT INTO t VALUES (1);";
        let exemptions = parse_exemptions(sql);
        assert!(exemptions.is_empty());
    }

    #[test]
    fn test_parse_exemptions_case_insensitive() {
        let sql = "-- SQLGUARD-DISABLE-NEXT-LINE dml001";
        let exemptions = parse_exemptions(sql);
        assert_eq!(exemptions.get(&2), Some(&vec!["DML001".to_string()]));
    }

    // ===== is_exempted =====

    #[test]
    fn test_is_exempted_exact_match() {
        let v = make_violation("DML001", Some(2));
        let mut exemptions = HashMap::new();
        exemptions.insert(2, vec!["DML001".to_string()]);
        assert!(is_exempted(&v, &exemptions));
    }

    #[test]
    fn test_is_exempted_no_match() {
        let v = make_violation("DML002", Some(2));
        let mut exemptions = HashMap::new();
        exemptions.insert(2, vec!["DML001".to_string()]);
        assert!(!is_exempted(&v, &exemptions));
    }

    #[test]
    fn test_is_exempted_wildcard() {
        let v = make_violation("DML001", Some(2));
        let mut exemptions = HashMap::new();
        exemptions.insert(2, vec!["*".to_string()]);
        assert!(is_exempted(&v, &exemptions));
    }

    #[test]
    fn test_is_exempted_prefix_wildcard() {
        let v = make_violation("DML001", Some(2));
        let mut exemptions = HashMap::new();
        exemptions.insert(2, vec!["DML*".to_string()]);
        assert!(is_exempted(&v, &exemptions));
    }

    #[test]
    fn test_is_exempted_no_line_number() {
        let v = make_violation("DML001", None);
        let mut exemptions = HashMap::new();
        exemptions.insert(1, vec!["DML001".to_string()]);
        assert!(!is_exempted(&v, &exemptions));
    }

    #[test]
    fn test_is_exempted_line_not_in_exemptions() {
        let v = make_violation("DML001", Some(5));
        let mut exemptions = HashMap::new();
        exemptions.insert(2, vec!["DML001".to_string()]);
        assert!(!is_exempted(&v, &exemptions));
    }

    // ===== RuleFilter =====

    #[test]
    fn test_rule_filter_default_is_empty() {
        assert!(RuleFilter::default().is_empty());
    }

    #[test]
    fn test_rule_filter_from_cli_include_rules() {
        let f = RuleFilter::from_cli(&Some("DML001".to_string()), &None, &None, &None);
        assert!(!f.is_empty());
        assert!(f.matches_id_group("DML001", None));
    }

    #[test]
    fn test_rule_filter_from_cli_exclude_rules() {
        let f = RuleFilter::from_cli(&None, &None, &Some("DML001".to_string()), &None);
        assert!(!f.matches_id_group("DML001", None));
        // 其他规则不受影响
        assert!(f.matches_id_group("DML002", None));
    }

    #[test]
    fn test_rule_filter_prefix_wildcard() {
        let f = RuleFilter::from_cli(&Some("DML*".to_string()), &None, &None, &None);
        assert!(f.matches_id_group("DML001", None));
        assert!(f.matches_id_group("DML002", None));
        assert!(!f.matches_id_group("DDL001", None));
    }
}
