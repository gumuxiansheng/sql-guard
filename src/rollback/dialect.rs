//! 方言适配层。
//!
//! 封装所有方言差异，生成器只面向 `DialectRenderer` trait 编程。
//! 新增方言（v2 Oracle / SQL Server）只需实现该 trait。
//!
//! 对应设计文档 §4.6.2。

use std::str::FromStr;

/// 支持的方言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    MySql,
    PostgreSql,
}

impl FromStr for Dialect {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "mysql" | "mariadb" => Ok(Dialect::MySql),
            "postgres" | "postgresql" | "pg" => Ok(Dialect::PostgreSql),
            other => Err(format!(
                "Unsupported dialect: {} (supported: mysql, postgresql)",
                other
            )),
        }
    }
}

impl Dialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Dialect::MySql => "mysql",
            Dialect::PostgreSql => "postgresql",
        }
    }
}

/// ★ DDL 回滚策略枚举（三方言分化，取代原"两变体"枚举漏掉的 DROP TABLE 重建分支）
///
/// - `AtomicRename`：MySQL DDL 回滚主力路径。先建影子表 + 灌数据，再 `RENAME TABLE t TO t_old, shadow TO t`
///   原子切换，原表保留为 `t_old` 兜底。适用于：ALTER DROP/MODIFY COLUMN、DROP INDEX、DROP PRIMARY KEY 等
///   原表仍存在的场景。
/// - `Transactional`：PG DDL 可事务化，事务内 `DROP + CREATE LIKE + INSERT`，失败 ROLLBACK。
///   适用于 PG 所有需要原表定义的 DDL 回滚。
/// - `RebuildFromBackup`：DROP TABLE 特例。原表已被 DROP，无法 RENAME 原表为 _old，
///   只能直接 `CREATE TABLE t LIKE bks_t; INSERT INTO t SELECT * FROM bks_t;`。
///   非原子：CREATE 失败则原表无法恢复，强依赖 bks_ 表存在。manifest 标 `irreversible_if_backup_missing: true`。
///   两种方言均走此路径（PG 虽可事务化，但原表已不存在，事务内重建仍是"从 bks_ 重建"语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicStrategy {
    AtomicRename,
    Transactional,
    RebuildFromBackup,
}

/// 方言渲染器。所有 SQL 文本拼接都通过该 trait 完成，保证生成器主逻辑方言无关。
pub trait DialectRenderer: Sync {
    /// 引用标识符（MySQL: `` `name` `` / PG: `"name"`）
    fn quote_ident(&self, name: &str) -> String;

    /// CREATE TABLE 中的 LIKE 子句。
    /// - MySQL: `LIKE \`t\``
    /// - PG: `(LIKE "t" INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED)`
    fn create_table_like_clause(&self, source_table: &str) -> String;

    /// DROP INDEX 语句（MySQL: `DROP INDEX IF EXISTS i ON t` / PG: `DROP INDEX IF EXISTS i`）
    fn drop_index(&self, index_name: &str, table_name: &str) -> String;

    /// RENAME TABLE 语句（MySQL: `RENAME TABLE old TO new` / PG: `ALTER TABLE old RENAME TO new`）
    fn rename_table(&self, old: &str, new: &str) -> String;

    /// ALTER TABLE DROP PRIMARY KEY 语句
    /// （MySQL: `ALTER TABLE t DROP PRIMARY KEY` / PG: `ALTER TABLE t DROP CONSTRAINT t_pkey`，需 PG 知道约束名）
    fn drop_primary_key(&self, table: &str, constraint_name: Option<&str>) -> String;

    /// partial LIKE 警告文案（双方言不复制内容不同）
    fn partial_like_warning(&self) -> String;

    /// 方言名称（用于 manifest 中记录）
    fn name(&self) -> &'static str;

    /// ★ DDL 回滚策略（按语句类型 + 方言决定，调用方传入语句类型辅助判断）
    /// - ALTER DROP/MODIFY COLUMN、DROP INDEX、DROP PRIMARY KEY：
    ///     MySQL → AtomicRename，PG → Transactional
    /// - DROP TABLE（原表已不存在）：两种方言均 → RebuildFromBackup
    fn atomic_ddl_rollback_strategy(&self, stmt_kind: &str) -> AtomicStrategy;

