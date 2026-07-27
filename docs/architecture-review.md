# SqlGuard 技术架构批判

> 评审者：严苛架构师视角
> 日期：2026-07-23
> 对象：SqlGuard v0.1.0，Rust 编写的 SQL 脚本检查工具，6215 行源码

---

## 一、总体评价

**能跑，能用，但经不起推敲。** 作为一个 CI/CD 流水线工具，它解决了"有"的问题，但没解决"好"的问题。架构上存在若干结构性缺陷，如果不在早期修正，技术债会随规则数量增长呈指数级膨胀。

---

## 二、致命问题

### 2.1 engine.rs 是一颗 3214 行的定时炸弹

**这是整个项目最大的架构失败。** 单文件 3214 行，占源码总量的 51.7%。里面混装了：

- SQL 解析（parse_sql_to_ast）
- AST 类型转换（convert_statement，覆盖 20+ 种语句类型）
- 查询分析（analyze_query，含子查询递归、窗口函数收集、CTE 提取）
- 文本扫描器（detect_comma_join_in_sql、collect_comments）
- Rhai 引擎构建（build_engine，手动注册所有类型和方法）
- Rhai scope 构建与脚本执行（run_single_rule）
- 规则筛选器（RuleFilter）

**没有任何模块边界。** 想改"子查询递归深度"要去翻 3000 行找在哪；想改"Rhai 方法注册"得在同一文件里上下跳。这不是一个模块，是一个垃圾场。

**正确做法：** 拆成至少 5 个子模块：
- `engine/ast.rs` — AST 包装类型定义
- `engine/parser.rs` — SQL 解析与位置追踪
- `engine/analyzer.rs` — 查询分析（子查询、窗口函数、CTE）
- `engine/scanner.rs` — 文本扫描器（逗号 JOIN 检测、注释收集）
- `engine/runner.rs` — Rhai 引擎构建与规则执行

### 2.2 main.rs 同样臃肿（969 行），且职责混乱

`run_check` 和 `run_check_diff` 两个函数各 200+ 行，里面有大量重复逻辑：配置加载、路径解析、文件收集、报告输出。更荒唐的是 **16 条规则脚本的完整内容硬编码在 main.rs 里**（`get_no_drop_table_script()` 等函数），用 `&'static str` 返回。

这意味着：**改一条规则脚本的默认内容，要改 main.rs。** 想加一条新规则，要改 main.rs 的 `run_init` 函数 + `generate_default_config` 函数 + `get_default_config_content` 函数 + 写一个新的 `get_xxx_script()` 函数。四个地方，全靠人肉同步。

**正确做法：** 规则脚本用 `include_str!` 从文件嵌入，或打包到 `assets/` 目录在编译时拷贝。默认配置用 TOML 文件而不是 Rust 结构体字面量。

### 2.3 run_check 和 run_check_diff 大量重复

两个函数的前半段（配置加载、filter 构建、路径解析）几乎一模一样。后段的文件遍历 + 规则执行逻辑也高度相似，只是 check_diff 多了一层 hunk 过滤。这是典型的 copy-paste 架构。

**正确做法：** 抽取 `check_files(config, filter, engine, files) -> Vec<Violation>`，check_diff 在此基础上加 diff 过滤层。

---

## 三、设计缺陷

### 3.1 Rhai 作为规则语言的选型风险

Rhai 的问题不在于它能不能跑，而在于**它是一个沙盒脚本语言，但你的规则需要深度访问 AST**。当前做法是把 20+ 个 Rust 结构体逐一注册到 Rhai，为每个类型手写几十个 `register_fn`。这导致：

- **类型注册代码爆炸**：build_engine 函数可能有数百行纯注册代码
- **Rhai 类型系统限制**：返回 `Vec<CustomType>` 需要手动 `.into_iter().map(Dynamic::from).collect()` 转为 Array
- **版本绑定风险**：rhai 1.x 的 API 不稳定，升级成本高
- **调试困难**：Rhai 脚本出错时，错误信息对规则作者不友好

