//! 主键解析：配置 → StmtInfo → bks_ 表 JOIN 推断。
//!
//! 对应设计文档 §4.4 / §4.5.1。
//! 优先级：
//! 1. `RollbackConfig.primary_keys` 显式声明
//! 2. StmtInfo.create_table 中的列级 / 表级 PRIMARY KEY
//! 3. 无主键 → 返回空 Vec，调用方按 `reliable=false` 处理

use std::collections::HashMap;
/// ★ D3：`PrimaryKeyDecl` 仅在 cfg(test) 测试模块中使用（构造测试配置），
/// 编译器 dead_code 分析不看测试模块故报 unused，此处显式允许。
#[allow(unused_imports)]
use crate::config::{RollbackConfig, PrimaryKeyDecl};
use crate::rule::engine::ast::StmtInfo;
use super::strip_ident_quotes;

/// 主键解析器。
pub struct PrimaryKeyResolver {
    /// 配置显式声明：table → Vec<column>
    declared: HashMap<String, Vec<String>>,
}

impl PrimaryKeyResolver {
    pub fn new(rc: &RollbackConfig) -> Self {
        let mut declared = HashMap::new();
        for pk in &rc.primary_keys {
            declared.insert(strip_ident_quotes(&pk.table).to_lowercase(), pk.columns.clone());
        }
        PrimaryKeyResolver { declared }
    }

    /// 解析指定表的主键列。
    /// `stmt` 可选：若该语句的 create_table 字段含主键信息，作为回退来源。
    pub fn resolve(&self, table: &str, stmt: Option<&StmtInfo>) -> Vec<String> {
        let clean = strip_ident_quotes(table);
        let key = clean.to_lowercase();
        if let Some(cols) = self.declared.get(&key) {
            return cols.clone();
        }
        if let Some(s) = stmt {
            if let Some(ci) = &s.create_table {
                if !ci.primary_key_columns.is_empty() {
                    return ci.primary_key_columns.clone();
                }
                // 列级 PRIMARY KEY
                let pk_cols: Vec<String> = ci.columns.iter()
                    .filter(|c| c.is_primary_key)
                    .map(|c| c.name.clone())
                    .collect();
                if !pk_cols.is_empty() {
                    return pk_cols;
                }
            }
        }
        Vec::new()
    }

    /// 注册一个主键声明（供脚本内含 CREATE TABLE 上下文时回填）。
    /// ★ D2：当前 generator 未实现"扫描 CREATE TABLE 上下文回填主键"路径，
    /// 此方法保留用于未来扩展及测试（pk.rs 测试模块使用）。
    #[allow(dead_code)]
    pub fn register(&mut self, table: &str, columns: Vec<String>) {
        let clean = strip_ident_quotes(table).to_lowercase();
        self.declared.insert(clean, columns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::engine::ast::{StmtInfo, CreateInfo, ColumnInfo};

    fn rc_with_pk(table: &str, cols: &[&str]) -> RollbackConfig {
        let mut rc = RollbackConfig::default();
        rc.primary_keys = vec![PrimaryKeyDecl {
            table: table.to_string(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
        }];
        rc
    }

    #[test]
    fn resolve_prefers_config_declaration() {
        let rc = rc_with_pk("users", &["id"]);
        let resolver = PrimaryKeyResolver::new(&rc);
        assert_eq!(resolver.resolve("users", None), vec!["id".to_string()]);
        assert_eq!(resolver.resolve("`users`", None), vec!["id".to_string()]);
        assert_eq!(resolver.resolve("USERS", None), vec!["id".to_string()]);
    }

    #[test]
    fn resolve_falls_back_to_stmt_create_table() {
        let rc = RollbackConfig::default();
        let resolver = PrimaryKeyResolver::new(&rc);
        let mut stmt = StmtInfo {
            kind: "INSERT".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: Some(CreateInfo {
                table_name: "users".to_string(),
                primary_key_columns: vec!["id".to_string()],
                ..Default::default()
            }),
            drop_object: None, select: None, insert: None, update: None, delete: None,
            alter_table: None, truncate: None, create_view: None, create_index: None,
            transaction: None,
        };
        let _ = &mut stmt;
        assert_eq!(resolver.resolve("users", Some(&stmt)), vec!["id".to_string()]);
    }

    #[test]
    fn resolve_falls_back_to_column_level_pk() {
        let rc = RollbackConfig::default();
        let resolver = PrimaryKeyResolver::new(&rc);
        let mut stmt = StmtInfo {
            kind: "INSERT".to_string(),
            line: 1, end_line: 1, column: 0,
            create_table: Some(CreateInfo {
                table_name: "users".to_string(),
                columns: vec![
                    ColumnInfo { name: "id".to_string(), is_primary_key: true, ..Default::default() },
                    ColumnInfo { name: "name".to_string(), ..Default::default() },
                ],
                ..Default::default()
            }),
            drop_object: None, select: None, insert: None, update: None, delete: None,
            alter_table: None, truncate: None, create_view: None, create_index: None,
            transaction: None,
        };
        let _ = &mut stmt;
        assert_eq!(resolver.resolve("users", Some(&stmt)), vec!["id".to_string()]);
    }

    #[test]
    fn resolve_returns_empty_when_no_pk() {
        let rc = RollbackConfig::default();
        let resolver = PrimaryKeyResolver::new(&rc);
        assert!(resolver.resolve("users", None).is_empty());
    }

    #[test]
    fn register_overrides_later_resolves() {
        let rc = RollbackConfig::default();
        let mut resolver = PrimaryKeyResolver::new(&rc);
        assert!(resolver.resolve("orders", None).is_empty());
        resolver.register("orders", vec!["order_id".to_string()]);
        assert_eq!(resolver.resolve("orders", None), vec!["order_id".to_string()]);
    }
}
