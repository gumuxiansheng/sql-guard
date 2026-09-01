use sqlparser::ast::{
    Expr, GroupByExpr, JoinConstraint, JoinOperator, LimitClause, Query, SelectItem, SetExpr,
    SetOperator, SetQuantifier, TableFactor, TableWithJoins, WindowType,
};

use super::ast::*;

/// 递归检查整个查询树（顶层投影、集合运算各分支、FROM/JOIN 子查询）是否含 SELECT *。
pub(crate) fn query_has_wildcard(q: &Query) -> bool {
    set_expr_has_wildcard(&q.body)
}

/// 递归检查一个 SetExpr 是否含通配符。覆盖嵌套子查询，确保 SELECT * 不被漏报。
pub(crate) fn set_expr_has_wildcard(e: &SetExpr) -> bool {
    match e {
        SetExpr::Select(s) => {
            let proj_wc = s.projection.iter().any(|item| {
                matches!(
                    item,
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)
                )
            });
            let derived_wc = |tf: &TableWithJoins| -> bool {
                let rel_wc = matches!(&tf.relation, TableFactor::Derived { subquery, .. } if query_has_wildcard(subquery));
                let join_wc = tf
                    .joins
                    .iter()
                    .any(|j| matches!(&j.relation, TableFactor::Derived { subquery, .. } if query_has_wildcard(subquery)));
                rel_wc || join_wc
            };
            proj_wc || s.from.iter().any(derived_wc)
        }
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_has_wildcard(left) || set_expr_has_wildcard(right)
        }
        SetExpr::Query(inner) => query_has_wildcard(inner),
        _ => false,
    }
}

