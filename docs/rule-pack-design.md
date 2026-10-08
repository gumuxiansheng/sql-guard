# 规则与引擎解耦（规则包）— 需求说明与技术方案

> 状态：**M1–M5 已实现**（「独立规则仓 + 独立 CI」属组织性工作，本仓库只提供前置条件，见 §十二）
> 日期：2026-09-30
> 作者：SqlGuard Team
> 关联模块：`src/config.rs`、`src/rule/engine/runner.rs`、`src/main.rs`（`run_init`）、`src/cache.rs`、`src/cli.rs`
> 版本目标：不破坏现有 `sqlguard.toml` / `sqlguard.rules.toml` 语义；无 `[rule_packs]` 配置时行为与现状**逐字节一致**
> 方案定位：这是"规则包 + 锁文件"方案（选项 A）。远程拉取（选项 B）是本方案的**可选上层**，不在本期范围。

---

## 一、背景与目标

### 1.1 现状问题

当前规则与引擎（二进制）是**同一个发版单元**：

1. **规则脚本随引擎编译**：`main.rs` 的 `INIT_RULE_SCRIPTS` 用 `include_str!` 把每条 `.rhai` 嵌进二进制，`sqlguard init` 只是把它们"吐"到项目目录。改一条规则 = 重新发一个引擎版本。
2. **公共辅助函数写死在引擎里**：`runner.rs:15` 的 `HELPERS_SCRIPT` 用 `include_str!` 嵌入 `config/rules/lib/helpers.rhai`，每条脚本执行前 prepend（`runner.rs:361`）。规则一旦用到新 helper，就要求新引擎。
3. **规则 API = 引擎 ABI**：脚本里的 `context["ast"]`（`SqlAst` / `StmtInfo` / …）由 `runner.rs::build_engine()` 注册（类型 25 个、函数 245 个）。规则与引擎强绑定同一个 API 版本面。

已具备的解耦基础（可直接复用，不必推倒重来）：

- `config.rs:466-494` 的规则来源解析：显式 `rules_file` → 同级 `sqlguard.rules.toml` → 内联 `[[rules]]`。
- `config.rs:585-598` 的 `resolve_script_path`：脚本相对路径以规则文件所在目录（`rules_dir`）为基准解析。
- `RuleConfig` 已有的 `enabled / group / applies_to / severity / params`，天然支持"按项目裁剪规则"。
- `sqlguard.rules.toml.example` 已按"P0 默认启用 / GaussDB 默认关闭"分层——**说明"规则分级"的需求已经存在，只是没有独立版本载体**。

### 1.2 目标

- **发版解耦**：规则新增/修复走"规则包"独立版本与 CI，不必重发引擎；引擎升级（解析器/API）也不必强迫所有项目同步改规则。
- **项目自选**：不同项目/项目组可独立选择生效的规则包与包内具体规则，并可覆盖严重级别与阈值。
- **项目自编**：项目可写自己的本地规则，与包内规则并存，且优先级明确。
- **可复现**：同一份配置 + 锁文件在任意机器/CI 上得到完全相同的规则集与检查结果。
- **兼容**：不配置规则包时，与现状行为完全一致。

### 1.3 非目标

- 不引入 registry 服务、不做在线拉取（选项 B）。
- 不改 SQL 解析、AST 包装层语义（本方案只加"版本声明 + 校验"，不改 API 本身）。
- 不改 `check-diff` / `gen-rollback` / `replay-export` 的既有语义。

---

## 二、术语与分层模型

| 层 | 载体 | 版本节奏 | 职责 |
|----|------|----------|------|
| **引擎** | `sqlguard` 二进制 | 独立 tag（如 `v0.2.8`） | SQL 解析、AST/Rhai API、执行器、报告、缓存 |
| **规则包** | 目录/压缩包（`rules-pack.toml` + `*.rhai`） | 独立 tag（如 `rules-gaussdb/v1.3.0`） | 一批规则脚本 + 元数据 + 兼容声明 |
| **项目实例** | `sqlguard.toml` + `sqlguard.rules.toml` + `sqlguard.lock` | 随项目仓库 | 选包、选规则、覆盖参数、写本地规则 |

两层边界各有一条**契约**：

- 引擎 ↔ 规则包：**规则 API 契约**（`api_version` + `engine_compat`）。
- 规则包 ↔ 项目：**规则清单契约**（复用现有 `[[rules]]` 数据结构）。

```
引擎发布 v0.2.8 ──┐
                  ├──▶ 规则 API 契约 ──▶ 项目实例（选包 + 选规则 + 本地规则）
规则包发布 v1.3.0 ─┘
```

---

## 三、规则包格式定义