如果规则脚本数量增长到 50+ 条，这套注册机制会成为维护噩梦。

**替代方案：** 如果规则复杂度确实需要 AST 访问，考虑用 Rust 插件机制（WASM/动态库）或用 Python/JavaScript 作为规则语言（生态成熟，调试工具完善）。如果规则简单，纯文本匹配就够了，不需要 Rhai。

### 3.2 AST 包装层的"翻译损耗"

sqlparser 的 Statement enum 本身就是完整的 AST，项目又包了一层 `StmtInfo`，把每种语句类型拆成独立的 Info 结构体。这导致：

- 每次新增一种语句支持，要改 `convert_statement` + 新增 `XxxInfo` 结构体 + 在 Rhai 注册 + 写测试
- 翻译过程中会丢失信息（比如 sqlparser 原始 AST 里的某些细节字段被忽略）
- 两套模型之间的一致性靠人维护，没有编译时保证

**这层包装的收益是什么？** "不让规则脚本接触 sqlparser 的复杂 enum"——但这恰恰是 Rhai 的限制，不是架构的必然。如果用 Rust 写规则，直接 match Statement 就完了。

### 3.3 位置信息追踪的脆弱性

`parse_sql_to_ast` 用 `peek_token()` 在解析前抓位置，end_line 用"下一个 token 位置 - 1"推测。这个方案：

- 依赖 sqlparser 内部行为（peek_token 返回的是哪个 token）
- 多语句文件中，最后一条语句的 end_line 可能不准
- 解析失败时（PARSE_ERROR），位置信息完全丢失
- 注释和空白行的位置无法精确关联到语句

**对 CI/CD 工具来说，位置不准 = 误报/漏报。** 增量校验（check-diff）直接依赖行号做 hunk 交集，位置错了过滤就错了。

> **已改进（2026-07-27）。** 位置切片改用 **字符级** 偏移（`location_to_byte_offset`，因 sqlparser 0.60 的 `Location` 是 char 级、1-based，byte 偏移在多字节字符下会错位，且兼容 LF/CRLF）。同时新增 **逐语句方言回退链**（`dialect_fallback` 配置 + `--dialect-fallback` CLI），解决 GaussDB「PG 内核 + Oracle 外壳」混合方言解析：每条语句按「主方言 → 回退方言 → Generic」重试，首个成功即采用；并修复了「PG 把 `CONNECT BY` 当残缺前缀静默吞掉、回退链永不触发」的软失败问题（改用以 `;`/`EOF` 判定的「干净解析」检查）。详见 [docs/dialect-fallback.md](docs/dialect-fallback.md)。

### 3.4 Mapper 模式的 statement_type 硬编码

`mapper::map_statement_type` 把 `<select>` / `<insert>` / `<update>` / `<delete>` 硬编码映射到 `"dml"`。这意味着：

- 用户无法配置 "把 `<select>` 当 `query` 类型"
- 自定义 statement 类型（如 `<sql>` 片段）无法扩展
- 规则的 `applies_to` 字段灵活性被削弱

应该用配置映射而不是硬编码。

### 3.5 错误处理策略不一致

项目定义了 `SqlGuardError` 枚举，但使用方式混乱：

- 有些地方用 `?` 传播，有些地方用 `eprintln!` + `continue`（如 mapper XML 解析失败）
- `run_check` 中文件读取失败会中断整个检查，但 mapper XML 解析失败只跳过当前文件
- Rhai 脚本运行时错误被转为 Violation（混入正常报告），其他错误转为 ScriptError
- exit code 逻辑：`has_errors || has_missing` 时返回 Err，但 `has_missing` 是目录结构问题，和 SQL 规则违规混在一起，CI 里无法区分

### 3.6 没有缓存机制

每次 `check` 都重新解析所有文件、重建 Rhai 引擎。对于大型项目（数百个 SQL 文件），这会很慢。`build_engine()` 在 `run_check` 里只调一次（好），但 AST 解析是每个文件一次且无缓存。

