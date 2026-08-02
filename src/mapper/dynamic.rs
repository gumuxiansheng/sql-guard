//! MyBatis 动态 SQL 保留结构解析与变体展开。
//!
//! 与 [`crate::mapper::parser`] 的"剥离所有动态标签、仅保留文本"语义不同，
//! 本模块把 `<select>/<insert>/<update>/<delete>` 内部解析为 [`DynNode`] 树，
//! 保留 `<if>`/`<choose>`/`<foreach>`/`<where>`/`<set>`/`<trim>` 的分支结构，
//! 再按用户配置的策略展开为多条具体 SQL 变体，供 `replay-export` 导出。
//!
//! ## 展开策略
//! - **全组合 + 阈值降级**（默认）：统计独立 `<if>` 数 N，N ≤ `max_independent_ifs`
//!   (默认 8) 时全组合 2^N；超过则降级为"单分支激活"（基线 + 每个 if 单独 true，共 N+1）。
//! - **`<choose>`**：按 when 分支数展开，有 `<otherwise>` 再加 1 条。
//! - **`<foreach>`**：固定生成 3 种变体（空集合 / 1 元素 / 3 元素）。
//! - **`<where>`/`<set>`/`<trim>`**：容器，不增变体数，按 MyBatis 规则处理前后缀。
//!
//! ## 变体标识
//! 每个变体带 `label`，如 `if:name!=null=true,choose:when0,foreach:1elem`。

use std::collections::{HashMap, HashSet};

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::SqlGuardError;
use crate::mapper::placeholder::normalize_placeholders;

/// 四类 SQL 语句标签。
const SQL_TAGS: &[&str] = &["select", "insert", "update", "delete"];

/// 默认独立 if 阈值：N ≤ 8 时全组合（最多 256 变体），超过降级为单分支激活。
pub const DEFAULT_MAX_INDEPENDENT_IFS: usize = 8;

/// 一条语句解析后的动态 SQL 节点树 + 元信息。
#[derive(Debug, Clone)]
pub struct DynamicStatement {
    pub statement_id: String,
    pub statement_type: String,
    pub raw_xml_line: usize,
    pub root_nodes: Vec<DynNode>,
}

/// 动态 SQL 节点（保留分支结构）。
#[derive(Debug, Clone)]
pub enum DynNode {
    /// 纯文本（含占位符原文）。
    Text(String),
    /// `<if test="...">...</if>`
    If {
        test: String,
        children: Vec<DynNode>,
    },
    /// `<choose><when>...</when>...<otherwise>...</otherwise></choose>`
    Choose {
        when_clauses: Vec<(String, Vec<DynNode>)>,
        otherwise: Option<Vec<DynNode>>,
    },
    /// `<foreach collection="..." item="..." open="(" close=")" separator=",">...</foreach>`
    ForEach {
        open: String,
        close: String,
        separator: String,
        children: Vec<DynNode>,
    },
    /// `<where>...</where>`：内部非空时插 WHERE 并去首 AND/OR。
    Where(Vec<DynNode>),
    /// `<set>...</set>`：内部非空时插 SET 并去尾逗号。
    Set(Vec<DynNode>),
    /// `<trim prefix="" suffix="" prefixOverrides="" suffixOverrides="">...</trim>`
    Trim {
        prefix: String,
        suffix: String,
        prefix_overrides: String,
        suffix_overrides: String,
        children: Vec<DynNode>,
    },
    /// `<bind name="..." value="..."/>`：变量绑定，不影响 SQL 结构，展开时忽略。
    Bind,
}

/// 展开后的一个变体。
#[derive(Debug, Clone)]
pub struct Variant {
    /// 扁平化 + 占位符标准化后的 SQL 文本。
    pub sql: String,
    /// 分支组合描述，如 `if:name!=null=true,foreach:1elem`。
    pub label: String,
}

/// 从 Mapper XML 解析所有语句为动态节点树。
///
/// 与 [`crate::mapper::parser::extract_sql_from_xml`] 的区别：保留动态标签结构，
/// 不剥离；`<include>` 在解析阶段内联展开（同文件内，按 refid 完整匹配）。
pub fn parse_dynamic_statements(
    xml_path: &std::path::Path,
) -> Result<Vec<DynamicStatement>, SqlGuardError> {
    let content = std::fs::read_to_string(xml_path).map_err(|e| {
        SqlGuardError::MapperError(format!(
            "Failed to read mapper XML '{}': {}",
            xml_path.display(),
            e
        ))
    })?;

    // 第一遍：收集 <sql id="..."> 片段为 DynNode 树
    let fragments = collect_sql_fragments_dynamic(&content)?;

    // 第二遍：解析 <select>/<insert>/<update>/<delete> 为 DynNode 树
    parse_statements_dynamic(&content, &fragments)
}

