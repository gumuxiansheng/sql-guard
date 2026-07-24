// index_naming_convention.rhai (DDL005)
// 索引命名规范：
//   非唯一索引：idx_字段名_字段名[_字段名]
//   唯一索引：  uk_字段名_字段名[_字段名]
//   主键：      pk_表名称

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

/// 根据前缀和列名列表构建期望的索引名（小写）。
fn build_expected_name(prefix, cols) {
    let cleaned = [];
    for c in cols {
        cleaned.push(strip_quotes(c).to_lower());
    }
    return prefix + join_strs(cleaned, "_");
}

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let tname = ct.table_name();
        let tname_clean = extract_identifier(tname).to_lower();

        // 表级非唯一索引 → idx_col_col
        for idx in ct.indexes() {
            let iname = idx.name();
            if iname.len() == 0 { continue; }
            let expected = build_expected_name("idx_", idx.columns());
            if iname.to_lower() != expected {
                violations.push(violation("Non-unique index '" + iname + "' on table '" + tname + "' should be named '" + expected + "' (idx_field_field)", s.line(), s.column()));
            }
        }

        // 表级唯一约束 → uk_col_col
        for u in ct.uniques() {
            let uname = u.name();
            if uname.len() == 0 { continue; }
            let expected = build_expected_name("uk_", u.columns());
            if uname.to_lower() != expected {
                violations.push(violation("Unique constraint '" + uname + "' on table '" + tname + "' should be named '" + expected + "' (uk_field_field)", s.line(), s.column()));
            }
        }

        // 命名主键约束 → pk_tablename
        let pkname = ct.primary_key_name();
        if pkname.len() > 0 {
            let expected_pk = "pk_" + tname_clean;
            if pkname.to_lower() != expected_pk {
                violations.push(violation("Primary key constraint '" + pkname + "' on table '" + tname + "' should be named '" + expected_pk + "' (pk_table)", s.line(), s.column()));
            }
        }
    }

    // CREATE INDEX → idx_ 或 uk_ 前缀
    if s.has_create_index() {
        let ci = s.create_index();
        let iname = ci.name();
        if iname.len() == 0 { continue; }
        let tname = ci.table_name();
        let prefix = "idx_";
        if ci.is_unique() { prefix = "uk_"; }
        let expected = build_expected_name(prefix, ci.columns());
        if iname.to_lower() != expected {
            violations.push(violation("Index '" + iname + "' on table '" + tname + "' should be named '" + expected + "' (" + prefix + "field_field)", s.line(), s.column()));
        }
    }
}
