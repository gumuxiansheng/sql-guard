# replay-export 增量导出 — 需求说明与技术方案

> 状态：**已实现（M1-M5 全部完成）**
> 日期：2026-08-02
> 作者：SqlGuard Team
> 关联模块：`src/replay_export.rs`、`src/git_diff.rs`、`src/cli.rs`、`src/main.rs`、`replay/`（Java 消费侧）
> 版本目标：不破坏现有 `sql-manifest.json` v1 格式与 Java 侧兼容性

## 实现状态

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| **M1** | `git_diff` 扩展：hunk 新旧侧范围（`FileDiff.old_hunks`）+ `git_show(base, path)` | ✅ 完成 |
| **M2** | `replay_export` 增量构建：`build_incremental_manifest` + removed 检测 + `change` 标记 | ✅ 完成 |
| **M3** | CLI `--base` 接线：`sql-manifest.json` + `sql-manifest-removed.json` 输出 | ✅ 完成 |
| **M4** | 端到端集成测试：临时 git 仓库覆盖 修改/删除/新增/无改动 场景 | ✅ 完成 |
| **M5** | README / replay 侧文档更新 | ✅ 完成 |

**测试覆盖**：`git_diff` 11 个单元测试（旧侧 hunk、纯删除/纯新增、上下文含 `-`/`+`）、
`replay_export` 40 个单元测试（hunk 过滤、mapper 标签过滤、removed 检测与 id 排除、
类型组合、清单序列化）、2 个端到端集成测试（改动场景 + 无改动场景）。

**实现要点**：removed 检测对「修改语句」做 id 排除（旧 id 仍存在于新清单 → 是修改不是
删除）；脚本语句 id 保持与全量导出一致（序号按全部语句计数）；`--base` 缺省时行为与
旧版逐字节一致。

---

## 一、背景与目标

`replay-export` 当前为**全量导出**：每次扫描全部 SQL 脚本与 Mapper XML，重建完整
`sql-manifest.json` 并整体覆盖输出（`src/main.rs:545-561`）。在大工程 / 镜像库重放
场景下存在三个痛点：

1. **导出耗时**：几千个文件每次全量重解析（`parse_sql_to_ast` + Mapper 动态分支展开）。
2. **重放耗时**：Java 侧对全量清单逐条 EXPLAIN/计时，PR 迭代中大量语句重复重放。
3. **回归定位**：全量清单没有"本次改了哪些语句"的语义，慢 SQL 报告无法与 PR 改动关联。

**本次目标**：为 `replay-export` 增加**可选的增量导出**能力，CI 中只导出/重放本次
改动的语句，同时保持默认行为（无增量参数时）与现状完全一致、零回归。

---

## 二、现状分析

### 2.1 replay_export 结构（现状）

| 组件 | 现状 | 与增量的关系 |
|------|------|--------------|
| `build_manifest` | 全量输入 `(target_dir, sql_files, mapper_files, type_filter)` → 全量 `Manifest` | 纯函数，无副作用，容易扩展行级过滤参数 |
| 语句 id | 脚本 `<source>#<seq>`（per-file 序号，注释明确"保证前面文件增删不会平移后续编号"）；mapper `<source>#<statement_id>[#vN]` | mapper id 稳定；脚本 seq 在**文件内语句重排**时漂移 |
| 语句行号 | 脚本用 `stmt.line / effective_end`；mapper 用 `raw_xml_line`（标签起始行） | 与 git hunk 做交集过滤的锚点 |
| 事务控制语句 | 不导出（`replay_type` 返回 None） | 与 hunk 过滤无冲突 |

### 2.2 已存在的基础设施（可直接复用）

`check-diff` 子命令已实现 git baseline 增量校验，核心组件：

- `git_diff::get_diff(base, patterns)`（`src/git_diff.rs:34`）：调用
  `git diff --unified=0 --diff-filter=d <base>...HEAD`，输出每个改动文件的
  `hunks: Vec<(start, end)>` 与 `is_new` 标记。
- 语句级交集过滤：`[line, end_line] ∩ hunk != ∅` 则保留（`src/main.rs:769-791`）。
- 路径模式限制：`*.sql / *.ddl / *.dml` + mapper 配置中的 patterns。