/// 从 XML 文本解析动态语句（可选外部片段表，支持跨 namespace include）。
///
/// [`parse_dynamic_statements`] 是本函数「读文件 + 无外部片段」的封装。
pub fn parse_dynamic_statements_from_content(
    content: &str,
    global: Option<&HashMap<String, Vec<DynNode>>>,
) -> Result<Vec<DynamicStatement>, SqlGuardError> {
    let mut fragments = collect_sql_fragments_dynamic(content)?;
    if let Some(g) = global {
        // 本地片段优先：仅补充本地没有的 key（跨 namespace 的 `ns.id` 形式不会冲突）
        for (k, v) in g {
            fragments.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    parse_statements_dynamic(content, &fragments)
}

/// 收集单个 XML 文本中的 `<sql id="...">` 片段树（供跨文件全局片段表构建）。
pub fn collect_fragments_from_content(
    content: &str,
) -> Result<HashMap<String, Vec<DynNode>>, SqlGuardError> {
    collect_sql_fragments_dynamic(content)
}

/// 单变体渲染模式：把动态节点树压成**一条**可解析的代表性 SQL，供静态规则检查使用。
///
/// 与 [`expand_variants`]（重放导出用，穷举所有分支）互补：静态检查只需要一条
/// 语法合法、且尽量覆盖全部列/条件的代表 SQL。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// 所有 `<if>` 取真——覆盖面最大，是默认首选。
    AllTrue,
    /// 嵌套在另一个 `<if>` 内部的**兄弟** `<if>` 只取第一个。
    ///
    /// 对付「外层 if 提供 `and`，内层多个 if 互斥地提供操作数」的写法：
    /// ```xml
    /// <if test="orgno != null"> and
    ///   <if test="c != '1'"> n.SUPORGNO = #{orgno} </if>
    ///   <if test="c != '0'"> n.ORGNO   = #{orgno} </if>
    /// </if>
    /// ```
    /// AllTrue 会拼出 `and n.SUPORGNO = ? n.ORGNO = ?`（语法错误），
    /// 本模式只保留第一个内层分支。
    ExclusiveNested,
    /// 每个同层（含嵌套）的兄弟 `<if>` 组只取第一个渲染。
    ///
    /// 对付「顶层/嵌套的**互斥** `<if>` 被 AllTrue 全拼导致条件间缺 AND」的写法，
    /// 例如 `ibkcdeflg == 0` 与 `ibkcdeflg == 1`、`orgLv == 0` 与 `orgLv != '0'`
    /// 在运行时互斥（同一参数只能取一个值），全取真会拼成
    /// `where o.orglev in (...) o.orgno = ?`（缺 AND）→ 解析失败。
    /// 作为兜底候选，只要能解析即可，牺牲部分覆盖面换取"至少能解析"。
    /// 与 [`RenderMode::ExclusiveNested`] 的区别：后者只在「嵌套于另一个 `<if>` 内」
    /// 时折叠，本模式在**所有层级**折叠同层兄弟 `<if>`。
    FirstBranch,
}

/// 按 `mode` 把节点树渲染成单条 SQL（未做占位符标准化）。
///
/// 语义要点：
/// - `<where>` / `<set>` / `<trim>`：按 MyBatis 规则补关键字、去多余 AND/OR 与逗号
/// - `<choose>`：只取第一个 `<when>`（没有 when 则取 `<otherwise>`）——MyBatis 运行时
///   本就只会命中一个分支，全拼是错的
/// - `<foreach>`：按「集合有 1 个元素」渲染，即 `open + 单份 body + close`，
///   这样 `IN <foreach open="(" close=")">#{i}</foreach>` 才能得到合法的 `IN (?)`
/// - `<bind>`：忽略
pub fn render_canonical(nodes: &[DynNode], mode: RenderMode) -> String {
    render_seq_canonical(nodes, mode, false)
}

fn render_seq_canonical(nodes: &[DynNode], mode: RenderMode, inside_if: bool) -> String {
    let mut out = String::new();
    // ExclusiveNested 模式下，同一层级中嵌套于 <if> 内的兄弟 <if> 只保留第一个
    let mut nested_if_taken = false;
    // FirstBranch 模式下，每个同层兄弟 <if>` 连续段只保留第一个；
    // 遇到非 <if> 节点（文本/标签）即重置，使独立的 <if> 段各自成组。
    let mut first_if_in_run = true;
    for node in nodes {
        match node {
            DynNode::Text(s) => {
                out.push_str(s);
                // 仅当文本含非空白内容时才重置「兄弟 if 段」——标签间的空白
                // （换行/缩进）不能算作段边界，否则同层互斥 <if> 之间因空白被拆开，
                // 折叠失效。
                if !s.trim().is_empty() {
                    first_if_in_run = true;
                }
            }
            DynNode::Bind => {
                first_if_in_run = true;
            }
            DynNode::If { children, .. } => {
                if mode == RenderMode::ExclusiveNested && inside_if {
                    if nested_if_taken {
                        continue;
                    }
                    nested_if_taken = true;
                }
                if mode == RenderMode::FirstBranch && !first_if_in_run {
                    continue;
                }
                first_if_in_run = false;
                out.push_str(&render_seq_canonical(children, mode, true));
            }
            DynNode::Choose {
                when_clauses,
                otherwise,
            } => {
                first_if_in_run = true;
                if let Some((_, ch)) = when_clauses.first() {
                    out.push_str(&render_seq_canonical(ch, mode, inside_if));
                } else if let Some(oth) = otherwise {
                    out.push_str(&render_seq_canonical(oth, mode, inside_if));
                }
            }
            DynNode::ForEach {
                open,
                close,
                children,
                ..
            } => {
                first_if_in_run = true;
                let body = render_seq_canonical(children, mode, inside_if);
                if body.trim().is_empty() {
                    continue;
                }
                out.push_str(open);
                out.push_str(&body);
                out.push_str(close);
            }
            DynNode::Where(ch) => {
                first_if_in_run = true;
                out.push(' ');
                out.push_str(&process_where(&render_seq_canonical(ch, mode, inside_if)));
                out.push(' ');
            }
            DynNode::Set(ch) => {
                first_if_in_run = true;
                out.push(' ');
                out.push_str(&process_set(&render_seq_canonical(ch, mode, inside_if)));
                out.push(' ');
            }
            DynNode::Trim {
                prefix,
                suffix,
                prefix_overrides,
                suffix_overrides,
                children,
            } => {
                first_if_in_run = true;
                let inner = render_seq_canonical(children, mode, inside_if);
                out.push(' ');
                out.push_str(&process_trim(
                    &inner,
                    prefix,
                    suffix,
                    prefix_overrides,
                    suffix_overrides,
                ));
                out.push(' ');
            }
        }
    }
    out
}

/// 把一条语句的节点树展开为多个变体。
///
/// `max_independent_ifs` 控制全组合阈值；超过则降级为单分支激活。
pub fn expand_variants(stmt: &DynamicStatement, max_independent_ifs: usize) -> Vec<Variant> {
    let if_count = count_independent_ifs(&stmt.root_nodes);
    let policy = if if_count <= max_independent_ifs {
        Policy::Full
    } else {
        Policy::SingleBranch { active: None } // 基线，由调用方循环激活
    };

    let mut all = Vec::new();
    if matches!(policy, Policy::Full) {
        let mut ctx = ExpansionCtx {
            policy,
            if_counter: 0,
        };
        all.extend(expand_seq(&stmt.root_nodes, &mut ctx));
    } else {
        // 单分支激活：基线（全 false）+ 每个 if 单独 true
        // 基线
        let mut ctx = ExpansionCtx {
            policy: Policy::SingleBranch { active: None },
            if_counter: 0,
        };
        all.extend(expand_seq(&stmt.root_nodes, &mut ctx));
        // 每个 if 单独 true
        for k in 0..if_count {
            let mut ctx = ExpansionCtx {
                policy: Policy::SingleBranch { active: Some(k) },
                if_counter: 0,
            };
            all.extend(expand_seq(&stmt.root_nodes, &mut ctx));
        }
    }
    // 占位符标准化（#{...} → ?, ${...} → _var_xxx），与静态检查侧保持一致
    for v in &mut all {
        v.sql = normalize_placeholders(&v.sql);
    }
    dedup_variants(all)
}

/// 判断一条动态语句是否包含 MyBatis `${}` 文本替换（运行时才确定内容，静态期不可解析）。
///
/// 用于 replay-export 的信任跳过：含 `${}` 的语句渲染后若因运行时片段残缺而解析失败，
/// 属于预期内，不应在清单里标记为普通「解析错误」。实现上直接扫描解析后的节点树文本——
/// `${}` 在 `DynNode::Text` 中保持原文，未被 `normalize_placeholders` 改写。
pub fn contains_dollar_substitution(stmt: &DynamicStatement) -> bool {
    fn walk(nodes: &[DynNode]) -> bool {
        for n in nodes {
            match n {
                DynNode::Text(s) => {
                    if s.contains("${") {
                        return true;
                    }
                }
                DynNode::If { children, .. } => {
                    if walk(children) {
                        return true;
                    }
                }
                DynNode::Choose {
                    when_clauses,
                    otherwise,
                } => {
                    for (_, ch) in when_clauses {
                        if walk(ch) {
                            return true;
                        }
                    }
                    if let Some(o) = otherwise {
                        if walk(o) {
                            return true;
                        }
                    }
                }
                DynNode::ForEach { children, .. }
                | DynNode::Where(children)
                | DynNode::Set(children)
                | DynNode::Trim { children, .. } => {
                    if walk(children) {
                        return true;
                    }
                }
                DynNode::Bind => {}
            }
        }
        false
    }
    walk(&stmt.root_nodes)
}

/// 按「空白归一化 + 忽略大小写」后的 SQL 去重，保留首次出现的变体（含其 label）。
///
/// 不同分支组合经常渲染出**字面完全相同**的 SQL（例如分支体本身为空、
/// 或互斥分支被同一条件覆盖）。重放侧对同一条 SQL 反复取执行计划纯属浪费，
/// 也会让使用者误以为「多个分支都是同一个语句」。
fn dedup_variants(variants: Vec<Variant>) -> Vec<Variant> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(variants.len());
    for v in variants {
        let key = v
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if key.is_empty() || seen.insert(key) {
            out.push(v);
        }
    }
    out
}

// ==================== 内部实现 ====================