### 3.1 目录结构

规则包就是一个目录（压缩包是同一结构的 tar/zip）：

```
rules-gaussdb/                     # 包根（即 pack root）
├── rules-pack.toml                # ★ 包清单（必需）
├── rules/
│   ├── ddl/
│   │   ├── primary_key_required.rhai
│   │   └── ...
│   └── dml/
│       └── ...
└── lib/
    └── pack_helpers.rhai          # 可选：包内私有辅助函数
```

- 包根由 `rules-pack.toml` 唯一标识；脚本相对路径一律**相对包根**解析。
- 允许包内嵌套子目录；不做固定 `ddl/dml` 约束（`applies_to` 决定语义，目录只是组织方式）。

### 3.2 包清单 `rules-pack.toml`

```toml
[pack]
name        = "rules-gaussdb"        # 必填，全局包名（锁文件与 CLI 引用）
namespace   = "gaussdb"              # 可选，规则 id 命名空间：本包规则对外 id = gaussdb:<id>
version     = "1.3.0"                # 必填，SemVer
api_version = 1                      # 必填，本包使用的规则 API 版本（见 §四）
engine      = ">=0.2.7 <0.3"         # 可选，兼容的引擎版本范围（不满足仅告警）
description = "GaussDB 开发规范规则集"
license     = "MIT"                  # 可选
helpers     = "lib/pack_helpers.rhai" # 可选，包内辅助函数（在引擎 helpers 之后 prepend）

# 规则清单：字段与现有 RuleConfig 完全一致（config.rs:184-222）
# 此处 id 为「包内短 id」；对外规范 id = <namespace>:<id> = "gaussdb:GDB001"
[[rules]]
id          = "GDB001"
name        = "primary_key_required"
group       = "ddl-safety"
description = "CREATE TABLE 必须定义主键"
enabled     = true                   # 包内默认值，项目可覆盖（见 §六）
script_path = "rules/ddl/primary_key_required.rhai"
applies_to  = ["ddl"]
severity    = "warning"
# [rules.params] max_xxx = 3
```

要点：

- **`[[rules]]` 结构 100% 复用现有 `RuleConfig`**（`config.rs:184`），新增解析代码量最小，且包清单可直接内联进 `sqlguard.rules.toml`（见 §五兼容）。
- **`namespace` 决定 id 是否带前缀**：设了 `namespace` → 规范 id = `<namespace>:<id>`；未设 → 规范 id = 裸 `<id>`。**默认包不带 namespace**，以保证 `DDL001` / `DML001` 等现有 id 不发生用户可见变更（详见 §5.3）。
- `script_path` 相对包根解析，**不允许** `..` 逃逸包根（与 `config.rs:511-540` 的 `sanitize_relative_rules_path` 同策略）。
- 包清单自身也参与 checksum（锁文件，见 §七）。

---

## 四、规则 API 契约（`api_version` / `engine_compat`）

这是全方案的**地基**：没有它，规则包独立发版必然出现"规则静默失败"。

### 4.1 定义

- 引擎声明一个整数 `RULE_API_VERSION`（起始 `1`），语义 = **引擎暴露给 Rhai 的 API 面**：

  | 组成部分 | 变更是否升 `api_version` | 说明 |
  |----------|--------------------------|------|
  | 注册类型（`SqlAst`/`StmtInfo`/…） | 是 | 增删改类型/字段 |
  | 注册函数（`runner.rs` 的 `register_fn`） | 是 | **新增函数也升**（旧引擎没有该函数） |
  | 引擎提供的 helpers（`HELPERS_SCRIPT`） | 是 | 规则可直接调用，属 API 面 |
  | `context` 字段（`sql_content`/`params`/…） | 是 | 规则读取的契约 |
  | 纯引擎内部实现（解析器实现、性能） | 否 | 对规则不可见 |

- 兼容规则：`api_version` **单调递增，且只保证"向后兼容的加法"**。即 v1 规则在 api_version ≥1 的引擎上必然可跑；破坏性变更必须升到 v2，且引擎需按 `api_version` 保留旧注册面（或明确拒绝旧包并给出可读报错）。

- `engine` 字段是引擎**版本号**范围（SemVer range），用于表达"这个包在哪些引擎版本上验证过"。与 `api_version` 是两道不同的闸门：
  - `api_version` 不兼容 → **硬错误**（规则会静默失败，必须拦）。
  - `engine` 不满足 → **默认 warning**，`--strict-engine` 时升级为 error（允许项目在未验证的新引擎上试跑）。

### 4.2 校验时机与报错

在 `Config::load` 之后、`build_engine()` 之前（`main.rs:595` / `907` 两处入口），由新增的 `pack::resolve(&config, ...)` 统一执行：