    /// ★ F12 原子 RENAME 切换 rollback（MySQL）。
    /// 输出形如：
    /// ```sql
    /// SET FOREIGN_KEY_CHECKS=0;
    /// CREATE TABLE `_rb_<seq>_<table>` LIKE `bks_<table>_<date>_<seq>`;
    /// INSERT INTO `_rb_<seq>_<table>` SELECT * FROM `bks_<table>_<date>_<seq>`;
    /// RENAME TABLE `<table>` TO `<table>_old_<seq>`, `_rb_<seq>_<table>` TO `<table>`;
    /// SET FOREIGN_KEY_CHECKS=1;
    /// ```
    fn render_atomic_rename_rollback(&self, table: &str, bks: &str, shadow: &str) -> String;

    /// PG 事务内 rollback（外层 BEGIN/COMMIT 由 render.rs 统一包裹）。
    /// 输出形如：
    /// ```sql
    /// DROP TABLE IF EXISTS "table";
    /// CREATE TABLE "table" (LIKE "bks" INCLUDING ...);
    /// INSERT INTO "table" SELECT * FROM "bks";
    /// ```
    fn render_transactional_rollback(&self, table: &str, bks: &str) -> String;

    /// ★ RebuildFromBackup：DROP TABLE 回滚特例（两种方言均走此路径）。
    /// 原表已被 DROP，只能直接从 bks_ 表 LIKE 重建。非原子。
    fn render_rebuild_from_backup_rollback(&self, table: &str, bks: &str) -> String;

    /// ★ 幂等 backup 段：`DROP IF EXISTS + CREATE LIKE + INSERT SELECT`。
    /// MySQL 段内自带 `FLUSH TABLES WITH READ LOCK; ... UNLOCK TABLES;`（render coalesce 时由
    /// `strip_lock_statements` 剥离）。PG 段内自带 `LOCK TABLE t IN ACCESS SHARE MODE;`。
    fn render_idempotent_backup(&self, table: &str, bks: &str) -> String;

    /// ★ F13 schema 校验语句（只读 SELECT，驱动友好）。
    /// 由发布平台解析结果集后按 `assert_on_schema_mismatch` 决策。
    fn render_schema_check(&self, table: &str, bks: &str) -> String;

    /// F15 分区表检测语句（只读 SELECT）。
    fn render_partition_check(&self, table: &str) -> String;

    /// 从 backup 段文本中剥离段内锁语句（FTWRL / UNLOCK / LOCK TABLE ... ACCESS SHARE）。
    /// coalesce 模式下，段内锁由 render.rs 统一发射。
    fn strip_lock_statements(&self, backup_sql: &str) -> String;
}

/// MySQL 渲染器。
pub struct MySqlRenderer;

impl DialectRenderer for MySqlRenderer {
    fn quote_ident(&self, name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    fn create_table_like_clause(&self, src: &str) -> String {
        format!("LIKE {}", self.quote_ident(src))
    }

    fn drop_index(&self, idx: &str, tbl: &str) -> String {
        format!(
            "DROP INDEX IF EXISTS {} ON {}",
            self.quote_ident(idx),
            self.quote_ident(tbl)
        )
    }

    fn rename_table(&self, old: &str, new: &str) -> String {
        format!("RENAME TABLE {} TO {}", self.quote_ident(old), self.quote_ident(new))
    }

    fn drop_primary_key(&self, tbl: &str, _constraint_name: Option<&str>) -> String {
        format!("ALTER TABLE {} DROP PRIMARY KEY", self.quote_ident(tbl))
    }

    fn partial_like_warning(&self) -> String {
        "外键约束、CHECK 约束、触发器、表注释未在 CREATE TABLE LIKE 中保留".to_string()
    }

    fn name(&self) -> &'static str {
        "mysql"
    }

    fn atomic_ddl_rollback_strategy(&self, stmt_kind: &str) -> AtomicStrategy {
        match stmt_kind {
            // DROP TABLE 原表已不存在，无法 RENAME，走从 bks_ 重建路径
            "DROP_TABLE" => AtomicStrategy::RebuildFromBackup,
            // 其他 DDL（ALTER DROP/MODIFY COLUMN、DROP INDEX、DROP PRIMARY KEY 等）原表仍在，走原子 RENAME
            _ => AtomicStrategy::AtomicRename,
        }
    }