/// 分析单个 SELECT/Query，构造 SelectInfo。
///
/// 覆盖：通配符、投影、FROM/JOIN（含别名/ON 条件）、集合运算、WHERE/GROUP BY/HAVING/
/// QUALIFY、ORDER BY/LIMIT/OFFSET/FETCH（Query 层）、DISTINCT、CTE/WITH、子查询递归、
/// 窗口函数、WHERE/HAVING/投影的顶层 ExprInfo、隐式逗号 JOIN。
pub(crate) fn analyze_query(q: &Query) -> SelectInfo {
    let mut info = SelectInfo::default();

    // 检查是否为集合运算（UNION / INTERSECT / EXCEPT）。
    // 注意：ALL / DISTINCT 在 set_quantifier（而非 op）中，op 只是运算类型。
    if let SetExpr::SetOperation {
        op, set_quantifier, ..
    } = &*q.body
    {
        match op {
            SetOperator::Union => {
                info.union = true;
                info.union_all = matches!(
                    set_quantifier,
                    SetQuantifier::All | SetQuantifier::AllByName
                );
            }
            SetOperator::Intersect => info.intersect = true,
            SetOperator::Except => info.except = true,
            // Oracle 兼容语法 MINUS（等价于 EXCEPT）
            SetOperator::Minus => info.except = true,
        }
    }

    // 递归识别整个查询（含集合运算各分支、FROM/JOIN 子查询）中的 SELECT *，
    // 否则 SELECT * 出现在 UNION 分支或子查询里会被 DML001 漏报。
    info.has_wildcard = info.has_wildcard || query_has_wildcard(q);

    // CTE / WITH（在 Query 层，无论是否集合运算都要查）
    if let Some(with) = &q.with {
        info.has_cte = true;
        info.is_recursive = with.recursive;
        for cte in &with.cte_tables {
            info.ctes.push(CteInfo {
                name: cte.alias.name.to_string(),
                column_count: cte.alias.columns.len() as i64,
                is_recursive: with.recursive,
            });
        }
    }

    // Query 层的 ORDER BY / LIMIT / OFFSET / FETCH（无论是否集合运算都在 q 上）
    info.has_order_by = q.order_by.is_some();
    info.has_limit = q.limit_clause.is_some();
    info.has_offset = q.limit_clause.as_ref().is_some_and(|l| match l {
        LimitClause::LimitOffset { offset, .. } => offset.is_some(),
        LimitClause::OffsetCommaLimit { .. } => true,
    });
    info.has_fetch = q.fetch.is_some();

    // Select 层的字段（仅 SetExpr::Select 时有效）
    if let SetExpr::Select(s) = &*q.body {
        // DISTINCT
        info.has_distinct = s.distinct.is_some();

        // WHERE / GROUP BY / HAVING / QUALIFY
        info.has_where = s.selection.is_some();
        info.where_clause = s.selection.as_ref().map(|e| e.to_string());
        info.where_expr = s.selection.as_ref().map(analyze_expr);
        info.has_group_by = matches!(&s.group_by, GroupByExpr::Expressions(es, _) if !es.is_empty())
            || matches!(&s.group_by, GroupByExpr::All(_));
        info.has_having = s.having.is_some();
        info.having_expr = s.having.as_ref().map(analyze_expr);
        info.has_qualify = s.qualify.is_some();

        let has_multiple_tables = s.from.len() > 1 || s.from.iter().any(|t| !t.joins.is_empty());

        // 投影 + 窗口函数收集（在投影中扫描）
        let mut window_funcs = Vec::new();
        for item in &s.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    info.has_wildcard = true;
                    info.has_bare_wildcard = true;
                    // 裸星号也要进入 projection，否则依赖投影文本的规则（如 no_unused_join）
                    // 会拿到一个空投影，把"用了所有表"误判成"所有 JOIN 都没用"。
                    info.projection.push("*".to_string());
                    info.projection_exprs.push(ExprInfo {
                        kind: "WILDCARD".to_string(),
                        text: "*".to_string(),
                        ..Default::default()
                    });
                }
                SelectItem::QualifiedWildcard(obj, _) => {
                    info.has_wildcard = true;
                    // sqlparser 0.60 的 ObjectName Display 已带 `.*`（如 `t1.*`），
                    // 不能再拼一次，否则会得到 `t1.*.*`。
                    // `t1.*` 是"引用了 t1 的所有列"，必须进 projection 文本，
                    // 否则规则看不到 t1，会把它当成未使用的 JOIN 误报。
                    let text = obj.to_string();
                    info.projection.push(text.clone());
                    info.projection_exprs.push(ExprInfo {
                        kind: "WILDCARD".to_string(),
                        text,
                        ..Default::default()
                    });
                }
                SelectItem::UnnamedExpr(expr) => {
                    let text = expr.to_string();
                    info.projection.push(text.clone());
                    if has_multiple_tables
                        && !text.contains('.')
                        && matches!(expr, Expr::Identifier(_))
                    {
                        info.has_unqualified_column = true;
                    }
                    info.projection_exprs.push(analyze_expr(expr));
                    collect_window_funcs_in_expr(expr, &mut window_funcs);
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    let text = format!("{} AS {}", expr, alias);
                    info.projection.push(text);
                    // 仅当别名表达式是"裸列"（无表限定的标识符）才算未限定列；
                    // 函数调用（如 COUNT(*)）、字面量、计算表达式不算，否则误报。
                    if has_multiple_tables && matches!(expr, Expr::Identifier(_)) {
                        info.has_unqualified_column = true;
                    }
                    info.projection_exprs.push(analyze_expr(expr));
                    collect_window_funcs_in_expr(expr, &mut window_funcs);
                }
            }
        }

        // WHERE / HAVING 中的窗口函数（虽然窗口函数在 WHERE 中非法，但保险起见也扫一下）
        if let Some(expr) = &s.selection {
            collect_window_funcs_in_expr(expr, &mut window_funcs);
        }
        if let Some(expr) = &s.having {
            collect_window_funcs_in_expr(expr, &mut window_funcs);
        }

        if !window_funcs.is_empty() {
            info.has_window_function = true;
            info.window_functions = window_funcs;
        }

        // 分析 FROM 子句
        // 总表数 = 每个 TableWithJoins 的主表(1) + 其 JOIN 表数；s.from.len()>1 表示隐式逗号 JOIN
        info.table_count = s
            .from
            .iter()
            .map(|twj| 1 + twj.joins.len())
            .sum::<usize>() as i64;
        for table_with_joins in &s.from {
            // 分析主表
            match &table_with_joins.relation {
                TableFactor::Table { name, alias, .. } => {
                    if info.from_table.is_none() {
                        info.from_table = Some(name.to_string());
                    }
                    if info.from_table_alias.is_none() {
                        info.from_table_alias = alias.as_ref().map(|a| a.name.to_string());
                    }
                }
                TableFactor::Derived { alias, .. } => {
                    info.has_subquery_in_from = true;
                    // 默认值为 false；遇到带别名子查询时改为 true，
                    // 否则规则 subquery_alias_required 会误报"已带别名的子查询缺别名"
                    if alias.is_some() {
                        info.from_subquery_has_alias = true;
                    }
                }
                _ => {}
            }

            // 分析 JOIN
            for join in &table_with_joins.joins {
                let (table_name, join_type, has_condition, alias, condition_text) =
                    analyze_join_operator(&join.join_operator, &join.relation);
                info.joins.push(JoinInfo {
                    table_name,
                    join_type,
                    has_condition,
                    alias,
                    condition_text,
                    explicit_join_type: join_has_explicit_type(&join.join_operator),
                    line: None,
                    column: None,
                });
            }
        }

        // 隐式逗号 JOIN：FROM a, b（sqlparser 把它解析成多个 TableWithJoins 元素）
        if s.from.len() > 1 {
            info.has_comma_join = true;
        }
    }

    // 引用限定符收集（供 DML101 no_unused_join 判断 JOIN 表是否真的被使用）
    // 只有"真正使用了表的列"才算引用：投影 + WHERE + GROUP BY + HAVING + QUALIFY + ORDER BY。
    // 刻意不含 JOIN 的 ON 条件——右表通常只在 ON 条件里出现一次，那样仍属"未被使用"。
    let mut qualifiers: Vec<String> = Vec::new();
    for text in &info.projection {
        collect_qualifiers_from_text(text, &mut qualifiers);
    }
    if let Some(w) = &info.where_clause {
        collect_qualifiers_from_text(w, &mut qualifiers);
    }
    if let SetExpr::Select(s) = &*q.body {
        if let GroupByExpr::Expressions(exprs, _) = &s.group_by {
            for e in exprs {
                collect_qualifiers_from_text(&e.to_string(), &mut qualifiers);
            }
        }
        if let Some(h) = &s.having {
            collect_qualifiers_from_text(&h.to_string(), &mut qualifiers);
        }
        if let Some(ql) = &s.qualify {
            collect_qualifiers_from_text(&ql.to_string(), &mut qualifiers);
        }
    }
    // ORDER BY 在 Query 层（集合运算整体排序）。sqlparser 0.60：OrderBy { kind, interpolate }，
    // 表达式在 OrderByKind::Expressions 中。
    if let Some(ob) = &q.order_by {
        if let sqlparser::ast::OrderByKind::Expressions(exprs) = &ob.kind {
            for o in exprs {
                collect_qualifiers_from_text(&o.expr.to_string(), &mut qualifiers);
            }
        }
    }
    info.referenced_qualifiers = qualifiers;

    // 子查询递归收集（FROM 子查询 / WHERE 子查询 / EXISTS / 集合运算的括号分支）
    let mut subs = Vec::new();
    collect_subqueries_in_query(q, &mut subs, 0);
    if !subs.is_empty() {
        info.has_subquery = true;
        info.subqueries = subs;
    }

    info
}

