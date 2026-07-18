// no_select_all.rhai - Disallow SELECT * in DML scripts
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("SELECT *") {
    violations.push("SELECT * is not allowed. Specify columns explicitly.");
}