**注意**：现有 `parse_hunk_header`（`src/git_diff.rs:115`）**只解析新侧**
（`+new_start,new_len`），删除侧（`-old_start,old_len`）被忽略。增量导出要识别
"被删除的语句"，必须扩展解析旧侧行范围（见 4.2）。

### 2.3 Java 消费侧（现状）

- `ManifestLoader` 校验 `version == 1`（`ManifestLoader.java:21`）。
- `Manifest` / `ManifestStatement` 均标注 `@JsonIgnoreProperties(ignoreUnknown = true)`
  （`Manifest.java:11`、`ManifestStatement.java:12`），**未知字段静默忽略**。
- → 增量字段以**可选字段**追加、`version` 保持 1 即可做到消费侧零改动兼容。

---

## 三、可行性判断

**结论：完全可行，且成本低。** 理由：

1. **git 增量通路已存在**：`check-diff` 已打通 `git diff` → hunk → 语句级过滤的完整链路，
   `replay-export` 只需复用同一套 `git_diff` 模块，新增的参数形态与 `--base` 与
   `check-diff` 一致，用户心智负担低。
2. **`build_manifest` 是纯函数**：只需增加"文件白名单 + 行范围过滤"两个输入参数，
   内部逻辑（解析、展开、序列化）零改动，风险面小。
3. **格式向后兼容有保障**：Java 侧已容忍未知字段，`version` 保持 1 时旧版
   `sqlguard-replay` 可直接消费增量清单（只重放子集，行为合法）。
4. **语句级锚点完备**：脚本语句有 `[line, end_line]`，mapper 标签有起始行，均能
   与 hunk 求交集。

**唯一需要新增的基础能力**：`git_diff` 解析删除侧 hunk（为识别被删语句），以及
`git show <base>:<path>` 读取旧文件内容。均为纯增量改动，不影响 `check-diff`。

---

## 四、方案设计

### 4.1 三种候选方案对比

| 维度 | 方案 A：git baseline（`--base`） | 方案 B：旧清单 diff 合并 | 方案 C：mtime 状态文件 |
|------|----------------------------------|--------------------------|------------------------|
| 原理 | `git diff <base>...HEAD` → hunk → 行级过滤，只导出改动语句 | 全量重建后与旧 `sql-manifest.json` 按 id 求差集 | 记录文件 mtime/hash，只重扫变化文件 |
| 依赖 | 必须 git 仓库 | 无 git 依赖 | 无 git 依赖 |
| 新增/修改识别 | ✅ hunk 直接定位 | ✅ 靠 id 匹配 | ✅ 靠重扫 |
| **删除识别** | ✅ 旧侧 hunk + `git show` 旧文件 | ✅ 旧清单差集 | ⚠️ 需文件列表对比，无法知被删语句 |
| 解析成本下降 | ✅ 只解析改动文件 | ❌ 仍全量解析 | ✅ 只解析变化文件 |
| **id 稳定性要求** | 无（不跨版本匹配 id） | **高**：脚本 seq 在语句重排时漂移，误判大量 change | 高：seq 漂移影响合并结果 |
| 实现复杂度 | 低（复用 git_diff） | 中（diff 算法 + 稳定性补救） | 高（状态文件格式、并发、清理） |
| 适用场景 | **CI / PR（推荐）** | 非 git 工程、发布平台归档 | 本地大工程迭代 |

### 4.2 推荐方案：A（git baseline 增量，`--base` 参数）

与 `check-diff` 完全同构，符合项目既有模式。核心流程：

```
sqlguard replay-export ./sql -o manifest_out/ --base origin/main
│
├─ 1. git diff --unified=0 origin/main...HEAD（复用 git_diff::get_diff）
│      输出：改动文件 + 新侧 hunk [s,e]（新增/修改）+ 旧侧 hunk [s_old,e_old]（删除）
│
├─ 2. 对每个改动文件（is_new 整文件算改动，跳过 hunk 过滤）：
│      a. SQL 脚本：parse_sql_to_ast → 语句 [line, end_line] ∩ 新侧 hunk → 导出为 added/modified
│      b. Mapper XML：标签起始行 raw_xml_line ∩ 新侧 hunk → 导出该标签全部动态分支变体
│
├─ 3. 删除识别（仅对 base 中存在的文件）：
│      a. git show <base>:<path> 取旧文件内容
│      b. 解析旧文件 → 语句 [line, end_line] ∩ 旧侧 hunk → 标 removed
│
└─ 4. 输出：
      ├─ sql-manifest.json        仅含 added/modified 语句（增量清单）
      └─ sql-manifest-removed.json 仅含 removed 语句元信息（id/source/line，无 sql 文本）
```