/// 从一段 SQL 文本中抽取"表限定符"（`t1.col` / `t1.*` / `SUM(t2.amt)` 中的 `t1`/`t2`）。
///
/// 用于判断某张表是否真的被查询体引用（DML101 no_unused_join）。实现要点：
/// - 取标识符中**最后一个点之前**的部分：`t1.col`/`t1.*` → `T1`，
///   `ofsm.cdeorg.col`（三段式）→ `OFSM.CDEORG`，与表名/别名对齐；
/// - 单引号字符串字面量内部整体跳过，避免 `'a.b'` 被误当成列引用；
/// - 纯数字字面量（如 `1.5`）不产生限定符；
/// - 结果统一大写并去重，便于大小写不敏感比较。
pub(crate) fn collect_qualifiers_from_text(text: &str, out: &mut Vec<String>) {
    let mut token = String::new();
    let mut in_string = false;

    let flush = |token: &mut String, out: &mut Vec<String>| {
        let t = token.trim().trim_matches('"').trim_matches('\'');
        if let Some(pos) = t.rfind('.') {
            let q = t[..pos].trim().trim_matches('"').to_uppercase();
            let is_numeric = q.chars().next().is_some_and(|c| c.is_ascii_digit());
            if !q.is_empty() && !is_numeric && !out.contains(&q) {
                out.push(q);
            }
        }
        token.clear();
    };

    for c in text.chars() {
        if c == '\'' {
            flush(&mut token, out);
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if c.is_alphanumeric() || c == '_' || c == '$' || c == '.' || c == '"' {
            token.push(c);
        } else {
            flush(&mut token, out);
        }
    }
    flush(&mut token, out);
}

/// 分析一个 JOIN 的操作符与目标表，返回
/// (table_name, join_type, has_condition, alias, condition_text)。
pub(crate) fn analyze_join_operator(
    op: &JoinOperator,
    relation: &TableFactor,
) -> (String, String, bool, Option<String>, Option<String>) {
    let (table_name, alias) = table_factor_name_and_alias(relation);
    match op {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::Semi(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::Anti(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint) => {
            let join_type = match op {
                JoinOperator::Join(_) => "INNER",
                JoinOperator::Inner(_) => "INNER",
                JoinOperator::Left(_) => "LEFT",
                JoinOperator::LeftOuter(_) => "LEFT",
                JoinOperator::Right(_) => "RIGHT",
                JoinOperator::RightOuter(_) => "RIGHT",
                JoinOperator::FullOuter(_) => "FULL",
                JoinOperator::Semi(_) => "SEMI",
                JoinOperator::LeftSemi(_) => "LEFT_SEMI",
                JoinOperator::RightSemi(_) => "RIGHT_SEMI",
                JoinOperator::Anti(_) => "ANTI",
                JoinOperator::LeftAnti(_) => "LEFT_ANTI",
                JoinOperator::RightAnti(_) => "RIGHT_ANTI",
                _ => "OTHER",
            }
            .to_string();
            let (has_condition, condition_text) = analyze_join_constraint(constraint);
            (table_name, join_type, has_condition, alias, condition_text)
        }
        JoinOperator::CrossJoin(_) => (table_name, "CROSS".to_string(), true, alias, None),
        _ => (table_name, "OTHER".to_string(), false, alias, None),
    }
}

/// 判断 JOIN 关键字是否显式指定了类型（LEFT / INNER / RIGHT / FULL / CROSS 等）。
/// sqlparser 中裸 `JOIN` 解析为 `JoinOperator::Join`，`INNER JOIN` 等显式类型解析为各自的变体，
/// 因此仅裸 `JOIN` 返回 false。供"JOIN 必须显式指定类型"规则使用。
pub(crate) fn join_has_explicit_type(op: &JoinOperator) -> bool {
    !matches!(op, JoinOperator::Join(_))
}

/// 从 TableFactor 提取表名（或派生表的字符串形式）与别名。
pub(crate) fn table_factor_name_and_alias(tf: &TableFactor) -> (String, Option<String>) {
    match tf {
        TableFactor::Table { name, alias, .. } => {
            (name.to_string(), alias.as_ref().map(|a| a.name.to_string()))
        }
        TableFactor::Derived {
            alias, subquery, ..
        } => (
            format!("({})", subquery),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::TableFunction { expr, alias } => (
            format!("TABLE({})", expr),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::Function { name, alias, .. } => {
            (name.to_string(), alias.as_ref().map(|a| a.name.to_string()))
        }
        _ => (tf.to_string(), None),
    }
}

/// 分析 JoinConstraint，返回 (has_condition, condition_text)。
/// On(expr) 时 condition_text = Some(expr.to_string())。
pub(crate) fn analyze_join_constraint(constraint: &JoinConstraint) -> (bool, Option<String>) {
    match constraint {
        JoinConstraint::On(expr) => (true, Some(expr.to_string())),
        JoinConstraint::Using(_) => (true, None),
        JoinConstraint::Natural => (true, None),
        JoinConstraint::None => (false, None),
    }
}

/// 收集查询中所有子查询，递归下钻（深度限制 20 层防止恶意/异常输入导致栈溢出）。
///
/// 覆盖：
/// - `TableFactor::Derived { subquery, .. }`（FROM 子查询，含 JOIN 中的）
/// - `Expr::Subquery(Box<Query>)` / `Expr::Exists { subquery }` / `Expr::InSubquery`
///   / `Expr::ArraySubquery`（WHERE/SELECT 中的标量子查询/EXISTS/IN）
/// - `SetExpr::Query(inner)`（集合运算的括号分支）
/// - 递归下钻到子查询的子查询
pub(crate) fn collect_subqueries_in_query(q: &Query, out: &mut Vec<SelectInfo>, depth: usize) {
    if depth > 20 {
        return;
    }
    // WITH 中的 CTE 子查询也算（每个 Cte.query 都是子查询）
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            out.push(analyze_query(&cte.query));
            collect_subqueries_in_query(&cte.query, out, depth + 1);
        }
    }

    match &*q.body {
        SetExpr::Select(s) => {
            // FROM / JOIN 中的 Derived
            for twj in &s.from {
                if let TableFactor::Derived { subquery, .. } = &twj.relation {
                    out.push(analyze_query(subquery));
                    collect_subqueries_in_query(subquery, out, depth + 1);
                }
                for j in &twj.joins {
                    if let TableFactor::Derived { subquery, .. } = &j.relation {
                        out.push(analyze_query(subquery));
                        collect_subqueries_in_query(subquery, out, depth + 1);
                    }
                }
            }
            // WHERE / HAVING / 投影中的子查询
            if let Some(expr) = &s.selection {
                collect_subqueries_in_expr(expr, out, depth);
            }
            if let Some(expr) = &s.having {
                collect_subqueries_in_expr(expr, out, depth);
            }
            if let Some(expr) = &s.qualify {
                collect_subqueries_in_expr(expr, out, depth);
            }
            for item in &s.projection {
                match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        collect_subqueries_in_expr(e, out, depth);
                    }
                    _ => {}
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            collect_subqueries_in_set_expr(left, out, depth);
            collect_subqueries_in_set_expr(right, out, depth);
        }
        SetExpr::Query(inner) => {
            out.push(analyze_query(inner));
            collect_subqueries_in_query(inner, out, depth + 1);
        }
        _ => {}
    }
}

pub(crate) fn collect_subqueries_in_set_expr(e: &SetExpr, out: &mut Vec<SelectInfo>, depth: usize) {
    if depth > 20 {
        return;
    }
    match e {
        SetExpr::Select(s) => {
            for twj in &s.from {
                if let TableFactor::Derived { subquery, .. } = &twj.relation {
                    out.push(analyze_query(subquery));
                    collect_subqueries_in_query(subquery, out, depth + 1);
                }
                for j in &twj.joins {
                    if let TableFactor::Derived { subquery, .. } = &j.relation {
                        out.push(analyze_query(subquery));
                        collect_subqueries_in_query(subquery, out, depth + 1);
                    }
                }
            }
            if let Some(expr) = &s.selection {
                collect_subqueries_in_expr(expr, out, depth);
            }
            for item in &s.projection {
                match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        collect_subqueries_in_expr(e, out, depth);
                    }
                    _ => {}
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            collect_subqueries_in_set_expr(left, out, depth + 1);
            collect_subqueries_in_set_expr(right, out, depth + 1);
        }
        SetExpr::Query(inner) => {
            out.push(analyze_query(inner));
            collect_subqueries_in_query(inner, out, depth + 1);
        }
        _ => {}
    }
}

/// 递归遍历表达式，把所有遇到的子查询（Subquery / Exists / InSubquery / ArraySubquery）
/// 推入 out，并继续下钻子查询内部。
pub(crate) fn collect_subqueries_in_expr(e: &Expr, out: &mut Vec<SelectInfo>, depth: usize) {
    if depth > 20 {
        return;
    }
    match e {
        Expr::Subquery(q) => {
            out.push(analyze_query(q));
            collect_subqueries_in_query(q, out, depth + 1);
        }
        Expr::Exists { subquery, .. } => {
            out.push(analyze_query(subquery));
            collect_subqueries_in_query(subquery, out, depth + 1);
        }
        Expr::InSubquery { subquery, .. } => {
            out.push(analyze_query(subquery));
            collect_subqueries_in_query(subquery, out, depth + 1);
        }
        // 递归下钻常见容器
        Expr::BinaryOp { left, right, .. } => {
            collect_subqueries_in_expr(left, out, depth);
            collect_subqueries_in_expr(right, out, depth);
        }
        Expr::UnaryOp { expr, .. } => collect_subqueries_in_expr(expr, out, depth),
        Expr::IsNull(expr) | Expr::IsNotNull(expr) => collect_subqueries_in_expr(expr, out, depth),
        Expr::InList { expr, list, .. } => {
            collect_subqueries_in_expr(expr, out, depth);
            for x in list {
                collect_subqueries_in_expr(x, out, depth);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_subqueries_in_expr(expr, out, depth);
            collect_subqueries_in_expr(low, out, depth);
            collect_subqueries_in_expr(high, out, depth);
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(o) = operand {
                collect_subqueries_in_expr(o, out, depth);
            }
            for c in conditions {
                collect_subqueries_in_expr(&c.condition, out, depth);
                collect_subqueries_in_expr(&c.result, out, depth);
            }
            if let Some(e) = else_result {
                collect_subqueries_in_expr(e, out, depth);
            }
        }
        Expr::Cast { expr, .. } => collect_subqueries_in_expr(expr, out, depth),
        Expr::Function(f) => {
            if let sqlparser::ast::FunctionArguments::List(list) = &f.args {
                for arg in &list.args {
                    let inner = match arg {
                        sqlparser::ast::FunctionArg::Named { arg, .. } => arg,
                        sqlparser::ast::FunctionArg::ExprNamed { arg, .. } => arg,
                        sqlparser::ast::FunctionArg::Unnamed(arg) => arg,
                    };
                    if let sqlparser::ast::FunctionArgExpr::Expr(e) = inner {
                        collect_subqueries_in_expr(e, out, depth);
                    }
                }
            }
        }
        _ => {}
    }
}

/// 在表达式中扫描窗口函数（`Function { over: Some(_), .. }`），把每个窗口函数
/// 的元信息推入 out。会下钻到 BinaryOp / UnaryOp / Case / Cast / Function 等容器。
pub(crate) fn collect_window_funcs_in_expr(e: &Expr, out: &mut Vec<WindowFuncInfo>) {
    match e {
        Expr::Function(f) => {
            if let Some(over) = &f.over {
                let (has_partition_by, has_order_by, has_window_frame) = match over {
                    WindowType::WindowSpec(spec) => (
                        !spec.partition_by.is_empty(),
                        !spec.order_by.is_empty(),
                        spec.window_frame.is_some(),
                    ),
                    WindowType::NamedWindow(_) => (false, false, false),
                };
                let func_name = f.name.0.last().map(|i| i.to_string()).unwrap_or_default();
                out.push(WindowFuncInfo {
                    function_name: func_name,
                    has_partition_by,
                    has_order_by,
                    has_window_frame,
                });
            }
            // 函数参数中可能还嵌套窗口函数，下钻
            if let sqlparser::ast::FunctionArguments::List(list) = &f.args {
                for arg in &list.args {
                    let inner = match arg {
                        sqlparser::ast::FunctionArg::Named { arg, .. } => arg,
                        sqlparser::ast::FunctionArg::ExprNamed { arg, .. } => arg,
                        sqlparser::ast::FunctionArg::Unnamed(arg) => arg,
                    };
                    if let sqlparser::ast::FunctionArgExpr::Expr(inner_e) = inner {
                        collect_window_funcs_in_expr(inner_e, out);
                    }
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_window_funcs_in_expr(left, out);
            collect_window_funcs_in_expr(right, out);
        }
        Expr::UnaryOp { expr, .. } => collect_window_funcs_in_expr(expr, out),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(o) = operand {
                collect_window_funcs_in_expr(o, out);
            }
            for c in conditions {
                collect_window_funcs_in_expr(&c.condition, out);
                collect_window_funcs_in_expr(&c.result, out);
            }
            if let Some(e) = else_result {
                collect_window_funcs_in_expr(e, out);
            }
        }
        Expr::Cast { expr, .. } => collect_window_funcs_in_expr(expr, out),
        Expr::IsNull(expr) | Expr::IsNotNull(expr) => collect_window_funcs_in_expr(expr, out),
        Expr::InList { expr, list, .. } => {
            collect_window_funcs_in_expr(expr, out);
            for x in list {
                collect_window_funcs_in_expr(x, out);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_window_funcs_in_expr(expr, out);
            collect_window_funcs_in_expr(low, out);
            collect_window_funcs_in_expr(high, out);
        }
        _ => {}
    }
}

/// 分析表达式的顶层信息（不递归暴露子表达式）。
pub(crate) fn analyze_expr(e: &Expr) -> ExprInfo {
    let text = e.to_string();
    let (
        kind,
        function_name,
        is_literal,
        is_column,
        is_subquery,
        operator,
        column_name,
        has_null_test,
    ) = match e {
        Expr::Identifier(ident) => (
            "IDENTIFIER",
            String::new(),
            false,
            true,
            false,
            String::new(),
            ident.value.clone(),
            false,
        ),
        Expr::CompoundIdentifier(idents) => (
            "COMPOUND_IDENTIFIER",
            String::new(),
            false,
            true,
            false,
            String::new(),
            idents.last().map(|i| i.value.clone()).unwrap_or_default(),
            false,
        ),
        Expr::Value(_) => (
            "LITERAL",
            String::new(),
            true,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::BinaryOp { op, .. } => (
            "BINARY_OP",
            String::new(),
            false,
            false,
            false,
            format!("{:?}", op).to_uppercase(),
            String::new(),
            false,
        ),
        Expr::UnaryOp { op, .. } => (
            "UNARY_OP",
            String::new(),
            false,
            false,
            false,
            format!("{:?}", op).to_uppercase(),
            String::new(),
            false,
        ),
        Expr::Function(f) => (
            "FUNCTION",
            f.name.0.last().map(|i| i.to_string()).unwrap_or_default(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Case { .. } => (
            "CASE",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Subquery(_) => (
            "SUBQUERY",
            String::new(),
            false,
            false,
            true,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Exists { .. } => (
            "EXISTS",
            String::new(),
            false,
            false,
            true,
            String::new(),
            String::new(),
            false,
        ),
        Expr::InList { .. } => (
            "IN_LIST",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::InSubquery { .. } => (
            "IN_SUBQUERY",
            String::new(),
            false,
            false,
            true,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Between { .. } => (
            "BETWEEN",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Cast { .. } => (
            "CAST",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::IsNull(_) => (
            "IS_NULL",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            true,
        ),
        Expr::IsNotNull(_) => (
            "IS_NOT_NULL",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            true,
        ),
        Expr::TypedString { .. } => (
            "TYPED_STRING",
            String::new(),
            true,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        Expr::Interval(_) => (
            "INTERVAL",
            String::new(),
            true,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
        _ => (
            "OTHER",
            String::new(),
            false,
            false,
            false,
            String::new(),
            String::new(),
            false,
        ),
    };
    ExprInfo {
        kind: kind.to_string(),
        text,
        function_name,
        is_literal,
        is_column,
        is_subquery,
        operator,
        column_name,
        has_null_test,
        line: None,
        column: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    /// 解析 SQL，返回首个 Query（适用于 SELECT / UNION / WITH ... SELECT 等）。
    fn parse_query(sql: &str) -> Query {
        let dialect = GenericDialect {};
        let ast = Parser::parse_sql(&dialect, sql).unwrap();
        match &ast[0] {
            sqlparser::ast::Statement::Query(q) => (**q).clone(),
            _ => panic!("expected a Query statement"),
        }
    }

    /// 取 Query 的 Select body（非集合运算 / 非 Values）。
    fn select_body(q: &Query) -> &sqlparser::ast::Select {
        match &*q.body {
            SetExpr::Select(s) => s,
            _ => panic!("expected a Select body"),
        }
    }

    /// 取首个投影表达式（UnnamedExpr 或 ExprWithAlias 的内部 expr）。
    fn first_proj_expr(q: &Query) -> &Expr {
        let s = select_body(q);
        match &s.projection[0] {
            SelectItem::UnnamedExpr(e) => e,
            SelectItem::ExprWithAlias { expr, .. } => expr,
            _ => panic!("first projection item is not an expression"),
        }
    }

    /// 从任意带约束槽的 JoinOperator 中提取 &JoinConstraint。
    fn join_constraint(op: &JoinOperator) -> &JoinConstraint {
        match op {
            JoinOperator::Join(c)
            | JoinOperator::Inner(c)
            | JoinOperator::Left(c)
            | JoinOperator::LeftOuter(c)
            | JoinOperator::Right(c)
            | JoinOperator::RightOuter(c)
            | JoinOperator::FullOuter(c)
            | JoinOperator::Semi(c)
            | JoinOperator::LeftSemi(c)
            | JoinOperator::RightSemi(c)
            | JoinOperator::Anti(c)
            | JoinOperator::LeftAnti(c)
            | JoinOperator::RightAnti(c)
            | JoinOperator::CrossJoin(c) => c,
            _ => panic!("join operator has no constraint slot"),
        }
    }

    // ===== query_has_wildcard =====

    #[test]
    fn test_query_has_wildcard_select_star() {
        let q = parse_query("SELECT * FROM t");
        assert!(query_has_wildcard(&q));
    }

    #[test]
    fn test_query_has_wildcard_no_star() {
        let q = parse_query("SELECT id FROM t");
        assert!(!query_has_wildcard(&q));
    }

    #[test]
    fn test_query_has_wildcard_nested_subquery() {
        let q = parse_query("SELECT * FROM (SELECT * FROM t) AS sub");
        assert!(query_has_wildcard(&q));
    }

    #[test]
    fn test_query_has_wildcard_union_branch() {
        let q = parse_query("SELECT id FROM t UNION SELECT * FROM t2");
        assert!(query_has_wildcard(&q));
    }

    // ===== analyze_query =====

    #[test]
    fn test_analyze_query_wildcard_and_from_table() {
        let q = parse_query("SELECT * FROM users");
        let info = analyze_query(&q);
        assert!(info.has_wildcard);
        assert_eq!(info.from_table.as_deref(), Some("users"));
    }

    #[test]
    fn test_analyze_query_join_with_condition() {
        let q = parse_query("SELECT id FROM users JOIN orders ON users.id = orders.uid");
        let info = analyze_query(&q);
        assert!(!info.joins.is_empty(), "joins should not be empty");
        assert!(info.joins[0].has_condition);
    }

    // ===== explicit_join_type =====

    #[test]
    fn test_explicit_join_type_bare_join() {
        let q = parse_query("SELECT id FROM a JOIN b ON a.id = b.id");
        let info = analyze_query(&q);
        assert!(!info.joins[0].explicit_join_type, "bare JOIN has no explicit type");
    }

    #[test]
    fn test_explicit_join_type_inner_left() {
        let q = parse_query(
            "SELECT id FROM a INNER JOIN b ON a.id = b.id LEFT JOIN c ON b.id = c.id",
        );
        let info = analyze_query(&q);
        assert!(info.joins[0].explicit_join_type, "INNER JOIN is explicit");
        assert!(info.joins[1].explicit_join_type, "LEFT JOIN is explicit");
    }

    #[test]
    fn test_explicit_join_type_cross() {
        let q = parse_query("SELECT id FROM a CROSS JOIN b");
        let info = analyze_query(&q);
        assert!(info.joins[0].explicit_join_type, "CROSS JOIN is explicit");
    }

    // ===== table_count =====

    #[test]
    fn test_table_count_single_table() {
        let q = parse_query("SELECT id FROM users");
        let info = analyze_query(&q);
        assert_eq!(info.table_count, 1);
    }

    #[test]
    fn test_table_count_with_joins() {
        let q = parse_query(
            "SELECT id FROM a JOIN b ON a.id = b.id JOIN c ON b.id = c.id",
        );
        let info = analyze_query(&q);
        assert_eq!(info.table_count, 3);
    }

    #[test]
    fn test_table_count_comma_join() {
        // 隐式逗号 JOIN：a, b, c 共 3 张表
        let q = parse_query("SELECT a.id, b.id, c.id FROM a, b, c WHERE a.id = b.id");
        let info = analyze_query(&q);
        assert_eq!(info.table_count, 3);
    }

    #[test]
    fn test_table_count_exceeds_five() {
        let q = parse_query(
            "SELECT a.id, b.id, c.id, d.id, e.id, f.id FROM a \
             JOIN b ON a.id = b.id JOIN c ON b.id = c.id JOIN d ON c.id = d.id \
             JOIN e ON d.id = e.id JOIN f ON e.id = f.id",
        );
        let info = analyze_query(&q);
        assert_eq!(info.table_count, 6);
    }

    #[test]
    fn test_analyze_query_where_clause() {
        let q = parse_query("SELECT id FROM users WHERE x = 1");
        let info = analyze_query(&q);
        assert!(info.has_where);
    }

    #[test]
    fn test_analyze_query_group_by() {
        let q = parse_query("SELECT id FROM users GROUP BY id");
        let info = analyze_query(&q);
        assert!(info.has_group_by);
    }

    #[test]
    fn test_analyze_query_order_by_and_limit() {
        let q = parse_query("SELECT id FROM users ORDER BY id LIMIT 10");
        let info = analyze_query(&q);
        assert!(info.has_order_by);
        assert!(info.has_limit);
    }

    #[test]
    fn test_analyze_query_subquery_in_from() {
        let q = parse_query("SELECT id FROM (SELECT * FROM t) AS sub");
        let info = analyze_query(&q);
        assert!(info.has_subquery_in_from);
        assert!(info.has_subquery);
    }

    #[test]
    fn test_analyze_query_cte() {
        let q = parse_query("WITH cte AS (SELECT 1) SELECT * FROM cte");
        let info = analyze_query(&q);
        assert!(info.has_cte);
    }

    #[test]
    fn test_analyze_query_union_distinct() {
        let q = parse_query("SELECT id FROM a UNION SELECT id FROM b");
        let info = analyze_query(&q);
        assert!(info.union);
        assert!(!info.union_all);
    }

    #[test]
    fn test_analyze_query_union_all() {
        let q = parse_query("SELECT id FROM a UNION ALL SELECT id FROM b");
        let info = analyze_query(&q);
        assert!(info.union);
        assert!(info.union_all);
    }

    #[test]
    fn test_analyze_query_comma_join() {
        let q = parse_query("SELECT id FROM a, b");
        let info = analyze_query(&q);
        assert!(info.has_comma_join);
    }

    #[test]
    fn test_analyze_query_window_function() {
        let q = parse_query("SELECT ROW_NUMBER() OVER (ORDER BY id) FROM t");
        let info = analyze_query(&q);
        assert!(info.has_window_function);
    }

    #[test]
    fn test_analyze_query_join_alias() {
        let q = parse_query("SELECT id FROM users u JOIN orders o ON u.id = o.uid");
        let info = analyze_query(&q);
        assert!(!info.joins.is_empty());
        assert_eq!(info.joins[0].alias.as_deref(), Some("o"));
    }

    // ===== analyze_join_operator =====

    #[test]
    fn test_analyze_join_operator_inner_on() {
        let q = parse_query("SELECT id FROM a JOIN b ON a.id = b.id");
        let s = select_body(&q);
        let join = &s.from[0].joins[0];
        let (table, jtype, has_cond, _alias, cond_text) =
            analyze_join_operator(&join.join_operator, &join.relation);
        assert_eq!(table, "b");
        assert_eq!(jtype, "INNER");
        assert!(has_cond);
        assert!(cond_text.is_some());
    }

    #[test]
    fn test_analyze_join_operator_left_on() {
        let q = parse_query("SELECT id FROM a LEFT JOIN b ON a.id = b.id");
        let s = select_body(&q);
        let join = &s.from[0].joins[0];
        let (_, jtype, _, _, _) = analyze_join_operator(&join.join_operator, &join.relation);
        assert_eq!(jtype, "LEFT");
    }

    #[test]
    fn test_analyze_join_operator_cross() {
        let q = parse_query("SELECT id FROM a CROSS JOIN b");
        let s = select_body(&q);
        let join = &s.from[0].joins[0];
        let (_, jtype, has_cond, _, _) = analyze_join_operator(&join.join_operator, &join.relation);
        assert_eq!(jtype, "CROSS");
        assert!(has_cond);
    }

    #[test]
    fn test_analyze_join_operator_using() {
        let q = parse_query("SELECT id FROM a JOIN b USING (id)");
        let s = select_body(&q);
        let join = &s.from[0].joins[0];
        let (_, _, has_cond, _, cond_text) =
            analyze_join_operator(&join.join_operator, &join.relation);
        assert!(has_cond);
        assert!(cond_text.is_none());
    }

    #[test]
    fn test_analyze_join_operator_none_constraint() {
        // None 约束无法通过常规 SQL 解析得到，这里手动构造 JoinOperator。
        let tf_q = parse_query("SELECT 1 FROM b");
        let tf = match &*tf_q.body {
            SetExpr::Select(s) => &s.from[0].relation,
            _ => panic!("expected a Select body"),
        };
        let op = JoinOperator::Inner(JoinConstraint::None);
        let (_, _, has_cond, _, cond_text) = analyze_join_operator(&op, tf);
        assert!(!has_cond);
        assert!(cond_text.is_none());
    }

    // ===== analyze_join_constraint =====

    #[test]
    fn test_analyze_join_constraint_on() {
        let q = parse_query("SELECT id FROM a JOIN b ON a.id = b.id");
        let s = select_body(&q);
        let c = join_constraint(&s.from[0].joins[0].join_operator);
        let (has_cond, text) = analyze_join_constraint(c);
        assert!(has_cond);
        assert!(text.is_some());
        assert_eq!(text.as_deref(), Some("a.id = b.id"));
    }

    #[test]
    fn test_analyze_join_constraint_using() {
        let q = parse_query("SELECT id FROM a JOIN b USING (id)");
        let s = select_body(&q);
        let c = join_constraint(&s.from[0].joins[0].join_operator);
        let (has_cond, text) = analyze_join_constraint(c);
        assert!(has_cond);
        assert!(text.is_none());
    }

    #[test]
    fn test_analyze_join_constraint_natural() {
        let q = parse_query("SELECT id FROM a NATURAL JOIN b");
        let s = select_body(&q);
        let c = join_constraint(&s.from[0].joins[0].join_operator);
        let (has_cond, text) = analyze_join_constraint(c);
        assert!(has_cond);
        assert!(text.is_none());
    }

    #[test]
    fn test_analyze_join_constraint_none() {
        let (has_cond, text) = analyze_join_constraint(&JoinConstraint::None);
        assert!(!has_cond);
        assert!(text.is_none());
    }

    // ===== collect_subqueries_in_query =====

    #[test]
    fn test_collect_subqueries_single_from_subquery() {
        let q = parse_query("SELECT * FROM (SELECT 1) AS sub");
        let mut subs: Vec<SelectInfo> = Vec::new();
        collect_subqueries_in_query(&q, &mut subs, 0);
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn test_collect_subqueries_two_from_subqueries() {
        let q = parse_query("SELECT * FROM (SELECT 1) AS a, (SELECT 2) AS b");
        let mut subs: Vec<SelectInfo> = Vec::new();
        collect_subqueries_in_query(&q, &mut subs, 0);
        assert_eq!(subs.len(), 2);
    }

    #[test]
    fn test_collect_subqueries_in_subquery() {
        let q = parse_query("SELECT id FROM t WHERE x IN (SELECT id FROM t2)");
        let mut subs: Vec<SelectInfo> = Vec::new();
        collect_subqueries_in_query(&q, &mut subs, 0);
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn test_collect_subqueries_exists() {
        let q = parse_query("SELECT id FROM t WHERE EXISTS (SELECT 1 FROM t2)");
        let mut subs: Vec<SelectInfo> = Vec::new();
        collect_subqueries_in_query(&q, &mut subs, 0);
        assert_eq!(subs.len(), 1);
    }

    // ===== collect_window_funcs_in_expr =====

    #[test]
    fn test_collect_window_funcs_order_by_only() {
        let q = parse_query("SELECT ROW_NUMBER() OVER (ORDER BY id) FROM t");
        let expr = first_proj_expr(&q);
        let mut out: Vec<WindowFuncInfo> = Vec::new();
        collect_window_funcs_in_expr(expr, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].function_name, "ROW_NUMBER");
        assert!(out[0].has_order_by);
        assert!(!out[0].has_partition_by);
    }

    #[test]
    fn test_collect_window_funcs_partition_and_order() {
        let q = parse_query("SELECT ROW_NUMBER() OVER (PARTITION BY x ORDER BY id) FROM t");
        let expr = first_proj_expr(&q);
        let mut out: Vec<WindowFuncInfo> = Vec::new();
        collect_window_funcs_in_expr(expr, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].function_name, "ROW_NUMBER");
        assert!(out[0].has_partition_by);
        assert!(out[0].has_order_by);
    }

    // ===== analyze_expr =====

    #[test]
    fn test_analyze_expr_identifier() {
        let q = parse_query("SELECT id FROM t");
        let info = analyze_expr(first_proj_expr(&q));
        assert_eq!(info.kind, "IDENTIFIER");
        assert!(info.is_column);
        assert_eq!(info.column_name, "id");
    }

    #[test]
    fn test_analyze_expr_value_literal() {
        let q = parse_query("SELECT 1 FROM t");
        let info = analyze_expr(first_proj_expr(&q));
        assert_eq!(info.kind, "LITERAL");
        assert!(info.is_literal);
    }

    #[test]
    fn test_analyze_expr_binary_op_plus() {
        let q = parse_query("SELECT a + b FROM t");
        let info = analyze_expr(first_proj_expr(&q));
        assert_eq!(info.kind, "BINARY_OP");
        assert_eq!(info.operator, "PLUS");
    }

    #[test]
    fn test_analyze_expr_function_count() {
        let q = parse_query("SELECT COUNT(*) FROM t");
        let info = analyze_expr(first_proj_expr(&q));
        assert_eq!(info.kind, "FUNCTION");
        assert_eq!(info.function_name, "COUNT");
    }

    #[test]
    fn test_analyze_expr_subquery() {
        let q = parse_query("SELECT (SELECT 1 FROM t) FROM a");
        let info = analyze_expr(first_proj_expr(&q));
        assert_eq!(info.kind, "SUBQUERY");
        assert!(info.is_subquery);
    }

    #[test]
    fn test_analyze_expr_is_null() {
        let q = parse_query("SELECT x IS NULL FROM t");
        let info = analyze_expr(first_proj_expr(&q));
        assert!(info.has_null_test);
    }

    // ---- 限定符抽取（no_unused_join DML101 误判修复）----

    #[test]
    fn test_collect_qualifiers_from_text() {
        let mut out = Vec::new();
        collect_qualifiers_from_text("t1.*", &mut out);
        assert_eq!(out, vec!["T1".to_string()]);

        let mut out = Vec::new();
        collect_qualifiers_from_text("SUM(t2.amt) AS total", &mut out);
        assert_eq!(out, vec!["T2".to_string()]);

        // 三段式：限定符保留 schema，与 JOIN 表名 ofsm.cdeorg 对齐
        let mut out = Vec::new();
        collect_qualifiers_from_text("ofsm.cdeorg.orgno", &mut out);
        assert_eq!(out, vec!["OFSM.CDEORG".to_string()]);

        // 字符串字面量内的点号不算列引用
        let mut out = Vec::new();
        collect_qualifiers_from_text("t1.code = 'a.b'", &mut out);
        assert_eq!(out, vec!["T1".to_string()]);

        // 纯数字字面量不产生限定符
        let mut out = Vec::new();
        collect_qualifiers_from_text("t1.amt > 1.5", &mut out);
        assert_eq!(out, vec!["T1".to_string()]);
    }

    /// 回归用例：SELECT t1.* ... LEFT JOIN ofsm.cdeorg t1
    /// 限定通配符必须进入 projection，且 t1 被视为"已引用"，否则 DML101 误报。
    #[test]
    fn test_qualified_wildcard_is_referenced() {
        let q = parse_query(
            "SELECT t1.* FROM ofsm.cdeusr t2 LEFT JOIN ofsm.cdeorg t1 ON t2.ibkcde = t1.orgno WHERE t2.usr_uid = :usrUid",
        );
        let info = analyze_query(&q);
        assert!(
            info.projection.contains(&"t1.*".to_string()),
            "限定通配符 t1.* 必须进入 projection：{:?}",
            info.projection
        );
        assert!(!info.has_bare_wildcard);
        assert!(info.is_qualifier_referenced("t1"));
        assert!(info.is_qualifier_referenced("T1"));
        assert!(info.is_qualifier_referenced("t2"));
        assert!(!info.is_qualifier_referenced("t3"));
        // JOIN 表名（含/不含 schema）都可匹配
        assert_eq!(info.joins[0].table_name, "ofsm.cdeorg");
        assert_eq!(info.joins[0].alias, Some("t1".to_string()));
        assert_eq!(info.joins[0].table_name_leaf(), "cdeorg");
    }

    /// 裸 SELECT *：所有表都被引用，规则应跳过（has_bare_wildcard = true）。
    #[test]
    fn test_bare_wildcard_flag() {
        let q = parse_query("SELECT * FROM users u JOIN orders o ON u.id = o.user_id");
        let info = analyze_query(&q);
        assert!(info.has_bare_wildcard);
        assert!(info.projection.contains(&"*".to_string()));
    }

    /// ORDER BY / GROUP BY / HAVING 中的列引用也算"使用"该表。
    #[test]
    fn test_order_by_and_group_by_qualifiers() {
        let q = parse_query(
            "SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id ORDER BY o.created_at",
        );
        let info = analyze_query(&q);
        assert!(
            info.is_qualifier_referenced("o"),
            "ORDER BY o.created_at 应视为引用了 o：{:?}",
            info.referenced_qualifiers
        );

        let q = parse_query(
            "SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id GROUP BY u.id HAVING COUNT(o.id) > 1",
        );
        let info = analyze_query(&q);
        assert!(info.is_qualifier_referenced("o"));
        assert!(info.is_qualifier_referenced("u"));
    }

    /// JOIN 的 ON 条件不算引用：右表只在 ON 中出现时仍应判为未使用。
    #[test]
    fn test_on_condition_is_not_a_reference() {
        let q = parse_query("SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id");
        let info = analyze_query(&q);
        assert!(info.is_qualifier_referenced("u"));
        assert!(
            !info.is_qualifier_referenced("o"),
            "o 只出现在 ON 条件，不应算被引用：{:?}",
            info.referenced_qualifiers
        );
    }
}
