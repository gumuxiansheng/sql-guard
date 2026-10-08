// no_redundant_index.rhai (DDL006)
// 避免冗余索引：
//   1) 避免在主键列上重复建立索引：与主键列完全相同、或为主键列前缀的非唯一索引均冗余。
//   2) 根据最左前缀原则，若某非唯一索引的列是另一索引列的真前缀，则前者冗余。
//      例如已存在 idx(a,b) 时，idx(a) 即为冗余。
// 唯一索引（UNIQUE）出于约束语义不视为冗余，不参与判断。
// 跨语句收集 CREATE TABLE 表级 INDEX + CREATE INDEX，按表聚合后统一判定，避免重复报告。
//
// 反面案例（会被拦截）：
//   -- idx_t_a 是 idx_t_a_b 的最左前缀，冗余
//   CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT, INDEX idx_t_a (a), INDEX idx_t_a_b (a, b));
//   -- idx_t2_id 与主键列完全相同，冗余
//   CREATE TABLE t2 (id INT PRIMARY KEY, a INT, INDEX idx_t2_id (id));
//
// 正面案例（放行）：
//   CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT, INDEX idx_t_a_b (a, b));
//   -- 唯一索引出于约束语义，不视为冗余
//   CREATE TABLE t3 (id INT PRIMARY KEY, a INT, UNIQUE KEY uk_t3_a (a));

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

/// 将列名列表规范化为小写、去引号的 Array。
fn normalize_cols(cols) {
    let out = [];
    for c in cols {
        out.push(strip_quotes(c).to_lower());
    }
    return out;
}

/// 判断两个规范化列名列表是否完全相等。
fn cols_equal(a, b) {
    if a.len() != b.len() { return false; }
    let i = 0;
    while i < a.len() {
        if a[i] != b[i] { return false; }
        i = i + 1;
    }
    return true;
}

/// 判断 a 是否为 b 的真前缀（a 非空、a.len() < b.len() 且逐元素相等）。
fn is_strict_prefix(a, b) {
    if a.len() == 0 { return false; }
    if a.len() >= b.len() { return false; }
    let i = 0;
    while i < a.len() {
        if a[i] != b[i] { return false; }
        i = i + 1;
    }
    return true;
}

fn cols_to_str(cols) {
    return join_strs(cols, ", ");
}

/// 从可能含 schema 前缀的表名提取最后一个标识符部分，去引号并小写，作为聚合 key。
fn table_key(tname) {
    let parts = tname.split(".");
    let last = parts[parts.len() - 1];
    return strip_quotes(last).to_lower();
}

// ===== 第一遍：按表聚合所有非唯一索引与主键列 =====
// table_indexes: key -> Array of #{name, cols, line, column}
// table_pk:      key -> Array of pk col names
// table_origin:  key -> 原始表名（用于 violation 文本，保留大小写与 schema）
// table_keys_order: 保持首次出现顺序，使输出稳定。
// 注意：Rhai 函数无法访问外层 scope 变量，因此“首次见到某表则初始化”的逻辑
// 直接内联在循环体中，而不封装为函数。
let table_indexes = #{};
let table_pk = #{};
let table_origin = #{};
let table_keys_order = [];

for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        let tname = ct.table_name();
        let key = table_key(tname);
        if !table_indexes.contains(key) {
            table_indexes[key] = [];
            table_origin[key] = tname;
            table_keys_order.push(key);
        }
        let pk_cols = normalize_cols(ct.primary_key_columns());
        if pk_cols.len() > 0 {
            table_pk[key] = pk_cols;
        }
        for idx in ct.indexes() {
            let arr = table_indexes[key];
            arr.push(#{
                "name": idx.name(),
                "cols": normalize_cols(idx.columns()),
                "line": s.line(),
                "column": s.column()
            });
            table_indexes[key] = arr;
        }
    }

    if s.has_create_index() {
        let ci = s.create_index();
        // 唯一索引跳过：约束语义使其不构成冗余
        if ci.is_unique() { continue; }
        let tname = ci.table_name();
        let key = table_key(tname);
        if !table_indexes.contains(key) {
            table_indexes[key] = [];
            table_origin[key] = tname;
            table_keys_order.push(key);
        }
        let arr = table_indexes[key];
        arr.push(#{
            "name": ci.name(),
            "cols": normalize_cols(ci.columns()),
            "line": s.line(),
            "column": s.column()
        });
        table_indexes[key] = arr;
    }
}

// ===== 第二遍：对每个表做冗余判定，每条索引最多报告一次 =====
// 注意 Rhai 值语义：从 Array 取出 map 元素得到的是副本，无法通过 map 字段回写状态。
// 因此用与 idx_list 等长的并行 flags 数组跟踪"已报告"状态。
for key in table_keys_order {
    let idx_list = table_indexes[key];
    if idx_list.len() == 0 { continue; }
    let tname = table_origin[key];
    let pk_cols = [];
    if table_pk.contains(key) {
        pk_cols = table_pk[key];
    }

    let flags = [];
    let k = 0;
    while k < idx_list.len() {
        flags.push(false);
        k = k + 1;
    }

    // 1) 与主键比较：完全相同 / 为主键真前缀 → 冗余
    if pk_cols.len() > 0 {
        let i = 0;
        while i < idx_list.len() {
            if !flags[i] {
                let idx = idx_list[i];
                if cols_equal(idx["cols"], pk_cols) {
                    violations.push(violation(
                        "Index '" + idx["name"] + "' on table '" + tname +
                        "' is redundant: its columns exactly match the primary key columns (" +
                        cols_to_str(pk_cols) + ")",
                        idx["line"], idx["column"]));
                    flags[i] = true;
                } else if is_strict_prefix(idx["cols"], pk_cols) {
                    violations.push(violation(
                        "Index '" + idx["name"] + "' on table '" + tname +
                        "' is redundant: its columns are a leftmost prefix of the primary key (" +
                        cols_to_str(pk_cols) + "), already covered by PK",
                        idx["line"], idx["column"]));
                    flags[i] = true;
                }
            }
            i = i + 1;
        }
    }

    // 2) 索引两两比较：a 是 b 的真前缀 → a 冗余（每条 a 只报告一次）
    let n = idx_list.len();
    let i = 0;
    while i < n {
        if !flags[i] {
            let a = idx_list[i];
            let j = 0;
            while j < n {
                if i != j {
                    let b = idx_list[j];
                    if is_strict_prefix(a["cols"], b["cols"]) {
                        violations.push(violation(
                            "Index '" + a["name"] + "' on table '" + tname +
                            "' is redundant: its columns (" + cols_to_str(a["cols"]) +
                            ") are a leftmost prefix of index '" + b["name"] +
                            "' (" + cols_to_str(b["cols"]) + ")",
                            a["line"], a["column"]));
                        flags[i] = true;
                        break;
                    }
                }
                j = j + 1;
            }
        }
        i = i + 1;
    }
}