```
解析包 → 校验 api_version ∈ 引擎支持集 → 校验 engine range（warn/error）
       → 校验 checksum（对锁文件，见 §七）→ 合并规则（§六）
```

报错必须**可读且给出动作**，示例：

```
error: rule pack 'rules-gaussdb' v1.3.0 requires rule API version 2,
       but this engine (sqlguard 0.2.7) supports up to 1.
       hint: upgrade sqlguard to >= 0.3.0, or pin the pack to a version
             with api_version = 1 (e.g. rules-gaussdb/v1.2.x).
```

---

## 五、加载与合并规则

### 5.1 配置面（新增 `[rule_packs]`）

```toml
[rules_packs]
# 包搜索路径（相对配置文件目录或绝对路径；按顺序查找同名包）
search_paths = [".sqlguard/rules", "vendor/rules", "../shared-rules"]

# 生效的包（按声明顺序决定优先级，后者覆盖前者）
packs = [
  { name = "rules-core",    version = "2.1.0" },
  { name = "rules-gaussdb", version = "1.3.0" },
  # 也可直接指向目录（跳过 search_paths 查找）
  { name = "rules-local",   path = "./my-rules" },
]

# 默认 false：忽略远程 source，仅允许 path / search_paths 命中
allow_remote = false
```

- **缺省 `[rule_packs]` 段 → 完全走现有逻辑**（`rules_file` / 同级 `sqlguard.rules.toml` / 内联 `[[rules]]`），零行为变化。
- 环境变量 `SQLGUARD_RULE_PATH`（`;` 分隔）追加搜索路径，便于 CI 注入共享包。

### 5.2 解析优先级（低 → 高）

| 优先级 | 来源 | 说明 |
|--------|------|------|
| 1（最低） | 引擎内置兜底 | **仅 helpers + 非 AST 轻量检查**（`FILE001` / `FILE002` + parse-error 兜底）；其余全部下沉到默认包 |
| 2 | `packs` 中声明的包 | 按数组顺序，靠后覆盖靠前 |
| 3 | 项目本地 `sqlguard.rules.toml`（或 `rules_file`） | 现有语义，作为最高优先级的"项目本地层" |
| 4（最高） | `[rules_packs.overrides]` | 只改 `enabled/severity/params`，不复制脚本（见 §5.4） |

### 5.3 命名空间与 id 规范

**规范 id（canonical id）** 由包清单的 `[pack].namespace` 决定：

| 场景 | 规范 id | 示例 |
|------|---------|------|
| 包设了 `namespace = "gaussdb"` | `<namespace>:<id>` | `gaussdb:GDB001` |
| 包未设 `namespace`（含**默认包**、项目本地规则、内联 `[[rules]]`） | 裸 `<id>` | `DDL001` |

- **默认包不带 namespace** → `DDL001` / `DML001` 等现有 id 对外表现不变，兑现 §10.1 兼容承诺。
- 需要隔离的业务线包（按项目组 / 规范来源拆分）设 `namespace` → 多个包可各自拥有 `DML001` 而互不冲突。
- 解析时同时保留 `canonical_id`（对外）与 `pack`（归属），供 `rules list`、报告与冲突诊断使用。
- `namespace` 字符集建议 `[a-z0-9_-]+`，禁止 `:` 与空白。

**过滤匹配语义**（`--rules` / `--groups` / `--exclude-*`）：

| 传入值 | 命中 |
|--------|------|
| `gaussdb:GDB001` | 精确匹配规范 id |
| `GDB001`（裸） | 匹配所有包的 `*:GDB001` **以及**裸 `GDB001`（跨包聚合；歧义不报错，`rules list` 可见全貌） |
| `gaussdb:*` | 该 namespace 下全部规则（前缀通配，沿用现有 `prefix*` 语义） |
| `DDL*`（裸前缀） | 匹配裸 `DDL*`，且匹配 `*:DDL*` 的冒号后缀部分（保持现有 `--rules DDL*` 直觉） |

黑名单（`--exclude-*`）语义不变，优先级仍高于白名单。

**id 冲突**（仅当两个来源产出**同一个规范 id** 才发生，通常是两包都未设 namespace）：

- 默认策略：按 §5.2 优先级"后者覆盖前者"，stderr 输出一条 `note:`（仅 stderr 为 TTY 时，避免污染 CI 日志）。
- `--strict-ids`：改为**硬错误**，列出冲突 id 与来源包，提示为该包设 `namespace` 或改用 `overrides`。`--strict-ids` 默认 `false`，CI 可显式开启。

### 5.4 项目覆盖（不复制脚本）