#[derive(Clone, Copy)]
enum Policy {
    /// 全组合：每个 if 生成 true + false 两个变体，笛卡尔积。
    Full,
    /// 单分支激活：active=None 时所有 if=false；active=Some(k) 时第 k 个 if=true，其余=false。
    SingleBranch { active: Option<usize> },
}

struct ExpansionCtx {
    policy: Policy,
    if_counter: usize,
}

/// 统计独立 if 数（包括嵌套在 where/set/trim/foreach 内的；choose 内的 when/otherwise
/// 递归统计，因为它们也可能含 if）。
fn count_independent_ifs(nodes: &[DynNode]) -> usize {
    let mut n = 0;
    for node in nodes {
        match node {
            DynNode::If { children, .. } => {
                n += 1;
                n += count_independent_ifs(children);
            }
            DynNode::Choose {
                when_clauses,
                otherwise,
            } => {
                for (_, ch) in when_clauses {
                    n += count_independent_ifs(ch);
                }
                if let Some(oth) = otherwise {
                    n += count_independent_ifs(oth);
                }
            }
            DynNode::Where(ch) | DynNode::Set(ch) => n += count_independent_ifs(ch),
            DynNode::Trim { children, .. } | DynNode::ForEach { children, .. } => {
                n += count_independent_ifs(children);
            }
            _ => {}
        }
    }
    n
}

/// 展开节点序列为变体列表（笛卡尔积）。
fn expand_seq(nodes: &[DynNode], ctx: &mut ExpansionCtx) -> Vec<Variant> {
    let mut acc = vec![Variant {
        sql: String::new(),
        label: String::new(),
    }];
    for node in nodes {
        let node_variants = expand_node(node, ctx);
        let mut next = Vec::with_capacity(acc.len() * node_variants.len().max(1));
        for a in &acc {
            for v in &node_variants {
                let mut sql = a.sql.clone();
                sql.push_str(&v.sql);
                let mut label = a.label.clone();
                if !label.is_empty() && !v.label.is_empty() {
                    label.push(',');
                }
                label.push_str(&v.label);
                next.push(Variant { sql, label });
            }
        }
        acc = next;
    }
    acc
}

fn expand_node(node: &DynNode, ctx: &mut ExpansionCtx) -> Vec<Variant> {
    match node {
        DynNode::Text(s) => vec![Variant {
            sql: s.clone(),
            label: String::new(),
        }],
        DynNode::Bind => vec![Variant {
            sql: String::new(),
            label: String::new(),
        }],

        DynNode::If { test, children } => {
            let this_index = ctx.if_counter;
            // 本 if 在 DFS 序中占用的索引区间大小：自身 1 个 + 子树内的 if 数。
            // 用于「关闭分支时手动推进计数器」，保证同一个 if 在不同 active
            // 取值下拿到**稳定**的索引（否则嵌套 if 永远激活不到 → 变体全重复）。
            let subtree_span = 1 + count_independent_ifs(children);
            ctx.if_counter += 1;
            let test_label = sanitize_test(test);

            match ctx.policy {
                Policy::Full => {
                    // true 变体：展开 children
                    let true_variants = expand_seq(children, ctx);
                    let mut out = Vec::with_capacity(true_variants.len() + 1);
                    for mut v in true_variants {
                        let mut label = format!("if:{}=true", test_label);
                        if !v.label.is_empty() {
                            label.push(',');
                            label.push_str(&v.label);
                        }
                        v.label = label;
                        out.push(v);
                    }
                    // false 变体：空
                    out.push(Variant {
                        sql: String::new(),
                        label: format!("if:{}=false", test_label),
                    });
                    out
                }
                Policy::SingleBranch { active } => {
                    // 目标索引落在本 if 的**子树区间**内 → 本 if 必须为 true，
                    // 这样嵌套 if 才可能被激活到（否则祖先关闭，内层永远渲染不出来，
                    // 每次都退化成基线 SQL，表现为「多个分支都是同一个语句」）。
                    let hit = matches!(active, Some(k)
                        if k >= this_index && k < this_index + subtree_span);
                    if hit {
                        // 该 if 激活：true
                        let mut out = Vec::new();
                        for mut v in expand_seq(children, ctx) {
                            let mut label = format!("if:{}=true", test_label);
                            if !v.label.is_empty() {
                                label.push(',');
                                label.push_str(&v.label);
                            }
                            v.label = label;
                            out.push(v);
                        }
                        out
                    } else {
                        // 该 if 关闭：子树不展开，需手动推进计数器跳过子树内的 if，
                        // 否则后续兄弟节点的 if 索引会整体前移，导致索引与
                        // count_independent_ifs 的 DFS 编号错位。
                        ctx.if_counter += subtree_span - 1;
                        vec![Variant {
                            sql: String::new(),
                            label: format!("if:{}=false", test_label),
                        }]
                    }
                }
            }
        }

        DynNode::Choose {
            when_clauses,
            otherwise,
        } => {
            let mut out = Vec::new();
            for (i, (test, children)) in when_clauses.iter().enumerate() {
                let test_label = sanitize_test(test);
                for mut v in expand_seq(children, ctx) {
                    let mut label = format!("choose:when{}({})", i, test_label);
                    if !v.label.is_empty() {
                        label.push(',');
                        label.push_str(&v.label);
                    }
                    v.label = label;
                    out.push(v);
                }
            }
            if let Some(oth) = otherwise {
                for mut v in expand_seq(oth, ctx) {
                    let mut label = "choose:otherwise".to_string();
                    if !v.label.is_empty() {
                        label.push(',');
                        label.push_str(&v.label);
                    }
                    v.label = label;
                    out.push(v);
                }
            }
            out
        }

        DynNode::ForEach {
            open,
            close,
            separator,
            children,
        } => {
            let inner_variants = expand_seq(children, ctx);
            vec![
                for_each_variant(open, close, separator, &inner_variants, 0, "foreach:0elem"),
                for_each_variant(open, close, separator, &inner_variants, 1, "foreach:1elem"),
                for_each_variant(open, close, separator, &inner_variants, 3, "foreach:3elem"),
            ]
        }

        DynNode::Where(children) => {
            let inner = expand_seq(children, ctx);
            inner
                .into_iter()
                .map(|mut v| {
                    v.sql = process_where(&v.sql);
                    v
                })
                .collect()
        }

        DynNode::Set(children) => {
            let inner = expand_seq(children, ctx);
            inner
                .into_iter()
                .map(|mut v| {
                    v.sql = process_set(&v.sql);
                    v
                })
                .collect()
        }

        DynNode::Trim {
            prefix,
            suffix,
            prefix_overrides,
            suffix_overrides,
            children,
        } => {
            let inner = expand_seq(children, ctx);
            inner
                .into_iter()
                .map(|mut v| {
                    v.sql =
                        process_trim(&v.sql, prefix, suffix, prefix_overrides, suffix_overrides);
                    v
                })
                .collect()
        }
    }
}

/// 生成 foreach 的单个变体。
///
/// `count=0`：空集合，open/close 也不输出（符合 MyBatis 行为）。
/// `count=n`：open + (inner separator-joined n 次) + close。
fn for_each_variant(
    open: &str,
    close: &str,
    separator: &str,
    inner_variants: &[Variant],
    count: usize,
    label: &str,
) -> Variant {
    if count == 0 || inner_variants.is_empty() {
        return Variant {
            sql: String::new(),
            label: label.to_string(),
        };
    }
    // 取第一个内部变体作为模板（foreach 内部通常是单条文本）
    let template = &inner_variants[0].sql;
    let mut sql = String::new();
    sql.push_str(open);
    for i in 0..count {
        if i > 0 {
            sql.push_str(separator);
        }
        sql.push_str(template);
    }
    sql.push_str(close);
    Variant {
        sql,
        label: label.to_string(),
    }
}

