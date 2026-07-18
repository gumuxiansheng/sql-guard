// use_coalesce.rhai (DML104)
// 优先使用标准的 COALESCE 而非数据库专有的 NVL 或 ISNULL。
// 原因：COALESCE 是 SQL 标准函数，跨数据库兼容（PostgreSQL、MySQL、SQLite 等均支持），
// 而 NVL（Oracle）和 ISNULL（SQL Server/T-SQL）是专有函数。
// SQLFluff CV02 等效规则。

let sql = context["sql_content"];
let upper = sql.to_upper();

if upper.contains("NVL(") {
    violations.push(#{
        "message": "Use standard COALESCE() instead of Oracle-specific NVL(). COALESCE is cross-database compatible.",
        "line": 1
    });
}

if upper.contains("ISNULL(") {
    violations.push(#{
        "message": "Use standard COALESCE() instead of T-SQL-specific ISNULL(). COALESCE is cross-database compatible.",
        "line": 1
    });
}