```toml
[[rules_packs.overrides]]
id       = "gaussdb:GDB001"  # 规范 id 或裸 id 均可（裸 id 命中多条时按包优先级取最后一条）
enabled  = false             # 项目关掉它
severity = "error"           # 或改级别

[[rules_packs.overrides]]
id     = "DML014"
[rules_packs.overrides.params]
max_join_tables = 5      # 规范阈值按项目口径调整（复用 RuleConfig.params）
```

- `overrides` 只允许改 `enabled / severity / params`，**不允许**改 `script_path`（改脚本请用项目本地规则）。
- 未命中的 `overrides` id → 报错（typo 防护），列出可用 id 的前 N 个近似项。

---

## 六、锁文件 `sqlguard.lock`

### 6.1 目的

保证"同配置 → 同规则集 → 同结果"，让 gating 决策可复现。

### 6.2 格式

```toml
version = 1

[[pack]]
name     = "rules-core"
version  = "2.1.0"
source   = "path:vendor/rules/rules-core"
checksum = "sha256:9f2c…"           # 包清单 + 全部 *.rhai 的内容哈希

[[pack]]
name     = "rules-gaussdb"
version  = "1.3.0"
source   = "git+https://cnb.cool/mikezhu/rules-gaussdb@v1.3.0"  # 仅 B 方案使用
checksum = "sha256:4a71…"
api_version = 1                     # 冗余记录，便于审计
```

- **checksum 算法**：对包内按路径排序后的 `(相对路径, 内容)` 序列做 SHA-256，聚合为单一哈希（与 `cache.rs::dir_signature_recursive` 的排序思路一致，保证跨平台稳定）。
- 仅包含**内容哈希**，不含 mtime（mtime 不可复现）。
- `source` 语法本期**只实现 `path:`**；`git+` / `registry:` 为预留（解析时给"暂不支持该 source，本期仅支持 path:"的可读报错，而非视为格式错误）。

### 6.3 校验语义

| 场景 | 行为 |
|------|------|
| 无锁文件 | 正常检查；stderr 提示 `note: no sqlguard.lock; run 'sqlguard rules lock'` |
| 锁文件存在且匹配 | 正常检查 |
| **版本**不匹配 | **默认 error**，提示 `run 'sqlguard rules lock'` 更新；`--no-lock` 可临时跳过 |
| **checksum** 不匹配（版本未变却改了内容） | 默认**不检测**（见 §7 的 IO 折中）；`--locked` 时 error |
| `--locked` | 严格模式：锁缺失、版本不符、checksum 不符、锁有多余条目，全部 error（CI 推荐） |

> 选择"默认 error"而非 cargo 式的默认 warn，因为本工具的输出直接用于 **CI 阻断决策**，静默漂移比报错更危险。

---

## 七、缓存签名调整（必须做）

`cache.rs::compute_run_signature`（`cache.rs:196-237`）当前只纳入：版本、方言、编码、filter、主配置 mtime、`rules_file` mtime、`rules_dir` 下 `.rhai` 的 `(path, mtime, size)`。

外置/多包后必须追加：

- 每个生效包的 `name@version` + 清单 checksum；
- 每个包的解析后根路径（避免不同路径同名包互相污染缓存）；
- 引擎 `RULE_API_VERSION`。

否则会出现"换了规则包版本但缓存命中、复用旧 violations"的**静默错结果**。

> 权衡：checksum 每次全量读包内文件会有 IO 成本。折中——签名只用 `(pack name@version, 清单文件 mtime/size, 锁文件 mtime/size)`；checksum 的全量校验只在 `--locked` 与 `rules verify` 时做。

---

## 八、CLI 与 `init` 调整

### 8.1 新增 `sqlguard rules` 子命令

| 子命令 | 作用 |
|--------|------|
| `rules list` | 列出解析后的**生效规则**（规范 id / group / severity / enabled / 来源包）+ 包清单与兼容状态 |
| `rules add <name@version \| path>` | 追加到 `[rule_packs].packs`，并更新 `sqlguard.lock` |
| `rules vendor` | 把 `search_paths` 命中的包复制到项目 `vendor/rules/`（离线 & 可提交） |
| `rules lock` | 生成 / 更新 `sqlguard.lock` |
| `rules verify` | 校验 `api_version`、`engine` range、checksum；不执行检查，供 CI 前置步骤 |

### 8.2 `init` 降级

- 现状：`init` 写入全部内置规则脚本（`INIT_RULE_SCRIPTS`，`main.rs:1382`）。
- 目标：`init` 只写
  1. `sqlguard.toml` + `sqlguard.rules.toml`（示例，保持不变）；
  2. **最小内置包**（仅 helpers + 非 AST 轻量检查：`FILE001` / `FILE002` + parse-error 兜底）；
  3. 可选 `--with-default-pack`：把默认规则包 vendor 到 `vendor/rules/` 并生成锁文件。