/// `<where>` 处理：内部文本 trim 后非空则插 `WHERE `，并去除首个 AND/OR。
fn process_where(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let after = trim_leading_and_or(trimmed);
    format!("WHERE {}", after)
}

/// `<set>` 处理：内部文本 trim 后非空则插 `SET `，并去除尾部逗号。
fn process_set(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let after = trimmed.trim_end_matches(',').trim_end();
    format!("SET {}", after)
}

/// `<trim>` 处理：按 prefixOverrides 去前缀、suffixOverrides 去后缀，再补 prefix/suffix。
fn process_trim(
    text: &str,
    prefix: &str,
    suffix: &str,
    prefix_overrides: &str,
    suffix_overrides: &str,
) -> String {
    let mut s = text.trim().to_string();
    // ★ MyBatis 语义：内容为空时整个 <trim> 不输出任何东西（prefix 也不输出）。
    // 否则 `<trim prefix="set">` 空体会渲染出 `UPDATE t set  WHERE ...` 这种语法错误。
    if s.is_empty() {
        return String::new();
    }
    if !prefix_overrides.is_empty() {
        for ov in prefix_overrides.split('|') {
            if ov.trim().is_empty() {
                continue;
            }
            if let Some(rest) = strip_token_prefix(s.trim_start(), ov) {
                s = rest.to_string();
                break;
            }
        }
    }
    if !suffix_overrides.is_empty() {
        for ov in suffix_overrides.split('|') {
            if ov.trim().is_empty() {
                continue;
            }
            if let Some(rest) = strip_token_suffix(s.trim_end(), ov) {
                s = rest.to_string();
                break;
            }
        }
    }
    let s = s.trim();
    // 去掉 override 后可能变空，同样不输出 prefix/suffix
    if s.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !prefix.is_empty() {
        out.push_str(prefix);
        out.push(' ');
    }
    out.push_str(s);
    if !suffix.is_empty() {
        out.push(' ');
        out.push_str(suffix);
    }
    out
}

/// 按 override 记号剥离前缀，大小写不敏感。
///
/// override 若以空白结尾（如 `"and "`），则允许匹配**任意空白**——与 MyBatis
/// `WhereSqlNode` 内置的 `"AND "/"AND\n"/"AND\r"/"AND\t"` 列表等价。
fn strip_token_prefix<'a>(s: &'a str, ov: &str) -> Option<&'a str> {
    let ov_trimmed = ov.trim();
    let head = s.get(..ov_trimmed.len())?;
    if !head.eq_ignore_ascii_case(ov_trimmed) {
        return None;
    }
    let rest = &s[ov_trimmed.len()..];
    let needs_boundary = ov.ends_with(char::is_whitespace)
        || ov_trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if needs_boundary && !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim_start())
}

/// 按 override 记号剥离后缀，大小写不敏感。
fn strip_token_suffix<'a>(s: &'a str, ov: &str) -> Option<&'a str> {
    let ov_trimmed = ov.trim();
    if s.len() < ov_trimmed.len() {
        return None;
    }
    let tail = s.get(s.len() - ov_trimmed.len()..)?;
    if !tail.eq_ignore_ascii_case(ov_trimmed) {
        return None;
    }
    Some(s[..s.len() - ov_trimmed.len()].trim_end())
}

/// 去除开头的 AND / OR（含其后空白），大小写不敏感。
///
/// ★ 关键：关键字后可以是**任意空白**（空格 / 换行 / Tab），不能只认空格。
/// 真实 mapper 中 `<if>` 体常写成独立一行的 `and`：
/// ```xml
/// <if test="x != null">
/// and
///     X = #{x}
/// </if>
/// ```
/// 旧实现要求 `"AND "` 严格跟空格，导致 `and\n` 无法剥离，
/// 渲染出 `WHERE and X = ?` 这种语法错误（本仓库样例中占解析失败的 82%）。
fn trim_leading_and_or(s: &str) -> &str {
    for kw in ["AND", "OR"] {
        // 用 get 避免多字节字符边界 panic
        if let Some(head) = s.get(..kw.len()) {
            if head.eq_ignore_ascii_case(kw) {
                let rest = &s[kw.len()..];
                if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    return rest.trim_start();
                }
            }
        }
    }
    s
}

/// 把 `<if test="...">` 的 test 表达式简化为 label 友好的字符串
/// （去空白、截断到 40 字符）。
fn sanitize_test(test: &str) -> String {
    let cleaned: String = test.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() <= 40 {
        cleaned
    } else {
        cleaned.chars().take(40).collect()
    }
}

