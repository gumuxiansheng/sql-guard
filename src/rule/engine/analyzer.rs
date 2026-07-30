use sqlparser::ast::{
    Expr, GroupByExpr, JoinConstraint, JoinOperator, LimitClause, Query, SelectItem,
    SetExpr, SetOperator, SetQuantifier, TableFactor, TableWithJoins, WindowType,
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
                matches!(item, SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _))
            });
            let derived_wc = |tf: &TableWithJoins| -> bool {
                let rel_wc =
                    matches!(&tf.relation, TableFactor::Derived { subquery, .. } if query_has_wildcard(subquery));
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
    info.has_offset = q.limit_clause.as_ref().map_or(false, |l| match l {
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
                    info.projection_exprs.push(ExprInfo {
                        kind: "WILDCARD".to_string(),
                        text: "*".to_string(),
                        ..Default::default()
                    });
                }
                SelectItem::QualifiedWildcard(obj, _) => {
                    info.has_wildcard = true;
                    info.projection_exprs.push(ExprInfo {
                        kind: "WILDCARD".to_string(),
                        text: obj.to_string() + ".*",
                        ..Default::default()
                    });
                }
                SelectItem::UnnamedExpr(expr) => {
                    let text = expr.to_string();
                    info.projection.push(text.clone());
                    if has_multiple_tables && !text.contains('.') {
                        if matches!(expr, Expr::Identifier(_)) {
                            info.has_unqualified_column = true;
                        }
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
        for table_with_joins in &s.from {
            // 分析主表
            match &table_with_joins.relation {
                TableFactor::Table { name, alias, .. } => {
                    if info.from_table.is_none() {
                        info.from_table = Some(name.to_string());
                    }
                    if info.from_table_alias.is_none() {
                        info.from_table_alias =
                            alias.as_ref().map(|a| a.name.to_string());
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

    // 子查询递归收集（FROM 子查询 / WHERE 子查询 / EXISTS / 集合运算的括号分支）
    let mut subs = Vec::new();
    collect_subqueries_in_query(q, &mut subs, 0);
    if !subs.is_empty() {
        info.has_subquery = true;
        info.subqueries = subs;
    }

    info
}

/// 分析一个 JOIN 的操作符与目标表，返回
/// (table_name, join_type, has_condition, alias, condition_text)。
pub(crate) fn analyze_join_operator(
    op: &JoinOperator,
    relation: &TableFactor,
) -> (String, String, bool, Option<String>, Option<String>) {
    let (table_name, alias) = table_factor_name_and_alias(relation);
    match op {
        JoinOperator::Inner(constraint)
        | JoinOperator::Join(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint) => {
            let join_type = match op {
                JoinOperator::Inner(_) | JoinOperator::Join(_) => "INNER",
                JoinOperator::LeftOuter(_) | JoinOperator::Left(_) => "LEFT",
                JoinOperator::RightOuter(_) | JoinOperator::Right(_) => "RIGHT",
                JoinOperator::FullOuter(_) => "FULL",
                JoinOperator::LeftSemi(_) => "LEFT_SEMI",
                JoinOperator::RightSemi(_) => "RIGHT_SEMI",
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

/// 从 TableFactor 提取表名（或派生表的字符串形式）与别名。
pub(crate) fn table_factor_name_and_alias(tf: &TableFactor) -> (String, Option<String>) {
    match tf {
        TableFactor::Table { name, alias, .. } => (
            name.to_string(),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::Derived { alias, subquery, .. } => (
            format!("({})", subquery),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::TableFunction { expr, alias } => (
            format!("TABLE({})", expr),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
        TableFactor::Function { name, alias, .. } => (
            name.to_string(),
            alias.as_ref().map(|a| a.name.to_string()),
        ),
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
            out.push(analyze_query(&*q));
            collect_subqueries_in_query(&*q, out, depth + 1);
        }
        Expr::Exists { subquery, .. } => {
            out.push(analyze_query(&*subquery));
            collect_subqueries_in_query(&*subquery, out, depth + 1);
        }
        Expr::InSubquery { subquery, .. } => {
            out.push(analyze_query(&*subquery));
            collect_subqueries_in_query(&*subquery, out, depth + 1);
        }
        // 递归下钻常见容器
        Expr::BinaryOp { left, right, .. } => {
            collect_subqueries_in_expr(left, out, depth);
            collect_subqueries_in_expr(right, out, depth);
        }
        Expr::UnaryOp { expr, .. } => collect_subqueries_in_expr(expr, out, depth),
        Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            collect_subqueries_in_expr(expr, out, depth)
        }
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
                let func_name = f
                    .name
                    .0
                    .last()
                    .map(|i| i.to_string())
                    .unwrap_or_default();
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
    let (kind, function_name, is_literal, is_column, is_subquery, operator, column_name, has_null_test) = match e {
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
        Expr::Value(_) => ("LITERAL", String::new(), true, false, false, String::new(), String::new(), false),
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
        Expr::Case { .. } => ("CASE", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::Subquery(_) => ("SUBQUERY", String::new(), false, false, true, String::new(), String::new(), false),
        Expr::Exists { .. } => ("EXISTS", String::new(), false, false, true, String::new(), String::new(), false),
        Expr::InList { .. } => ("IN_LIST", String::new(), false, false, false, String::new(), String::new(), false),
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
        Expr::Between { .. } => ("BETWEEN", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::Cast { .. } => ("CAST", String::new(), false, false, false, String::new(), String::new(), false),
        Expr::IsNull(_) => ("IS_NULL", String::new(), false, false, false, String::new(), String::new(), true),
        Expr::IsNotNull(_) => ("IS_NOT_NULL", String::new(), false, false, false, String::new(), String::new(), true),
        Expr::TypedString { .. } => ("TYPED_STRING", String::new(), true, false, false, String::new(), String::new(), false),
        Expr::Interval(_) => ("INTERVAL", String::new(), true, false, false, String::new(), String::new(), false),
        _ => ("OTHER", String::new(), false, false, false, String::new(), String::new(), false),
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