    fn render_atomic_rename_rollback(&self, table: &str, bks: &str, shadow: &str) -> String {
        let mut s = String::new();
        s.push_str("-- ★ F12 原子 RENAME 切换（MySQL）：原表保留为 _old 兜底，影子表 RENAME 为原表名\n");
        s.push_str("-- 乙-3 修正：RENAME 前关闭外键检查，避免 FK 拓扑破坏\n");
        s.push_str("SET FOREIGN_KEY_CHECKS=0;\n");
        // ★ N5 守卫：RENAME 前先 DROP 已存在的 _old / shadow，确保脚本可重跑
        let old_name = format!("{}_old_{}", table, shadow.strip_prefix("_rb_").and_then(|r| r.split('_').next()).unwrap_or("0001"));
        s.push_str(&format!("DROP TABLE IF EXISTS {};\n", self.quote_ident(&old_name)));
        s.push_str(&format!("DROP TABLE IF EXISTS {};\n", self.quote_ident(shadow)));
        s.push_str(&format!("CREATE TABLE {} LIKE {};\n", self.quote_ident(shadow), self.quote_ident(bks)));
        s.push_str(&format!("INSERT INTO {} SELECT * FROM {};\n", self.quote_ident(shadow), self.quote_ident(bks)));
        s.push_str(&format!(
            "RENAME TABLE {} TO {}, {} TO {};\n",
            self.quote_ident(table), self.quote_ident(&old_name),
            self.quote_ident(shadow), self.quote_ident(table)
        ));
        s.push_str("SET FOREIGN_KEY_CHECKS=1;\n");
        s.push_str(&format!("-- 校验通过后由 cleanup.sql 删除：DROP TABLE {};\n", self.quote_ident(&old_name)));
        s
    }

    fn render_transactional_rollback(&self, _table: &str, _bks: &str) -> String {
        // MySQL 不走 Transactional 路径（DDL 隐式提交），由 atomic_ddl_rollback_strategy 保证
        String::new()
    }

    fn render_rebuild_from_backup_rollback(&self, table: &str, bks: &str) -> String {
        let mut s = String::new();
        s.push_str("-- ★ RebuildFromBackup：DROP TABLE 回滚特例，从 bks_ 表 LIKE 重建（非原子）\n");
        s.push_str("-- 若其他表 FK 指向本表，需先关闭外键检查\n");
        s.push_str("SET FOREIGN_KEY_CHECKS=0;\n");
        s.push_str(&format!("CREATE TABLE {} LIKE {};\n", self.quote_ident(table), self.quote_ident(bks)));
        s.push_str(&format!("INSERT INTO {} SELECT * FROM {};\n", self.quote_ident(table), self.quote_ident(bks)));
        s.push_str("SET FOREIGN_KEY_CHECKS=1;\n");
        s
    }

    fn render_idempotent_backup(&self, table: &str, bks: &str) -> String {
        let mut s = String::new();
        // ★ 乙-2/N4：必须用全局 FLUSH TABLES WITH READ LOCK（无表名），不被隐式提交释放
        s.push_str("FLUSH TABLES WITH READ LOCK;\n");
        s.push_str(&format!("DROP TABLE IF EXISTS {};\n", self.quote_ident(bks)));
        s.push_str(&format!("CREATE TABLE {} LIKE {};\n", self.quote_ident(bks), self.quote_ident(table)));
        s.push_str(&format!("INSERT INTO {} SELECT * FROM {};\n", self.quote_ident(bks), self.quote_ident(table)));
        s.push_str("UNLOCK TABLES;\n");
        s
    }

