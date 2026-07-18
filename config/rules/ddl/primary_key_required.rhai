// primary_key_required.rhai - CREATE TABLE must have a PRIMARY KEY
let sql = context["sql_content"];
let upper = sql.to_upper();
if upper.contains("CREATE TABLE") && !upper.contains("PRIMARY KEY") {
    violations.push("CREATE TABLE statement should include a PRIMARY KEY");
}