- 兼容：`--force` 幂等语义（v0.2.5 起）不变；现有守护测试
  `init_rule_scripts_cover_example_declarations` / `default_contents_parse_as_toml` 需同步调整到"最小集 vs 默认包"。

---

## 九、安全

`*.rhai` 是**可执行代码**，规则包等于"可执行配置"，必须按代码对待：

| 风险 | 措施 |
|------|------|
| 脚本读写文件/网络 | 现状引擎**未注册**任何 IO 函数，Rhai 脚本天然无 FS/网络能力——保持这一约束，禁止后续为"方便"放开 |
| 死循环 / 资源耗尽 | `Engine` 配置 `max_operations` / `max_call_levels` / `max_expr_depth` / `max_array_size`，默认给出保守上限，可配置 |
| 包被篡改 | 锁文件 checksum（§六）；非 `path:` 来源强制要求 checksum |
| 供应链（B 方案） | 远程拉取需 checksum + 签名校验；`allow_remote` 默认 `false` |
| 目录逃逸 | 包内 `script_path` / `helpers` 禁止 `..` 逃逸包根（复用 `sanitize_relative_rules_path` 策略） |

---

## 十、迁移、兼容与测试

### 10.1 向后兼容承诺

| 现有用法 | 现状 | 本方案下 |
|----------|------|----------|
| 无 `[rule_packs]`，用 `sqlguard.rules.toml` | 生效 | **不变**（作为优先级 3 的本地层） |
| 内联 `[[rules]]` | 生效 | **不变** |
| `rules_file` 指向任意文件 | 生效 | **不变**；若文件带 `[pack]` 段则按包解析（可选增强） |
| `init` 产物 | 全部规则脚本 | **最小内置包**（helpers + 非 AST 轻量检查）；其余规则下沉默认包，`--with-default-pack` 可 vendor 默认包 |
| 规则 id / 报告中的 id | `DDL001` | **不变**（默认包不带 namespace）；仅带 namespace 的包使用 `<ns>:<id>` |
| `--rules/--groups/--exclude-*` | 生效 | **不变**；额外支持 `ns:id`（规范 id）与裸 id 两种写法（见 §5.3） |
| 缓存 | 生效 | 签名增加包维度（缓存可能整体失效一次，属预期） |

### 10.2 测试计划

**单元测试**
- `rules-pack.toml` 解析（含缺字段、非法 SemVer、`..` 逃逸）。
- `api_version` 兼容矩阵（等于/低于/高于引擎支持集）。
- `engine` SemVer range 判定（满足 → ok；不满足 → warn / `--strict-engine` → error）。
- 合并优先级（内置 < 包顺序 < 本地 < overrides）、跨包 id 冲突（默认覆盖 + `--strict-ids` 报错）。
- 规范 id 生成与匹配：`namespace` 有无 → `ns:id` / 裸 `id`；四种过滤写法（`ns:id` / 裸 id / `ns:*` / `DDL*`）命中集正确。
- `overrides` 未命中 id → 报错并给近似建议。
- checksum 计算稳定性（不同遍历顺序 / 跨平台路径分隔符 → 同哈希）。
- 锁文件：匹配 / 版本不符 / checksum 不符 / `--no-lock` / `--locked`。
- 缓存签名：换包版本 → 签名变化。

**集成测试**
- 多包项目端到端：两包 + 本地覆盖，`rules list` 输出与 `check` 结果符合预期。
- 离线场景：仅 `vendor/` 目录，`allow_remote=false` 下正常。
- 兼容性失败场景：包的 `api_version` 过高 → 退出码 2 + 可读报错（**不允许**静默跳过规则）。
- 回归守护：无 `[rule_packs]` 的既有 fixture 项目 → 报告逐字节不变。

### 10.3 分期落地

| 里程碑 | 内容 | 依赖 | 状态 |
|--------|------|------|------|
| **M1** | `RULE_API_VERSION` 常量 + 包兼容校验（先只用内联/`rules_file`，无新格式） | — | ✅ 已实现 |
| **M2** | 规则包格式 + 多包加载/合并/优先级 + `overrides` + namespace 生效 | M1 | ✅ 已实现 |
| **M3** | `rules` 子命令 + `sqlguard.lock`（缓存签名已在 M2 完成） | M2 | ✅ 已实现 |
| **M4** | `init --with-default-pack` + 默认规则包（`config/rules-pack.toml`）| M3 | ✅ 已实现（独立规则仓属组织工作） |
| **M5** | 文档（README / `docs/default-rules.md` / `docs/rule-scripting.md`）+ 迁移指南（§十二） | M4 | ✅ 已实现 |