/// 修去动态渲染产生的、**在合法 SQL 中绝不可能出现**的标点产物。
///
/// 只动"不可能有效"的标点，绝不改写语义，因此不会掩盖真实规则违规：
/// - `(,` → `(` ：列/值列表里绝不可能紧跟左括号后就是逗号
/// - `,)` → `)` ：绝不可能紧跟右括号前就是逗号（如 `IN (?, ,)`）
/// - `SET ,` → `SET ` ：逗号前置（comma-first）写法被 `<set>`/`<trim>` 保留下来的首逗号
/// - `, FROM` / `, WHERE` / `, GROUP` / `, ORDER` / `, HAVING` / `, LIMIT` /
///   `, UNION` / `, VALUES` → 删逗号：SELECT/SET 列表尾逗号残留
///
/// 这些产物来自 MyBatis 逗号前置风格与 `<set>`/`<trim>` 只去尾逗号不去首逗号的语义，
/// 在静态检查侧属于"渲染噪音"，修去后既能解析、又保留全部真实列/条件供规则检查。
pub fn repair_dynamic_artifacts(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // '(' 后（允许空白）紧跟 ',' → 跳掉该逗号，保留单空格分隔
        if c == '(' {
            out.push('(');
            let mut j = i + 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && chars[j] == ',' {
                j += 1;
                while j < n && chars[j].is_whitespace() {
                    j += 1;
                }
                out.push(' ');
                i = j;
                continue;
            }
            i = j;
            continue;
        }
        // ',' 后处理
        if c == ',' {
            let mut j = i + 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            // ',)' → 跳掉逗号
            if j < n && chars[j] == ')' {
                i = j;
                continue;
            }
            // ', FROM/WHERE/...' → 跳掉逗号
            if let Some(kw_end) =
                match_keyword_after_ws(&chars, j, &["FROM", "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "UNION", "VALUES"])
            {
                i = kw_end;
                continue;
            }
            out.push(',');
            i += 1;
            continue;
        }
        // 'SET' 后（允许空白）紧跟 ',' → 跳掉该逗号（保留 SET 关键字）
        if is_word(&chars, i, "SET") {
            let mut j = i + 3;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && chars[j] == ',' {
                j += 1;
                while j < n && chars[j].is_whitespace() {
                    j += 1;
                }
                out.push_str("SET ");
                i = j;
                continue;
            }
            out.push('S');
            i += 1;
            continue;
        }
        // 'SELECT' 后（允许空白）紧跟 ',' → 跳掉该逗号：逗号前置写法写在
        // CTE 列列表（`WITH x AS (SELECT , col ...)`）里产生的首逗号，合法 SQL 中
        // `SELECT ,` 不可能出现。
        if is_word(&chars, i, "SELECT") {
            let mut j = i + 6;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && chars[j] == ',' {
                j += 1;
                while j < n && chars[j].is_whitespace() {
                    j += 1;
                }
                out.push_str("SELECT ");
                i = j;
                continue;
            }
            out.push('S');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 从 `start` 起跳过空白后，是否匹配 `kw`（大小写不敏感）且其后为词边界。
/// 命中返回 kw **起始**位置（调用方据此只跳过前导逗号、保留关键字本身），否则 `None`。
fn match_keyword_after_ws(
    chars: &[char],
    start: usize,
    kws: &[&str],
) -> Option<usize> {
    let mut j = start;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    for kw in kws {
        let bytes: Vec<char> = kw.chars().collect();
        if j + bytes.len() <= chars.len() {
            let slice: String = chars[j..j + bytes.len()].iter().collect();
            if slice.eq_ignore_ascii_case(kw) {
                let after = j + bytes.len();
                let boundary = after >= chars.len()
                    || !chars[after].is_ascii_alphanumeric() && chars[after] != '_';
                if boundary {
                    return Some(j);
                }
            }
        }
    }
    None
}

/// `chars[i..]` 在词边界内是否以 `w` 开头（大小写不敏感）。
fn is_word(chars: &[char], i: usize, w: &str) -> bool {
    if i > 0 {
        let prev = chars[i - 1];
        if prev.is_ascii_alphanumeric() || prev == '_' {
            return false;
        }
    }
    let wb: Vec<char> = w.chars().collect();
    if i + wb.len() > chars.len() {
        return false;
    }
    let slice: String = chars[i..i + wb.len()].iter().collect();
    if !slice.eq_ignore_ascii_case(w) {
        return false;
    }
    let after = i + wb.len();
    after >= chars.len() || !chars[after].is_ascii_alphanumeric() && chars[after] != '_'
}

// ==================== XML 解析（保留结构）====================

/// 第一遍：收集 `<sql id="...">` 片段为 DynNode 树。
fn collect_sql_fragments_dynamic(
    content: &str,
) -> Result<HashMap<String, Vec<DynNode>>, SqlGuardError> {
    let mut scanner = XmlScanner::new(content);
    let mut fragments = HashMap::new();

    while let Some(result) = scanner.next_event() {
        let (event, _line) = result?;
        if let Event::Start(e) = event {
            let name = lowercased_name(&e);
            if name == "sql" {
                if let Some(id) = extract_attr(e.attributes(), "id")? {
                    let nodes = scanner.collect_nodes_until_end("sql")?;
                    fragments.insert(id, nodes);
                }
            }
        }
    }
    Ok(fragments)
}

/// 第二遍：解析 `<select>/<insert>/<update>/<delete>` 为 DynamicStatement。
fn parse_statements_dynamic(
    content: &str,
    fragments: &HashMap<String, Vec<DynNode>>,
) -> Result<Vec<DynamicStatement>, SqlGuardError> {
    let mut scanner = XmlScanner::new(content);
    let mut results = Vec::new();

    while let Some(result) = scanner.next_event() {
        let (event, line) = result?;
        if let Event::Start(e) = event {
            let name = lowercased_name(&e);
            if SQL_TAGS.contains(&name.as_str()) {
                let stmt_id = extract_attr(e.attributes(), "id")?.unwrap_or_default();
                let mut nodes = scanner.collect_nodes_until_end(&name)?;
                // 内联展开 <include>（递归同文件内片段）
                nodes = inline_includes(nodes, fragments)?;
                if !nodes
                    .iter()
                    .any(|n| matches!(n, DynNode::Text(t) if !t.trim().is_empty()))
                {
                    continue;
                }
                results.push(DynamicStatement {
                    statement_id: stmt_id,
                    statement_type: name,
                    raw_xml_line: line,
                    root_nodes: nodes,
                });
            }
        }
    }
    Ok(results)
}

/// 递归把 `DynNode::Text` 中的 `<include refid="..."/>` 标记替换为片段节点。
///
/// 注意：`<include>` 在 XML 解析时被当作文本收集（因为它在 `collect_nodes_until_end`
/// 中作为 Empty 事件处理，但当前实现把 include 的 refid 文本保留了）。
/// 实际上更稳妥的做法是：解析阶段就处理 include。这里在解析后递归替换。
fn inline_includes(
    nodes: Vec<DynNode>,
    fragments: &HashMap<String, Vec<DynNode>>,
) -> Result<Vec<DynNode>, SqlGuardError> {
    inline_includes_depth(nodes, fragments, 0)
}

/// 片段内可以再写 `<include>`，需要递归展开；`depth` 防止片段互相引用造成死循环。
const MAX_INCLUDE_DEPTH: usize = 10;

fn inline_includes_depth(
    nodes: Vec<DynNode>,
    fragments: &HashMap<String, Vec<DynNode>>,
    depth: usize,
) -> Result<Vec<DynNode>, SqlGuardError> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            DynNode::Text(t) => {
                // 文本中可能含 <include refid="..."/>（来自 collect_nodes_until_end 的 Empty 事件处理）
                if t.contains("<include") {
                    let expanded = expand_include_in_text(&t, fragments, depth)?;
                    out.extend(expanded);
                } else {
                    out.push(DynNode::Text(t));
                }
            }
            DynNode::If { test, children } => {
                out.push(DynNode::If {
                    test,
                    children: inline_includes_depth(children, fragments, depth)?,
                });
            }
            DynNode::Choose {
                when_clauses,
                otherwise,
            } => {
                let mut new_when = Vec::with_capacity(when_clauses.len());
                for (test, ch) in when_clauses {
                    new_when.push((test, inline_includes_depth(ch, fragments, depth)?));
                }
                let new_oth = match otherwise {
                    Some(ch) => Some(inline_includes_depth(ch, fragments, depth)?),
                    None => None,
                };
                out.push(DynNode::Choose {
                    when_clauses: new_when,
                    otherwise: new_oth,
                });
            }
            DynNode::ForEach {
                open,
                close,
                separator,
                children,
            } => {
                out.push(DynNode::ForEach {
                    open,
                    close,
                    separator,
                    children: inline_includes_depth(children, fragments, depth)?,
                });
            }
            DynNode::Where(ch) => {
                out.push(DynNode::Where(inline_includes_depth(ch, fragments, depth)?));
            }
            DynNode::Set(ch) => {
                out.push(DynNode::Set(inline_includes_depth(ch, fragments, depth)?));
            }
            DynNode::Trim {
                prefix,
                suffix,
                prefix_overrides,
                suffix_overrides,
                children,
            } => {
                out.push(DynNode::Trim {
                    prefix,
                    suffix,
                    prefix_overrides,
                    suffix_overrides,
                    children: inline_includes_depth(children, fragments, depth)?,
                });
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// 把文本中的 `<include refid="..."/>` 替换为片段的 DynNode 副本。
/// 借用 `resolve_includes` 提取 refid，但这里返回节点而非文本。
fn expand_include_in_text(
    text: &str,
    fragments: &HashMap<String, Vec<DynNode>>,
    depth: usize,
) -> Result<Vec<DynNode>, SqlGuardError> {
    // 复用 include 模块的解析：把文本中的 <include> 替换为占位标记，
    // 再按标记切分，遇到标记插入片段节点副本。
    // 简单实现：遍历文本，遇到 <include refid="..."/> 提取 refid，插入片段。
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<DynNode> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<'
            && i + 7 < chars.len()
            && chars[i + 1..i + 8].iter().collect::<String>() == "include"
        {
            // 找到标签结束 >
            let mut j = i + 7;
            while j < chars.len() && chars[j] != '>' {
                j += 1;
            }
            if j >= chars.len() {
                // 未闭合，当作普通文本
                buf.push_str(&chars[i..].iter().collect::<String>());
                break;
            }
            let tag: String = chars[i..=j].iter().collect();
            // 提取 refid（复用 include 模块逻辑）
            let tag_for_extract = tag.clone();
            if let Some(refid) = extract_refid_from_tag(&tag_for_extract) {
                if !buf.is_empty() {
                    out.push(DynNode::Text(std::mem::take(&mut buf)));
                }
                if let Some(frag) = fragments.get(&refid) {
                    // 片段自身可能还含 <include>，递归展开（有深度上限防环）
                    if depth < MAX_INCLUDE_DEPTH {
                        out.extend(inline_includes_depth(
                            frag.clone(),
                            fragments,
                            depth + 1,
                        )?);
                    } else {
                        out.extend(frag.iter().cloned());
                    }
                } else {
                    out.push(DynNode::Text(format!("/* missing include: {} */", refid)));
                }
            } else {
                buf.push_str(&tag);
            }
            i = j + 1;
        } else {
            buf.push(chars[i]);
            i += 1;
        }
    }
    if !buf.is_empty() {
        out.push(DynNode::Text(buf));
    }
    Ok(out)
}

fn extract_refid_from_tag(tag: &str) -> Option<String> {
    let key = "refid";
    let idx = tag.find(key)?;
    let mut j = idx + key.len();
    let bytes = tag.as_bytes();
    while j < bytes.len()
        && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'=')
    {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    let quote = bytes[j];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let rest = &tag[j + 1..];
    let end = rest.find(quote as char)?;
    Some(rest[..end].to_string())
}

// ==================== quick-xml 事件扫描器（保留结构版）====================

struct XmlScanner<'a> {
    reader: Reader<&'a [u8]>,
    content: &'a str,
    buf: Vec<u8>,
    last_pos: usize,
    current_line: usize,
}

impl<'a> XmlScanner<'a> {
    fn new(content: &'a str) -> Self {
        let reader = Reader::from_str(content);
        Self {
            reader,
            content,
            buf: Vec::new(),
            last_pos: 0,
            current_line: 1,
        }
    }

    fn next_event(
        &mut self,
    ) -> Option<Result<(quick_xml::events::Event<'static>, usize), SqlGuardError>> {
        let line_before = self.current_line;
        let event = self.reader.read_event_into(&mut self.buf);
        let cur_pos = self.reader.buffer_position().min(self.content.len());
        let bytes = self.content.as_bytes();
        for b in &bytes[self.last_pos..cur_pos] {
            if *b == b'\n' {
                self.current_line += 1;
            }
        }
        self.last_pos = cur_pos;
        match event {
            Ok(Event::Eof) => None,
            Ok(e) => {
                let owned = e.into_owned();
                self.buf.clear();
                Some(Ok((owned, line_before)))
            }
            Err(e) => Some(Err(SqlGuardError::MapperError(format!(
                "XML parse error: {}",
                e
            )))),
        }
    }

    /// 从当前位置（刚读完 Start 标签）收集 DynNode 树，直到匹配的 End。
    fn collect_nodes_until_end(&mut self, end_tag: &str) -> Result<Vec<DynNode>, SqlGuardError> {
        let end_bytes = end_tag.as_bytes();
        let mut out: Vec<DynNode> = Vec::new();
        let mut text_buf = String::new();

        while let Some(result) = self.next_event() {
            let (event, _line) = result?;
            match event {
                Event::Text(t) => {
                    let unescaped = t.unescape().map_err(|e| {
                        SqlGuardError::MapperError(format!("XML text unescape error: {}", e))
                    })?;
                    text_buf.push_str(&unescaped);
                }
                // ★ `<![CDATA[ ... ]]>` 内容原样保留。mapper 里常用它包住
                // `<=` / `>=` / `<>`，旧实现直接丢弃会把比较条件整段吃掉。
                Event::CData(c) => {
                    text_buf.push_str(&String::from_utf8_lossy(c.as_ref()));
                }
                Event::Empty(e) => {
                    let name = lowercased_name(&e);
                    match name.as_str() {
                        "include" => {
                            // 保留为文本标记，后续 inline_includes 替换
                            if let Some(refid) = extract_attr(e.attributes(), "refid")? {
                                if !text_buf.is_empty() {
                                    out.push(DynNode::Text(std::mem::take(&mut text_buf)));
                                }
                                out.push(DynNode::Text(format!("<include refid=\"{}\"/>", refid)));
                            }
                        }
                        "bind" => {
                            if !text_buf.is_empty() {
                                out.push(DynNode::Text(std::mem::take(&mut text_buf)));
                            }
                            out.push(DynNode::Bind);
                        }
                        _ => {
                            // 其他自闭合标签（罕见）：忽略标签本身，保留属性无意义
                        }
                    }
                }
                Event::Start(e) => {
                    if !text_buf.is_empty() {
                        out.push(DynNode::Text(std::mem::take(&mut text_buf)));
                    }
                    let name = lowercased_name(&e);
                    let node = self.parse_dynamic_element(&e, &name)?;
                    out.push(node);
                }
                Event::End(e) => {
                    if e.name().as_ref().eq_ignore_ascii_case(end_bytes) {
                        if !text_buf.is_empty() {
                            out.push(DynNode::Text(std::mem::take(&mut text_buf)));
                        }
                        return Ok(out);
                    }
                    // 其他 End 标签（不应出现，因为 Start 都已配对消费）：忽略
                }
                _ => {}
            }
        }
        Err(SqlGuardError::MapperError(format!(
            "Unexpected EOF while collecting <{}> content",
            end_tag
        )))
    }

    /// 解析一个动态 SQL 元素（已读 Start，需读至匹配 End）。
    fn parse_dynamic_element(
        &mut self,
        start: &quick_xml::events::BytesStart<'_>,
        name: &str,
    ) -> Result<DynNode, SqlGuardError> {
        match name {
            "if" => {
                let test = extract_attr(start.attributes(), "test")?.unwrap_or_default();
                let children = self.collect_nodes_until_end("if")?;
                Ok(DynNode::If { test, children })
            }
            "choose" => {
                let mut when_clauses: Vec<(String, Vec<DynNode>)> = Vec::new();
                let mut otherwise: Option<Vec<DynNode>> = None;
                // choose 内部只能是 when/otherwise 序列
                while let Some(result) = self.next_event() {
                    let (event, _line) = result?;
                    match event {
                        Event::Start(e) => {
                            let n = lowercased_name(&e);
                            match n.as_str() {
                                "when" => {
                                    let test =
                                        extract_attr(e.attributes(), "test")?.unwrap_or_default();
                                    let ch = self.collect_nodes_until_end("when")?;
                                    when_clauses.push((test, ch));
                                }
                                "otherwise" => {
                                    let ch = self.collect_nodes_until_end("otherwise")?;
                                    otherwise = Some(ch);
                                }
                                _ => {
                                    // 未知子元素：跳过至匹配 End
                                    let _ = self.collect_nodes_until_end(&n)?;
                                }
                            }
                        }
                        Event::End(e) => {
                            if e.name().as_ref().eq_ignore_ascii_case(b"choose") {
                                break;
                            }
                        }
                        Event::Text(_) => {
                            // choose 内部纯文本（通常只有空白）：忽略
                        }
                        _ => {}
                    }
                }
                Ok(DynNode::Choose {
                    when_clauses,
                    otherwise,
                })
            }
            "foreach" => {
                let collection =
                    extract_attr(start.attributes(), "collection")?.unwrap_or_default();
                let item = extract_attr(start.attributes(), "item")?.unwrap_or_default();
                let index = extract_attr(start.attributes(), "index")?.unwrap_or_default();
                let open = extract_attr(start.attributes(), "open")?.unwrap_or_default();
                let close = extract_attr(start.attributes(), "close")?.unwrap_or_default();
                let separator = extract_attr(start.attributes(), "separator")?.unwrap_or_default();
                let children = self.collect_nodes_until_end("foreach")?;
                // children 中的 #{item} 占位符在文本里，foreach 变体展开时按 count 复制
                let _ = (collection, item, index); // 暂未使用，保留以备将来按 collection 名生成 fixture 提示
                Ok(DynNode::ForEach {
                    open,
                    close,
                    separator,
                    children,
                })
            }
            "where" => {
                let children = self.collect_nodes_until_end("where")?;
                Ok(DynNode::Where(children))
            }
            "set" => {
                let children = self.collect_nodes_until_end("set")?;
                Ok(DynNode::Set(children))
            }
            "trim" => {
                let prefix = extract_attr(start.attributes(), "prefix")?.unwrap_or_default();
                let suffix = extract_attr(start.attributes(), "suffix")?.unwrap_or_default();
                let prefix_overrides =
                    extract_attr(start.attributes(), "prefixOverrides")?.unwrap_or_default();
                let suffix_overrides =
                    extract_attr(start.attributes(), "suffixOverrides")?.unwrap_or_default();
                let children = self.collect_nodes_until_end("trim")?;
                Ok(DynNode::Trim {
                    prefix,
                    suffix,
                    prefix_overrides,
                    suffix_overrides,
                    children,
                })
            }
            // `<selectKey>` 是**独立的**主键取值语句，不属于外层 INSERT 的 SQL 文本。
            // 若当作透明容器展开，会把 `SELECT SEQ.NEXTVAL FROM DUAL` 直接拼进
            // INSERT 里造成语法错误，故整体丢弃。
            "selectkey" => {
                let _ = self.collect_nodes_until_end(name)?;
                Ok(DynNode::Text(String::new()))
            }
            _ => {
                // 未知动态标签：当作透明容器，收集至匹配 End
                let children = self.collect_nodes_until_end(name)?;
                // 用 Text 节点保留 children，丢弃标签本身语义
                // （未知标签无法正确展开，保守起见把 children 文本拼接）
                let mut text = String::new();
                for ch in children {
                    if let DynNode::Text(t) = ch {
                        text.push_str(&t);
                    }
                }
                Ok(DynNode::Text(text))
            }
        }
    }
}

fn lowercased_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    let name = e.name();
    let name_bytes = name.as_ref();
    String::from_utf8_lossy(name_bytes).to_ascii_lowercase()
}

fn extract_attr(
    attrs: quick_xml::events::attributes::Attributes<'_>,
    key: &str,
) -> Result<Option<String>, SqlGuardError> {
    for attr in attrs {
        let attr = attr
            .map_err(|e| SqlGuardError::MapperError(format!("XML attribute parse error: {}", e)))?;
        if attr.key.as_ref().eq_ignore_ascii_case(key.as_bytes()) {
            let v = attr.unescape_value().map_err(|e| {
                SqlGuardError::MapperError(format!("XML attribute unescape error: {}", e))
            })?;
            return Ok(Some(v.into_owned()));
        }
    }
    Ok(None)
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_tmp(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("sqlguard_dynamic_{}.xml", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// 把变体列表的 (sql, label) 简化为可断言形式。
    fn variant_tuples(vs: &[Variant]) -> Vec<(String, String)> {
        vs.iter()
            .map(|v| (v.sql.trim().to_string(), v.label.clone()))
            .collect()
    }

    #[test]
    fn single_if_full_combination() {
        let xml = r#"<mapper>
  <select id="find">
    SELECT * FROM users
    <where>
      <if test="name != null">AND name = #{name}</if>
    </where>
  </select>
</mapper>"#;
        let path = write_tmp("single_if", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        assert_eq!(stmts.len(), 1);
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        // 1 个 if → 2 变体（true / false）
        assert_eq!(
            vs.len(),
            2,
            "should have 2 variants: {:?}",
            variant_tuples(&vs)
        );
        // true 变体含 WHERE name = ?
        let true_v = vs.iter().find(|v| v.label.contains("=true")).unwrap();
        assert!(
            true_v.sql.contains("WHERE"),
            "true variant sql: {}",
            true_v.sql
        );
        assert!(
            true_v.sql.contains("name = ?"),
            "true variant sql: {}",
            true_v.sql
        );
        assert!(
            !true_v.sql.contains("AND name"),
            "leading AND should be stripped: {}",
            true_v.sql
        );
        // false 变体不含 WHERE（where 内部为空）
        let false_v = vs.iter().find(|v| v.label.contains("=false")).unwrap();
        assert!(
            !false_v.sql.contains("WHERE"),
            "false variant sql: {}",
            false_v.sql
        );
    }

    #[test]
    fn two_ifs_full_combination_4_variants() {
        let xml = r#"<mapper>
  <select id="find">
    SELECT * FROM users
    <where>
      <if test="name != null">AND name = #{name}</if>
      <if test="age != null">AND age > #{age}</if>
    </where>
  </select>
</mapper>"#;
        let path = write_tmp("two_ifs", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        assert_eq!(
            vs.len(),
            4,
            "2 ifs → 4 variants, got: {:?}",
            variant_tuples(&vs)
        );
    }

    /// 回归测试：嵌套 `<if>` 的内层分支在外层激活时必须可达。
    ///
    /// 问题 5 的根因是嵌套 if 的索引计数器未跨过子树，导致 `active` 落到内层 if
    /// 时其外层 if 被判为 false、子树被整体折叠，最终「多个分支渲染成同一条基线
    /// SQL」。修复后 `active` 落在某 if 的 `subtree_span` 区间即视为该 if 为 true，
    /// 内层 if 才能被正确展开。
    #[test]
    fn nested_if_inner_branch_reachable() {
        let xml = r#"<mapper>
  <select id="q">
    SELECT * FROM t
    <where>
      <if test="a != null">AND a = #{a}</if>
      <if test="b != null">
        AND b = #{b}
        <if test="c != null">AND c = #{c}</if>
      </if>
    </where>
  </select>
</mapper>"#;
        let path = write_tmp("nested_if", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        // 关键：内层 if（c）在其外层 if（b）激活时必须可达，不能退化为基线 SQL。
        assert!(
            vs.iter()
                .any(|v| v.sql.contains("b = ?") && v.sql.contains("c = ?")),
            "内层 if 不可达 / 退化为基线，变体：{:?}",
            variant_tuples(&vs)
        );
        // b 单独（c 关闭）也应存在，证明两个分支相互独立。
        assert!(
            vs.iter()
                .any(|v| v.sql.contains("b = ?") && !v.sql.contains("c = ?")),
            "b 单独分支缺失，变体：{:?}",
            variant_tuples(&vs)
        );
    }

    #[test]
    fn choose_expands_when_plus_otherwise() {
        let xml = r#"<mapper>
  <select id="find">
    SELECT * FROM users WHERE
    <choose>
      <when test="id != null">id = #{id}</when>
      <when test="name != null">name = #{name}</when>
      <otherwise>1 = 1</otherwise>
    </choose>
  </select>
</mapper>"#;
        let path = write_tmp("choose", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        assert_eq!(
            vs.len(),
            3,
            "2 when + 1 otherwise → 3 variants: {:?}",
            variant_tuples(&vs)
        );
        assert!(vs
            .iter()
            .any(|v| v.label.contains("when0") && v.sql.contains("id = ?")));
        assert!(vs
            .iter()
            .any(|v| v.label.contains("when1") && v.sql.contains("name = ?")));
        assert!(vs
            .iter()
            .any(|v| v.label.contains("otherwise") && v.sql.contains("1 = 1")));
    }

    #[test]
    fn foreach_three_size_variants() {
        let xml = r#"<mapper>
  <select id="findByIds">
    SELECT * FROM users WHERE id IN
    <foreach collection="ids" item="id" open="(" close=")" separator=",">
      #{id}
    </foreach>
  </select>
</mapper>"#;
        let path = write_tmp("foreach", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        assert_eq!(
            vs.len(),
            3,
            "foreach → 3 size variants: {:?}",
            variant_tuples(&vs)
        );
        // 0 元素：IN 后为空
        let zero = vs.iter().find(|v| v.label.contains("0elem")).unwrap();
        assert!(
            !zero.sql.contains("()"),
            "0 elem should not produce (): {}",
            zero.sql
        );
        // 1 元素：含 ( 和 ? 和 )
        let one = vs.iter().find(|v| v.label.contains("1elem")).unwrap();
        let one_trim = one.sql.trim();
        assert!(
            one_trim.contains("(") && one_trim.contains("?") && one_trim.contains(")"),
            "1 elem should contain ( ? ): [{}]",
            one_trim
        );
        // 3 元素：含 3 个 ?
        let three = vs.iter().find(|v| v.label.contains("3elem")).unwrap();
        let q_count = three.sql.matches('?').count();
        assert_eq!(q_count, 3, "3 elem should have 3 ?: [{}]", three.sql);
    }

    #[test]
    fn set_tag_strips_trailing_comma() {
        let xml = r#"<mapper>
  <update id="update">
    UPDATE users
    <set>
      <if test="name != null">name = #{name},</if>
      <if test="age != null">age = #{age},</if>
    </set>
    WHERE id = #{id}
  </update>
</mapper>"#;
        let path = write_tmp("set", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        // 2 ifs → 4 variants
        assert_eq!(vs.len(), 4);
        // 找一个 name!=null=true, age!=null=false 的变体
        let name_only = vs
            .iter()
            .find(|v| v.label.contains("name!=null=true") && v.label.contains("age!=null=false"))
            .expect("should find name=true,age=false variant");
        assert!(
            name_only.sql.contains("SET name = ?"),
            "sql: {}",
            name_only.sql
        );
        assert!(
            !name_only.sql.contains("name = ?,"),
            "trailing comma should be stripped: {}",
            name_only.sql
        );
    }

    #[test]
    fn threshold_degradation_to_single_branch() {
        // 10 个 if > 阈值 8 → 降级为单分支激活：基线 + 10 个 = 11 变体
        let mut ifs = String::new();
        for i in 0..10 {
            ifs.push_str(&format!(
                "<if test=\"c{} != null\">AND c{} = #{{c{}}}</if>\n",
                i, i, i
            ));
        }
        let xml = format!(
            r#"<mapper>
  <select id="find">
    SELECT * FROM users
    <where>
      {}
    </where>
  </select>
</mapper>"#,
            ifs
        );
        let path = write_tmp("threshold", &xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], 8);
        assert_eq!(
            vs.len(),
            11,
            "10 ifs with threshold 8 → 11 variants (baseline + 10): {:?}",
            variant_tuples(&vs)
        );
        // 基线变体：所有 if=false，无 WHERE
        let baseline = vs.iter().find(|v| !v.label.contains("=true")).unwrap();
        assert!(
            !baseline.sql.contains("WHERE"),
            "baseline should have no WHERE: {}",
            baseline.sql
        );
    }

    #[test]
    fn no_dynamic_tags_single_variant() {
        let xml = r#"<mapper>
  <select id="findAll">
    SELECT id, name FROM users
  </select>
</mapper>"#;
        let path = write_tmp("no_dynamic", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        assert_eq!(vs.len(), 1);
        assert!(
            vs[0].label.is_empty(),
            "no dynamic → empty label: {:?}",
            vs[0]
        );
        assert!(vs[0].sql.contains("SELECT"));
    }

    #[test]
    fn include_inlined_in_dynamic_tree() {
        let xml = r#"<mapper>
  <sql id="conds">
    <if test="status != null">AND status = #{status}</if>
  </sql>
  <select id="find">
    SELECT * FROM users
    <where>
      <include refid="conds"/>
    </where>
  </select>
</mapper>"#;
        let path = write_tmp("include", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        // include 内联后含 1 个 if → 2 变体
        assert_eq!(
            vs.len(),
            2,
            "include with 1 if → 2 variants: {:?}",
            variant_tuples(&vs)
        );
        let true_v = vs.iter().find(|v| v.label.contains("=true")).unwrap();
        assert!(
            true_v.sql.contains("status = ?"),
            "include should be inlined: {}",
            true_v.sql
        );
    }

    #[test]
    fn trim_tag_applies_prefix_and_overrides() {
        let xml = r#"<mapper>
  <select id="find">
    SELECT * FROM users
    <trim prefix="WHERE" prefixOverrides="AND |OR ">
      <if test="name != null">AND name = #{name}</if>
    </trim>
  </select>
</mapper>"#;
        let path = write_tmp("trim", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let vs = expand_variants(&stmts[0], DEFAULT_MAX_INDEPENDENT_IFS);
        let true_v = vs.iter().find(|v| v.label.contains("=true")).unwrap();
        assert!(
            true_v.sql.contains("WHERE name = ?"),
            "trim should add WHERE and strip AND: {}",
            true_v.sql
        );
        assert!(
            !true_v.sql.contains("WHERE AND"),
            "AND should be stripped: {}",
            true_v.sql
        );
    }

    /// 标点补丁只删「合法 SQL 中绝不可能出现」的标点，不改语义。
    #[test]
    fn repair_strips_impossible_punctuation() {
        assert_eq!(
            repair_dynamic_artifacts("INSERT ( , a , b )"),
            "INSERT ( a , b )",
            "'(,' and ',)' should be stripped"
        );
        assert_eq!(
            repair_dynamic_artifacts("SET , col = ? , modtm = ?"),
            "SET col = ? , modtm = ?",
            "leading comma after SET should be stripped"
        );
        assert_eq!(
            repair_dynamic_artifacts("SELECT a , b , FROM t"),
            "SELECT a , b FROM t",
            "trailing comma before FROM should be stripped"
        );
        assert_eq!(
            repair_dynamic_artifacts("WHERE a = ? , )"),
            "WHERE a = ? )",
            "comma before ) should be stripped"
        );
        // SELECT 列表前置逗号（CTE 逗号前置写法）
        assert_eq!(
            repair_dynamic_artifacts("WITH x AS (SELECT , a , b FROM t)"),
            "WITH x AS (SELECT a , b FROM t)"
        );
        // 正常 SQL 不应被改动
        assert_eq!(
            repair_dynamic_artifacts("SELECT a, b FROM t WHERE x = ?"),
            "SELECT a, b FROM t WHERE x = ?"
        );
    }

    /// FirstBranch 把同层互斥 `<if>` 折叠为第一个分支，避免 AllTrue 把互斥分支全拼
    /// 导致条件间缺 AND（`where x = 1 y = 2`）。
    #[test]
    fn first_branch_takes_first_of_exclusive_group() {
        let xml = r#"<mapper>
  <select id="q">
    SELECT * FROM t
    where
    <if test="a == 0">X = 1</if>
    <if test="a == 1">Y = 2</if>
    <if test="b != null">and Z = #{b}</if>
  </select>
</mapper>"#;
        let path = write_tmp("first_branch", xml);
        let stmts = parse_dynamic_statements(&path).unwrap();
        let fb = render_canonical(&stmts[0].root_nodes, RenderMode::FirstBranch);
        let fb = fb.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(fb.contains("X = 1"), "first branch should render: {}", fb);
        assert!(
            !fb.contains("Y = 2"),
            "other exclusive branch must be dropped: {}",
            fb
        );
        assert!(
            !fb.contains("Z ="),
            "independent sibling also collapsed by fallback: {}",
            fb
        );
    }
}