check-diff 模式下虽然只检查改动文件，但仍然每次都从头解析。可以考虑文件级 mtime/hash 缓存。

---

## 四、工程质量问题

### 4.1 集成测试依赖 /tmp 硬编码路径

所有集成测试用 `/tmp/sqlguard-test-xxx` 路径，这在 Windows 上直接挂。`std::env::temp_dir()` 是跨平台的标准做法。

### 4.2 集成测试依赖 git 二进制

`test_check_diff_*` 系列测试调用 `Command::new("git")`，如果 CI 环境没装 git 就全挂。应该在测试前置条件中检测并 skip。

### 4.3 集成测试依赖 release 构建

`binary_abs_path()` 指向 `target/release/sqlguard`，意味着跑集成测试前必须 `cargo build --release`。这在开发循环中很慢。应该用 `cargo build` 的 debug 产物或 `#[cfg(test)]` 内联测试。

### 4.4 没有单元测试

除了 `include.rs`、`placeholder.rs`、`git_diff.rs` 有少量单元测试，核心的 `engine.rs`（3214 行）**零单元测试**。所有测试都走集成测试路径，运行慢、定位难。

### 4.5 依赖版本未锁定

`Cargo.toml` 里 `rhai = "1"`、`sqlparser = "0.45"`、`clap = "3"`，都是 major version 锁定。minor/patch 版本漂移可能导致行为变化（尤其是 sqlparser 的解析行为）。建议用 `Cargo.lock` 锁定精确版本，CI 中用 `--frozen` 构建。

### 4.6 clap v3 过时

clap v4 已经稳定很久，v3 的 derive 宏 API 有已知问题。应该升级。

---

## 五、可扩展性问题

### 5.1 规则配置与脚本分离

规则定义在 `sqlguard.toml`，脚本文件在 `config/rules/`。如果用户移动了脚本文件但忘了改配置，错误信息不会很友好。可以考虑在配置里内联简单规则，或做配置校验。

### 5.2 规则间无法共享状态

当前每条规则独立执行，无法共享中间分析结果。比如 DML001（no_select_all）和 DML005（column_references_qualified）都需要分析投影列，但各跑各的。如果规则数量增长，会有大量重复计算。

### 5.3 报告格式不可扩展

三种报告格式（plain/json/html）硬编码在 `main.rs` 的 match 分支里。想加 SARIF 格式（CI/CD 标准）要改 main.rs。应该用 trait + 注册机制。

---

## 六、优先级排序的改进建议

| 优先级 | 问题 | 影响 | 成本 |
|--------|------|------|------|
| P0 | 拆分 engine.rs | 可维护性 | 中 |
| P0 | 消除 main.rs 中规则脚本硬编码 | 可维护性 | 低 |
| P0 | 消除 run_check / run_check_diff 重复 | 可维护性 | 低 |
| P1 | engine.rs 补单元测试 | 质量保障 | 高 |
| P1 | 集成测试跨平台修复 | 兼容性 | 低 |
| P1 | 错误处理策略统一 | 用户体验 | 中 |
| P2 | 位置追踪精度提升 | 准确性 | 高 |
| P2 | 缓存机制 | 性能 | 中 |
| P2 | 报告格式可扩展 | 可扩展性 | 低 |
| P3 | Rhai 选型重新评估 | 长期维护 | 极高 |
| P3 | clap v4 升级 | 依赖健康 | 低 |

---

## 七、结语

SqlGuard 作为一个 v0.1.0 的工具，功能覆盖面不错——传统 SQL + MyBatis Mapper + 增量校验 + 三种报告格式。但架构上还停留在"能跑就行"的阶段。**最大的问题不是某个具体缺陷，而是 engine.rs 这个 3214 行的巨型文件吸收了所有复杂性，使得后续任何改动都要在一个没有模块边界的地方操作。** 如果不尽快拆分，随着规则数量增长，这个文件会变成项目的瓶颈。

建议在 v0.2 版本中优先解决 P0 问题，建立清晰的模块边界，然后逐步补测试和优化。