M1 可独立先上，**只堵"规则静默失败"风险**，不改任何格式，风险最低。

### 10.4 M1 已落地内容（实现对照）

| 项 | 实现位置 | 说明 |
|----|----------|------|
| `RULE_API_VERSION` | `src/rule/pack.rs` | 当前值 `1`；语义为"引擎暴露给 Rhai 的 API 面"（见 §4.1） |
| `[pack]` 解析 | `src/rule/pack.rs::RulePackMeta` + `src/config.rs` | `Config.pack`；外置规则文件的 `[pack]` 优先于主配置内联 |
| `api_version` 硬校验 | `config.rs::Config::validate_rule_pack` | 高于引擎支持值 → `ConfigError`（附升级 hint），检查直接中止 |
| `engine` 范围校验 | `pack.rs::RulePackMeta::engine_mismatch` + `main.rs::check_rule_pack_compat` | 默认 warning；`check` / `check-diff` 加 `--strict-engine` → error。**M1 只诊断本地 `[pack]`**，M2 起由 `config.rs::rule_pack_engine_mismatches` 覆盖每个生效规则包（见 §10.5） |
| `namespace` | `pack.rs::namespace_error` | 仅校验字符集 `[a-z0-9_-]`；解析成功后提示"尚未生效"，id 仍为裸 id（**M2 起已生效**，见 §10.5） |
| SemVer 范围求值 | `pack.rs`（私有 `parse_engine_range`） | 仅支持 `>=` `>` `<=` `<` `=` 的 AND 组合（空白/逗号分隔）；`||` 明确报错 |

M1 未改动任何文件格式与规则 id，既有工程行为不变。已附单元测试 16 个
（`pack.rs` 10 + `config.rs` 6）；`cargo test` 全量通过（lib 654 / integration 33 / proptest 10）。

### 10.5 M2 已落地内容（实现对照）

| 项 | 实现位置 | 说明 |
|----|----------|------|
| 包清单格式 | `pack.rs::PackManifest` / `resolve_and_merge` | `rules-pack.toml` = `[pack]` + `[[rules]]`；`name` / `version` / `api_version` 必填，`engine` 可选（仅软告警） |
| 多包加载 | `pack.rs::locate_pack` / `collect_search_paths` | `[rule_packs].search_paths` + 环境变量 `SQLGUARD_RULE_PATH`（`;` 分隔）；包位于 `<search_path>/<name>/rules-pack.toml`，或用 `packs[].path` 直接指向包根 |
| 合并优先级 | `pack.rs::Layered` | 引擎内置兜底 < packs（声明顺序，靠后覆盖靠前）< 项目本地 < overrides；同 id 原地替换并记 `Note:` |
| namespace 生效 | `pack.rs::qualify_rule_id` | 规则 id 规范化为 `<ns>:<id>`；未设 namespace 保持裸 id；id 含 `:` 直接报错 |
| 规范 id 过滤 | `ast.rs::matches_rule_id` | `ns:id` / 裸 id / `ns:*` / 裸前缀 四种写法（§5.3 表） |
| overrides | `pack.rs::apply_overrides` | 只改 `enabled` / `severity` / `params`；裸 id 命中多条取最后一条；未命中报错并给出可用 id |
| 包内 helpers | `pack.rs::read_pack_file` + `runner.rs::run_single_rule` + `config.rs::pack_helpers_for` | 按包根前缀定位所属包，在**引擎 helpers 之后** prepend；禁止逃逸包根 |
| 目录逃逸拦截 | `pack.rs::resolve_pack_path` | `script_path` / `helpers` 不得 `..` 逃逸包根 |
| 缓存签名 | `cache.rs::compute_run_signature` | 追加 `pack=name@version` + 包内 rhai 递归签名 + 清单签名（原计划 M3；因 M2 一引入包就存在"改包不失效"的静默风险，提前到 M2） |
| 非致命提示 | `config.rs::rule_pack_notes` + `main.rs::check_rule_pack_compat` | 仅在 stderr 为 TTY 时输出，避免污染 CI 日志 |
| 包级 `engine` 诊断 | `ResolvedPack::engine` + `config.rs::rule_pack_engine_mismatches` | `ResolvedPack` 必须带出包声明的 `engine`，否则包清单里的范围声明形同虚设（曾漏：只诊断本地 `[pack]`，包声明 `>=9.0` 也不告警） |