**关键语义决策**：

| 决策点 | 规则 | 理由 |
|--------|------|------|
| 部分修改语句 | 语句与 hunk 有交集即整条导出（与 `check-diff` 过滤语义一致） | 静态层面无法精确到语句内片段，整条重放保证计划真实 |
| Mapper 变体 | 标签起始行命中 hunk 即导出该标签**全部**变体 | 变体是导出期由 `<if>/<foreach>` 展开生成的，无源行，无法按变体过滤；且条件分支改动影响所有变体 |
| 纯删除文件 | `--diff-filter=d` 排除 | 与 `check-diff` 一致；文件级删除由 CI 管道清理归档 |
| `--types` 组合 | 在 hunk 过滤之后再按类型过滤 | 语义：本次改动中类型命中的语句 |
| 未提交改动 | 只覆盖已提交内容（`base...HEAD`） | 与 `check-diff` 一致，CI 场景成立 |
| removed 的 `sql` 字段 | 置空，仅保留 id/source/line 元信息 | 被删语句不重放，无重放价值；重放侧只用于从历史报告剔除 |

### 4.3 manifest 格式演进（向后兼容）

`version` **保持 1**，全部增量信息走可选字段：

```json
{
  "version": 1,
  "generator": "sqlguard replay-export",
  "generated_at": "1784909525",
  "base": "origin/main",            // 新增（可选）：增量模式才出现，记录 git 基线
  "incremental": true,               // 新增（可选）：false/缺省 = 全量导出
  "statement_count": 3,
  "statements": [
    {
      "id": "sql/dml/query.sql#1",
      "sql": "SELECT id, name FROM users WHERE id = ?",
      "type": "select",
      "source": "sql/dml/query.sql",
      "source_type": "sql",
      "line": 1,
      "end_line": 1,
      "change": "modified"           // 新增（可选）：added / modified；全量导出缺省
    }
  ]
}
```

- 顶层 `base` / `incremental`、语句级 `change` 均为**可选**，旧版 Java 侧
  `@JsonIgnoreProperties(ignoreUnknown = true)` 直接忽略，零改动可用。
- 新增独立文件 `sql-manifest-removed.json`（顶层含 `version: 1`、`base`、`removed` 数组），
  不进入主清单，避免污染重放器主流程。
- 若未来需要 Java 侧感知 `change` 语义（如报告里标注"本次改动"），为 `ManifestStatement`
  增加可选字段即可，仍无需升 version。

### 4.4 CLI 设计

```
sqlguard replay-export [PATH] [OPTIONS]

新增选项：
  --base <GIT-REF>   增量导出：只导出自该 git 基线以来的新增/修改语句，
                     并生成 sql-manifest-removed.json（被删语句）。
                     缺省 = 全量导出（现状行为，零回归）

现有选项保持不变：-c/--config、-o/--output-dir、--types
```

- 传 `--base` 且当前目录非 git 仓库 / `git diff` 失败 → 报错退出（与 `check-diff` 行为一致），
  不静默回退全量（静默回退会掩盖 CI 配置错误）。
- 无改动时：主清单 `statement_count: 0`，removed 文件照常生成，正常退出码 0。

---

## 五、实现计划

