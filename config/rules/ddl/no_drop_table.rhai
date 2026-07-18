// no_drop_table.rhai - Disallow DROP TABLE in DDL scripts
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("DROP TABLE") || upper.contains("DROP TABLE IF EXISTS") {
    violations.push("DROP TABLE is not allowed in DDL scripts");
}