M2 **未改动 `RuleConfig` 结构**（短 id 由 canonical id 按 `:` 反推），因此既有配置、报告、
`--rules` 用法在**不使用 `[rule_packs]` 与 namespace 时**完全不变。新增测试 18 个
（`pack.rs` 5 + `config.rs` 11 + `ast.rs` 2），`cargo test` 全量通过（lib 672 / integration 33 / proptest 10）；
并已用真实二进制对「包加载执行 + namespace 过滤 + 包内 helpers + overrides 生效 + 包缺失报错」
做过端到端验证。

> **实现期对 §3.2 的一处收窄**：包清单 `[pack]` 中 `engine` 改为**可选**——它只产生软告警，
> 强制必填会凭空增加包作者负担而不带来任何闸门；`api_version` 仍必填（它才是硬闸门）。

### 10.6 M3 已落地内容（实现对照）

| 项 | 实现位置 | 说明 |
|----|----------|------|
| 锁文件模型 | `src/rule/lock.rs`（`LockFile` / `LockedPack`） | TOML `[[pack]]`：`name` / `version` / `source` / `checksum` / `api_version` |
| checksum | `lock::pack_checksum` | `sha256:` + 包内**按路径排序**的 `相对路径\0长度\0内容`；跳过 `.git`/`.svn`/`.hg`；跨平台稳定 |
| 生成 / 更新 | `lock::build_lock` + `write_lock` | 带说明头注释，写 `<config_dir>/sqlguard.lock` |
| 校验 | `lock::verify_lock`（`LockMode::{Off,Auto,Strict}`） | Auto 只比对 name+version；Strict 另校验 checksum 并要求锁完整 |
| check 接入 | `main.rs::lock_mode` + `run_check` / `run_check_diff` | `--locked` → Strict；`--no-lock` → Off；默认 Auto（缺锁仅提示） |
| 子命令 | `main.rs::run_rules_subcommand` | `list` / `lock` / `verify` / `vendor` / `add` |
| 新依赖 | `Cargo.toml` 新增 `sha2 = "0.10"` | 本次唯一的直接新依赖（附带 `digest` / `block-buffer` 等传递依赖） |

> **对 §6.3 的实现折中**：默认（Auto）只比对 **version**，不做 checksum——全量 checksum
> 需读取包内所有文件，作为每次 `check` 的默认开销过重（§7 已给出该折中）。
> `--locked`（CI 严格模式）才做全量 checksum 与"锁完整性"校验。

### 10.7 M4/M5 已落地内容（实现对照）

> **2026-10-08 拆分更新（方案 A）**：默认规则按**基础类 / 定制类**拆成两个包——
> `config/rules-core/`（25 条通用规则，不设 namespace，保持裸 `DDL001` 等 id）与
> `config/rules-gaussdb/`（18 条 GaussDB 规范，`namespace = "gaussdb"`，包模式下 id 形如
> `gaussdb:GNAM001`）。`config/` 不再是单一包根，改为 `<search_path>` 下两个并列包目录。

| 项 | 实现位置 | 说明 |
|----|----------|------|
| 默认规则包 | `config/rules-core/rules-pack.toml` + `config/rules-gaussdb/rules-pack.toml` | 两个包均 `version = "1.0.0"`、`api_version = 1`；基础类不设 namespace，定制类设 `namespace = "gaussdb"` |
| 清单/示例防漂移 | `main.rs` 测试 `default_pack_manifest_matches_rules_example` | 两包清单合并后与 example 逐条比对 `(id, script_path, severity, enabled)`，并校验每个脚本存在于其所属包的内置脚本清单（`DEFAULT_PACKS`） |
| `init --with-default-pack` | `main.rs::run_init_with_default_pack` | 双包 vendor 到 `vendor/rules/<包名>/` + 向主配置**末尾追加** `[rule_packs]`（声明两个包）+ 生成 `sqlguard.lock`；与旧 `init` 互不影响 |
| 迁移指南 | 本文 §十二 + README「规则包迁移指南」 | — |
| 用户文档 | `README.md`（`rules` 子命令 / `sqlguard.lock` / 迁移）、`docs/default-rules.md`、`docs/rule-scripting.md` | — |

> **未在本仓库完成的部分**：把默认规则包抽到**独立规则仓**并接独立 CI/tag 属组织性工作
> （需要新仓库与账号权限）。本仓库已提供使其成立的全部前置条件：合规包根、
> `rules vendor`、锁文件、以及 §十二 的分步操作。
>
> **`init` 未做"降级"**：默认 `init` 仍写 `config/rules/` 全量脚本（保持向后兼容，既有测试与
> 用户工作流零变化）；`--with-default-pack` 是新增的包化路径。彻底移除二进制内嵌脚本会让
> "fresh init 即可用"失效（生成的 `sqlguard.rules.toml` 会指向不存在的脚本），故不做。