| 里程碑 | 内容 | 涉及文件 |
|--------|------|----------|
| **M1** | `git_diff` 扩展：hunk 同时解析旧侧 `-old_start,old_len`，`FileDiff` 增加 `old_hunks`；新增 `git_show(base, path)` 读旧文件内容；单元测试覆盖纯删除/混合 hunk | `src/git_diff.rs` |
| **M2** | `replay_export` 增加增量入口：`build_incremental_manifest(target, sql_files, mapper_files, diffs, type_filter)` —— 按文件白名单 + hunk 交集过滤，复用现有 `build_manifest` 的语句构建逻辑（抽公共函数），输出 `change` 标记；removed 检测（旧文件解析 + 旧侧 hunk 反向过滤） | `src/replay_export.rs` |
| **M3** | 主命令接线：CLI 解析 `--base`；改动文件集合与 `collect_sql_files / collect_mapper_files` 求交集（防止 git 路径与配置路径不一致）；写 `sql-manifest.json` + `sql-manifest-removed.json`；打印统计 | `src/cli.rs`、`src/main.rs` |
| **M4** | 端到端集成测试：临时 git 仓库（init + commit + 修改 + commit），覆盖新增文件 / 修改语句 / 删除语句 / 纯删除文件 / `--types` 组合 / 无改动；`check-diff` 现有测试回归 | `tests/integration_test.rs` |
| **M5** | 文档：README `replay-export` 小节 + `replay/README.md` 增量消费说明（removed 归档语义） | `README.md`、`replay/README.md` |

**测试覆盖**：M1 约 8-10 个 `git_diff` 单元测试；M2 约 10-15 个 `replay_export`
单元测试（hunk 过滤、mapper 标签过滤、removed 检测、类型组合）；M4 约 4-6 个端到端用例。

---

## 六、兼容性与回归

| 影响面 | 结论 |
|--------|------|
| 全量导出行为 | `--base` 缺省时代码路径不变，输出字节级一致 |
| 清单格式 | v1 不变，新增字段全部可选；Java 侧 `ignoreUnknown` 容忍 |
| `check-diff` | `git_diff` 的 hunk 结构扩展为增量字段，现有解析/过滤逻辑不受影响（新增字段不影响旧字段）；集成测试回归 |
| 新字段在报告链路 | 不感知 `change` 字段 → 增量清单被当作全量子集重放，行为合法 |
| removed 语句 | 独立文件承载，主清单消费者不受影响 |

---

## 七、验收标准

1. `sqlguard replay-export ./sql -o out/`（无 `--base`）：输出与改动前**逐字节一致**。
2. 临时 git 仓库场景：修改 1 条语句 → 增量清单仅含该语句且 `change: "modified"`；
   新增文件 → 整文件导出 `change: "added"`；删除 1 条语句 → 主清单不含该语句，
   `sql-manifest-removed.json` 含其 id。
3. Mapper：仅改动某 `<select>` 标签体 → 只导出该标签（含全部变体），其他标签不出现。
4. `--types select` 与 `--base` 组合：只导出改动中的 select。
5. Java 侧加载增量清单（含 `base`/`incremental`/`change` 字段）不报错、可正常重放。
6. `cargo test` 全量通过（含 `check-diff` 既有集成测试零修改）。

---

## 八、风险与限制

| 风险 | 等级 | 缓解 |
|------|------|------|
| 语句部分修改导致整条重放 | 低 | 与 `check-diff` 语义一致；静态无法细分，重放成本 ≤ 全量 |
| mapper 标签 hunk 命中粒度 | 低 | 标签起始行即锚点；改 `WHERE` 内多行也只命中一次 |
| `git show` 失败（base 中文件缺失/二进制） | 低 | 跳过该文件 removed 检测 + stderr 警告，不阻塞主流程 |
| 非 git 工程 | 中 | 明确报错提示；方案 B/C 留作后续需求 |
| 脚本 seq 漂移 | 不适用 | git 方案不做跨版本 id 匹配，天然免疫；方案 B 才受限 |
| 大型历史 base 全量重放 | 信息 | 首次基线需全量，之后 CI 每次只增量，符合预期 |

---

## 九、后续演进（本次不做）

- **方案 B（非 git 增量）**：`--diff-old-manifest <path>` 与旧清单按稳定 id 求差集；
  需先解决脚本语句 id 稳定性（改为按 SQL 文本 hash 或引入指纹字段），涉及格式演进。
- **重放侧增量消费**：`sqlguard-replay` 读取 `change` 字段在报告中标注"本次改动"、
  结合 `sql-manifest-removed.json` 从历史报告剔除已删语句。
- **`--allow-dirty`**：增量基线放宽到工作区（`git diff` 而非 `base...HEAD`），
  覆盖未提交改动的本地场景。