    fn render_schema_check(&self, table: &str, _bks: &str) -> String {
        let mut s = String::new();
        s.push_str("-- sqlguard schema check: table exists (expected: 1)\n");
        s.push_str(&format!(
            "SELECT IF(EXISTS(\n  SELECT 1 FROM information_schema.tables\n  WHERE table_schema=DATABASE() AND table_name='{}'\n), 1, 0) AS {};\n",
            table.replace('\'', "\\'"),
            self.quote_ident("sqlguard_check_table_exists")
        ));
        s.push_str("-- sqlguard schema check: row count (compare with rollback-time value)\n");
        s.push_str(&format!(
            "SELECT COUNT(*) AS {} FROM {};\n",
            self.quote_ident("sqlguard_check_row_count"),
            self.quote_ident(table)
        ));
        s
    }

    fn render_partition_check(&self, table: &str) -> String {
        let mut s = String::new();
        s.push_str("-- sqlguard partition check: non-empty result indicates partitioned table\n");
        s.push_str(&format!(
            "SELECT partition_method AS {}\nFROM information_schema.partitions\nWHERE table_schema=DATABASE() AND table_name='{}';\n",
            self.quote_ident("sqlguard_check_partition"),
            table.replace('\'', "\\'")
        ));
        s
    }

    fn strip_lock_statements(&self, backup_sql: &str) -> String {
        backup_sql
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.eq_ignore_ascii_case("FLUSH TABLES WITH READ LOCK;")
                    && !trimmed.eq_ignore_ascii_case("UNLOCK TABLES;")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
}

/// PostgreSQL 渲染器。
pub struct PostgreSqlRenderer;

impl DialectRenderer for PostgreSqlRenderer {
    fn quote_ident(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn create_table_like_clause(&self, src: &str) -> String {
        format!(
            "(LIKE {} INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES INCLUDING COMMENTS INCLUDING GENERATED)",
            self.quote_ident(src)
        )
    }

    fn drop_index(&self, idx: &str, _tbl: &str) -> String {
        format!("DROP INDEX IF EXISTS {}", self.quote_ident(idx))
    }

    fn rename_table(&self, old: &str, new: &str) -> String {
        format!("ALTER TABLE {} RENAME TO {}", self.quote_ident(old), self.quote_ident(new))
    }

    fn drop_primary_key(&self, tbl: &str, constraint_name: Option<&str>) -> String {
        // PG 主键约束名默认是 <table>_pkey，可显式指定
        let default_name = format!("{}_pkey", tbl);
        let name = constraint_name.unwrap_or(&default_name);
        format!("ALTER TABLE {} DROP CONSTRAINT {}", self.quote_ident(tbl), self.quote_ident(name))
    }

    fn partial_like_warning(&self) -> String {
        "外键约束（REFERENCES）、触发器、表级权限、SEQUENCE（SERIAL 列）、表注释未在 LIKE ... INCLUDING 中保留；CHECK 约束已通过 INCLUDING CONSTRAINTS 保留".to_string()
    }

    fn name(&self) -> &'static str {
        "postgresql"
    }

    fn atomic_ddl_rollback_strategy(&self, stmt_kind: &str) -> AtomicStrategy {
        match stmt_kind {
            "DROP_TABLE" => AtomicStrategy::RebuildFromBackup,
            _ => AtomicStrategy::Transactional,
        }
    }

    fn render_atomic_rename_rollback(&self, _table: &str, _bks: &str, _shadow: &str) -> String {
        // PG 不走 AtomicRename 路径（DDL 可事务化），由 atomic_ddl_rollback_strategy 保证
        String::new()
    }

    fn render_transactional_rollback(&self, table: &str, bks: &str) -> String {
        let mut s = String::new();
        s.push_str("-- ★ PG 事务内 rollback（外层 BEGIN/COMMIT 由 render.rs 统一包裹）\n");
        s.push_str(&format!("DROP TABLE IF EXISTS {};\n", self.quote_ident(table)));
        s.push_str(&format!(
            "CREATE TABLE {} {};\n",
            self.quote_ident(table),
            self.create_table_like_clause(bks)
        ));
        s.push_str(&format!("INSERT INTO {} SELECT * FROM {};\n", self.quote_ident(table), self.quote_ident(bks)));
        s
    }

    fn render_rebuild_from_backup_rollback(&self, table: &str, bks: &str) -> String {
        let mut s = String::new();
        s.push_str("-- ★ RebuildFromBackup：DROP TABLE 回滚特例，从 bks_ 表 LIKE 重建（事务内）\n");
        s.push_str(&format!(
            "CREATE TABLE {} {};\n",
            self.quote_ident(table),
            self.create_table_like_clause(bks)
        ));
        s.push_str(&format!("INSERT INTO {} SELECT * FROM {};\n", self.quote_ident(table), self.quote_ident(bks)));
        s
    }

