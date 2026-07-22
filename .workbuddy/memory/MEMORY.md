# SqlGuard 项目长期记忆

## 如何给 AST 包装层新增能力（改规则的固定套路）
位置：`src/rule/engine.rs`。步骤（缺一不可）：
1. 新增/修改 `XxxInfo` 结构体字段（derive Debug+Clone+Default）。
2. 若挂在语句上：给 `StmtInfo` 加 `Option<XxxInfo>` 字段，并更新 `convert_statement` 里**所有**构造分支（每个都要补 `xxx: None`，漏一个就编译失败）。
3. 在 `convert_statement` 对应 `Statement::Xxx` 分支填充数据。
4. 给 `impl XxxInfo` / `impl StmtInfo` 加方法。
5. 在 `build_engine()` 里 `register_type_with_name` + `register_fn`（返回 Vec 的闭包需转 Array）。
6. 改 `.rhai` 规则（`config/rules/` 与 `examples/sql-guard/config/rules/` 两份要同步），更新 `docs/rule-scripting.md`、`docs/default-rules.md`，补 `examples/`，写集成测试。

## sqlparser 0.45 易错点
- `ALTER TABLE ... ADD PRIMARY KEY` → `AlterTableOperation::AddConstraint(TableConstraint::PrimaryKey)`（无独立 AddPrimaryKey 变体）；`DropPrimaryKey` 是独立变体。
- 集合运算：`SetExpr::SetOperation { op: SetOperator{Union/Except/Intersect}, set_quantifier: SetQuantifier{All/Distinct/...} }`。
  **ALL/DISTINCT 在 set_quantifier，不在 op** —— 用 op_str.contains("ALL") 恒为 false（历史 bug）。
- 语句位置：每条 `Statement` 不带行列号，需在 `parse_sql_to_ast` 用 `Parser::peek_token().location` 取。

## 环境坑（Windows + Git Bash）
- 手工/集成测试调用 `target/release/sqlguard`（原生 exe）时，**必须用 Windows 绝对路径**（如 `C:/tmp/...`）。
  Git Bash 的 `/tmp` 是 MSYS 路径，原生 exe 解析成 `C:\tmp`，两者不一致会导致"找不到配置/文件"。

## 测试
- 集成测试在 `tests/integration_test.rs`，调用 release 二进制；改动引擎后先 `cargo build --release` 再 `cargo test --release --test integration_test`。
- 规则脚本测试：在测试里把仓库 `config/rules/...` 复制到临时项目 `config/rules/...`，再写 `sqlguard.toml` 引用。
