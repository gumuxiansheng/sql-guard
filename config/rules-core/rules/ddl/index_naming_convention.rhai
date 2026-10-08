// index_naming_convention.rhai (DDL005)
// 索引命名规范：
//   非唯一索引：idx_表名_字段名[_字段名]
//   唯一索引：  uk_表名_字段名[_字段名]
//   主键：      pk_表名
//
// 设计原因：表名前缀避免不同表的同列组合产生同名索引
// （PostgreSQL/Oracle 等要求索引名在 schema 内唯一），
// 同时与 pk_<表名> 风格保持一致，便于运维溯源。
//
// 反面案例（会被拦截）：
//   CREATE TABLE t (a INT, b INT, INDEX my_idx (a, b));      -- 应为 idx_t_a_b
//   CREATE UNIQUE INDEX my_uk ON t (b);                      -- 应为 uk_t_b
//   CREATE TABLE t2 (id INT, PRIMARY KEY pk_t (id));         -- 应为 pk_t2
//
// 正面案例（放行）：
//   CREATE TABLE t (a INT, b INT, INDEX idx_t_a_b (a, b));
//   CREATE UNIQUE INDEX uk_t_b ON t (b);
//   CREATE TABLE t2 (id INT, PRIMARY KEY pk_t2 (id));

if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

/// 去除标识符两侧的引号（双引号 / 反引号 / 方括号）。
fn strip_quotes(name) {
    let n = name.len();
    if n < 2 { return name; }
    if name.starts_with("\"") && name.ends_with("\"") {
        return name.sub_string(1, n - 2);
    }
    if name.starts_with("`") && name.ends_with("`") {
        return name.sub_string(1, n - 2);
    }
    if name.starts_with("[") && name.ends_with("]") {
        return name.sub_string(1, n - 2);
    }
    return name;
}

/// 从可能含 schema 前缀的名字中提取最后一个标识符部分并去引号。
fn extract_identifier(name) {
    let parts = name.split(".");
    let last = parts[parts.len() - 1];
    return strip_quotes(last);
}

/// 根据前缀、表名和列名列表构建期望的索引名（小写）。
/// 形如：prefix + table + "_" + col1 + "_" + col2 ...
fn build_expected_name(prefix, table_clean, cols) {
    let cleaned = [];
    for c in cols {
        cleaned.push(strip_quotes(c).to_lower());
    }
    return prefix + table_clean + "_" + join_strs(cleaned, "_");
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let tname = ct.table_name();
        let tname_clean = extract_identifier(tname).to_lower();

        // 表级非唯一索引 → idx_<table>_col_col
        for idx in ct.indexes() {
            let iname = idx.name();
            if iname.len() == 0 { continue; }
            let expected = build_expected_name("idx_", tname_clean, idx.columns());
            if iname.to_lower() != expected {
                violations.push(violation("Non-unique index '" + iname + "' on table '" + tname + "' should be named '" + expected + "' (idx_table_field_field)", s.line(), s.column()));
            }
        }

        // 表级唯一约束 → uk_<table>_col_col
        for u in ct.uniques() {
            let uname = u.name();
            if uname.len() == 0 { continue; }
            let expected = build_expected_name("uk_", tname_clean, u.columns());
            if uname.to_lower() != expected {
                violations.push(violation("Unique constraint '" + uname + "' on table '" + tname + "' should be named '" + expected + "' (uk_table_field_field)", s.line(), s.column()));
            }
        }

        // 命名主键约束 → pk_<table>
        let pkname = ct.primary_key_name();
        if pkname.len() > 0 {
            let expected_pk = "pk_" + tname_clean;
            if pkname.to_lower() != expected_pk {
                violations.push(violation("Primary key constraint '" + pkname + "' on table '" + tname + "' should be named '" + expected_pk + "' (pk_table)", s.line(), s.column()));
            }
        }
    }

    // CREATE INDEX → idx_<table>_col 或 uk_<table>_col 前缀
    if s.has_create_index() {
        let ci = s.create_index();
        let iname = ci.name();
        if iname.len() == 0 { continue; }
        let tname = ci.table_name();
        let tname_clean = extract_identifier(tname).to_lower();
        let prefix = "idx_";
        if ci.is_unique() { prefix = "uk_"; }
        let expected = build_expected_name(prefix, tname_clean, ci.columns());
        if iname.to_lower() != expected {
            violations.push(violation("Index '" + iname + "' on table '" + tname + "' should be named '" + expected + "' (" + prefix + "table_field_field)", s.line(), s.column()));
        }
    }
}