    fn render_idempotent_backup(&self, table: &str, bks: &str) -> String {
        let mut s = String::new();
        // PG 段内发 LOCK TABLE ... IN ACCESS SHARE MODE（PG 锁不被 DDL 释放）
        s.push_str(&format!("LOCK TABLE {} IN ACCESS SHARE MODE;\n", self.quote_ident(table)));
        s.push_str(&format!("DROP TABLE IF EXISTS {};\n", self.quote_ident(bks)));
        s.push_str(&format!(
            "CREATE TABLE {} {};\n",
            self.quote_ident(bks),
            self.create_table_like_clause(table)
        ));
        s.push_str(&format!("INSERT INTO {} SELECT * FROM {};\n", self.quote_ident(bks), self.quote_ident(table)));
        s
    }

    fn render_schema_check(&self, table: &str, _bks: &str) -> String {
        let mut s = String::new();
        s.push_str("-- sqlguard schema check: table exists (expected: true)\n");
        s.push_str(&format!(
            "SELECT EXISTS(\n  SELECT 1 FROM information_schema.tables\n  WHERE table_schema=current_schema() AND table_name='{}'\n) AS {};\n",
            table.replace('\'', "''"),
            self.quote_ident("sqlguard_check_table_exists")
        ));
        s.push_str("-- sqlguard schema check: row count (compare with rollback-time value)\n");
        s.push_str(&format!(
            "SELECT COUNT(*) AS {} FROM {};\n",
            self.quote_ident("sqlguard_check_row_count"),
            self.quote_ident(table)
        ));
        s
    }

    fn render_partition_check(&self, table: &str) -> String {
        let mut s = String::new();
        s.push_str("-- sqlguard partition check: non-empty result indicates partitioned table\n");
        s.push_str(&format!(
            "SELECT partition_strategy AS {}\nFROM pg_partitions\nWHERE schemaname=current_schema() AND tablename='{}';\n",
            self.quote_ident("sqlguard_check_partition"),
            table.replace('\'', "''")
        ));
        s
    }