---

## 十一、决策记录

以下 6 项已于 2026-09-30 评审确认，作为实现依据：

| # | 议题 | 决策 | 落点 |
|---|------|------|------|
| 1 | id 命名空间 | **引入 `pack:ID`**。由 `[pack].namespace` 控制；默认包不带 namespace，保持现有 `DDL001` 等 id 不变 | §3.2 / §5.3 |
| 2 | 锁文件默认严格度 | **默认 error**（不匹配即阻断），`--no-lock` 临时跳过，`--locked` 为 CI 严格模式 | §6.3 |
| 3 | `engine` range 不满足 | **默认 warning**，`--strict-engine` 升级为 error | §4.1 |
| 4 | `api_version` 粒度 | **整体单号**（单调递增的整数） | §4.1 |
| 5 | 引擎内置兜底范围 | **仅 helpers + 非 AST 轻量检查**（`FILE001` / `FILE002` + parse-error 兜底），其余规则全部下沉到默认包 | §5.2 / §8.2 |
| 6 | 远程 source 语法 | **预留** `git+` / `registry:` 语法，本期只实现 `path:` | §6.2 |

### 小项确认（2026-09-30）

- `namespace` 字符集：`[a-z0-9_-]+`，禁止 `:` 与空白 —— M1 已按此实现校验。
- `--strict-ids` 默认值：`false`（CI 可显式开启）—— M2 实现。
- 默认包正式名：`rules-core` —— M2/M4 使用（`init --with-default-pack` 的 vendor 目录名随之确定）。

---

## 十二、迁移指南

> 面向用户的精简版见 `README.md`「规则包迁移指南」；本节给出与代码一一对应的操作与约束。

### 12.1 零改动路径

不配置 `[rule_packs]` 时，`resolve_and_merge` 仅在本地 `[pack]` 声明了 `namespace` 时改写
规则 id，其余与合并前完全等价。既有工程升级后行为不变（见 §10.1 / §10.2）。

### 12.2 迁移到规则包

```bash
sqlguard init . --with-default-pack   # vendor 默认包 + [rule_packs] + sqlguard.lock
sqlguard rules list                   # 核对生效规则与来源
sqlguard rules lock                   # 固定版本（提交 sqlguard.lock）
sqlguard check . --locked             # CI：强制锁一致 + checksum
```

- **升级包版本** = 改 `[rule_packs].packs[].version` → `rules lock`；未更新锁文件会被直接拒绝（版本不符即 error）。
- **项目自定义规则**继续写在 `sqlguard.rules.toml`（优先级最高，可覆盖包内同 id 规则）。
- **只改级别 / 阈值 / 开关**用 `[[rule_packs.overrides]]`（不复制脚本）；引用未知 id 报错防 typo。
- **同名不同义**：给包设 `namespace` → id 变 `<ns>:<id>`；过滤支持 `ns:id` / 裸 id / `ns:*` / 裸前缀（§5.3）。
- **CI 严格模式**：`--locked`（锁 + checksum）、`--strict-engine`（引擎版本不满足即失败）、`--strict-ids`（跨来源同 id 冲突即失败）。

### 12.3 抽取独立规则仓库

默认规则已按基础类/定制类拆成两个包根：`config/rules-core/`（含 `rules-pack.toml` +
`rules/**` + `lib/helpers.rhai`）与 `config/rules-gaussdb/`（含 `rules-pack.toml` +
`rules/**`）。方案 A（monorepo 多包 + 按包打 tag）的抽仓步骤：

1. 新仓库放 `packs/rules-core/` 与 `packs/rules-gaussdb/`，各自保留包根结构；基础包的
   `lib/helpers.rhai` **不要**声明为 `[pack].helpers`——该 helpers 由引擎内置注入；
   定制包（及其他团队包）自带 `namespace`（如 `gaussdb`）隔离规则 id；
2. 该仓库按包打 tag（如 `rules-core/v1.2.0`、`rules-gaussdb/v1.0.0`）并接 CI；包内只需
   维护 `version` / `api_version` / `engine`；
3. 消费侧本期用 `path`（`rules vendor` / submodule / CI 检出）；`git+` / `registry:` 待后续里程碑；
4. 引擎改动若触及规则可见 API，**必须**递增 `RULE_API_VERSION`，否则新包会在旧引擎上静默失效——这正是本方案存在的根本原因。

### 12.4 回退

`rules-pack.toml` 与锁文件都是纯附加物：删掉 `[rule_packs]` 段、恢复 `sqlguard.rules.toml`
即可回到脚本布局；`--no-lock` 可临时跳过锁校验。无数据迁移，回退成本为零。