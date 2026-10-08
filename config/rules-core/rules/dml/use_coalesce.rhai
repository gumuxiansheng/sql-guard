// use_coalesce.rhai (DML104)
// 优先使用标准的 COALESCE 而非数据库专有的 NVL 或 ISNULL。
// COALESCE 是 SQL 标准函数，跨数据库兼容（PostgreSQL、MySQL、SQLite 等均支持）。
//
// 反面案例（会被拦截）：
//   SELECT NVL(name, 'N/A') FROM users;      -- Oracle 专有
//   SELECT ISNULL(name, 'N/A') FROM users;   -- SQL Server 专有
//
// 正面案例（放行）：
//   SELECT COALESCE(name, 'N/A') FROM users;  -- 标准函数，跨库兼容

let sql = context["sql_content"];
let upper = sql.to_upper();

if upper.contains("NVL(") {
    violations.push(violation_line("Use standard COALESCE() instead of Oracle-specific NVL(). COALESCE is cross-database compatible.", 1));
}

if upper.contains("ISNULL(") {
    violations.push(violation_line("Use standard COALESCE() instead of T-SQL-specific ISNULL(). COALESCE is cross-database compatible.", 1));
}