    fn strip_lock_statements(&self, backup_sql: &str) -> String {
        backup_sql
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                // PG 备份导出会包含 `LOCK TABLE ... IN ACCESS SHARE MODE;` 锁提示语句，
                // 回滚时无需执行，应过滤掉。仅当一行**同时**包含两个标记时才视为锁语句。
                // （等价于原 De Morgan 写法 `!contains(A) || !contains(B)`，此处显式提取可读性更好。）
                let is_lock_stmt = trimmed.to_uppercase().contains("LOCK TABLE")
                    && trimmed.to_uppercase().contains("IN ACCESS SHARE MODE;");
                !is_lock_stmt
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
}

/// 工厂函数：根据 Dialect 枚举返回对应渲染器。
pub fn renderer_for(d: Dialect) -> Box<dyn DialectRenderer> {
    match d {
        Dialect::MySql => Box::new(MySqlRenderer),
        Dialect::PostgreSql => Box::new(PostgreSqlRenderer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_quote_ident_escapes_backtick() {
        let r = MySqlRenderer;
        assert_eq!(r.quote_ident("order"), "`order`");
        assert_eq!(r.quote_ident("with`tick"), "`with``tick`");
    }

    #[test]
    fn pg_quote_ident_escapes_double_quote() {
        let r = PostgreSqlRenderer;
        assert_eq!(r.quote_ident("order"), "\"order\"");
        assert_eq!(r.quote_ident("with\"quote"), "\"with\"\"quote\"");
    }

    #[test]
    fn dialect_from_str_accepts_aliases() {
        assert_eq!(Dialect::from_str("mysql").unwrap(), Dialect::MySql);
        assert_eq!(Dialect::from_str("MySQL").unwrap(), Dialect::MySql);
        assert_eq!(Dialect::from_str("mariadb").unwrap(), Dialect::MySql);
        assert_eq!(Dialect::from_str("pg").unwrap(), Dialect::PostgreSql);
        assert_eq!(Dialect::from_str("postgres").unwrap(), Dialect::PostgreSql);
        assert_eq!(Dialect::from_str("postgresql").unwrap(), Dialect::PostgreSql);
        assert!(Dialect::from_str("oracle").is_err());
    }

    #[test]
    fn mysql_atomic_strategy_drop_table_rebuilds() {
        let r = MySqlRenderer;
        assert_eq!(r.atomic_ddl_rollback_strategy("DROP_TABLE"), AtomicStrategy::RebuildFromBackup);
        assert_eq!(r.atomic_ddl_rollback_strategy("ALTER_TABLE"), AtomicStrategy::AtomicRename);
    }

    #[test]
    fn pg_atomic_strategy_drop_table_rebuilds() {
        let r = PostgreSqlRenderer;
        assert_eq!(r.atomic_ddl_rollback_strategy("DROP_TABLE"), AtomicStrategy::RebuildFromBackup);
        assert_eq!(r.atomic_ddl_rollback_strategy("ALTER_TABLE"), AtomicStrategy::Transactional);
    }

    #[test]
    fn mysql_idempotent_backup_uses_ftwrl() {
        let r = MySqlRenderer;
        let sql = r.render_idempotent_backup("users", "bks_users_20260731_0001");
        assert!(sql.contains("FLUSH TABLES WITH READ LOCK;"));
        assert!(sql.contains("DROP TABLE IF EXISTS `bks_users_20260731_0001`;"));
        assert!(sql.contains("CREATE TABLE `bks_users_20260731_0001` LIKE `users`;"));
        assert!(sql.contains("INSERT INTO `bks_users_20260731_0001` SELECT * FROM `users`;"));
        assert!(sql.contains("UNLOCK TABLES;"));
    }

    #[test]
    fn pg_idempotent_backup_uses_access_share_mode() {
        let r = PostgreSqlRenderer;
        let sql = r.render_idempotent_backup("users", "bks_users_20260731_0001");
        assert!(sql.contains("LOCK TABLE \"users\" IN ACCESS SHARE MODE;"));
        assert!(sql.contains("DROP TABLE IF EXISTS \"bks_users_20260731_0001\";"));
        assert!(sql.contains("CREATE TABLE \"bks_users_20260731_0001\" (LIKE \"users\""));
        assert!(sql.contains("INSERT INTO \"bks_users_20260731_0001\" SELECT * FROM \"users\";"));
    }

    #[test]
    fn mysql_strip_lock_removes_ftwrl_and_unlock() {
        let r = MySqlRenderer;
        let backup = "FLUSH TABLES WITH READ LOCK;\nDROP TABLE IF EXISTS `bks_x`;\nCREATE TABLE `bks_x` LIKE `x`;\nINSERT INTO `bks_x` SELECT * FROM `x`;\nUNLOCK TABLES;\n";
        let stripped = r.strip_lock_statements(backup);
        assert!(!stripped.contains("FLUSH TABLES"));
        assert!(!stripped.contains("UNLOCK TABLES"));
        assert!(stripped.contains("DROP TABLE IF EXISTS `bks_x`;"));
        assert!(stripped.contains("INSERT INTO `bks_x` SELECT * FROM `x`;"));
    }

    #[test]
    fn mysql_atomic_rename_includes_n5_guard_drops() {
        // ★ N5 守卫：RENAME 前必须先 DROP _old 和 shadow，确保脚本可重跑
        let r = MySqlRenderer;
        let sql = r.render_atomic_rename_rollback("users", "bks_users_20260731_0001", "_rb_0001_users");
        assert!(sql.contains("DROP TABLE IF EXISTS `users_old_0001`;"), "N5 guard: must DROP _old before RENAME");
        assert!(sql.contains("DROP TABLE IF EXISTS `_rb_0001_users`;"), "N5 guard: must DROP shadow before RENAME");
        assert!(sql.contains("CREATE TABLE `_rb_0001_users` LIKE `bks_users_20260731_0001`;"));
        assert!(sql.contains("RENAME TABLE `users` TO `users_old_0001`, `_rb_0001_users` TO `users`;"));
    }
}
