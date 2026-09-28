# SqlGuard CNB 多平台构建并行化改造方案

> 目标：将当前 `.cnb.yml` 中「单 stage 串行构建 5 个目标」改造为并行构建，压缩 tag 发布的端到端耗时。
> 参考对象：`C:/Dev/Projects/java-guard/.cnb.yml`（本组织内已落地、已验证的并行构建范式）。

---

## 一、现状分析（串行，证据先行）

当前 `C:/Dev/Projects/sql-guard/.cnb.yml`（`tag_push` 触发 `v*`）只有 **4 个串行 stage**：

| Stage | 内容 | 耗时性质 |
|-------|------|----------|
| 创建 Release | `type: git:release` | 秒级 |
| **构建多平台二进制** | `bash scripts/build-release.sh`（`timeout: 60m`） | **瓶颈：5 个目标在此脚本内 for 循环串行** |
| 组装发布包 | `bash scripts/package-release.sh` | 分钟级 |
| 上传 Release 附件 | `cnbcool/attachments` | 秒级 |

瓶颈在 `scripts/build-release.sh` 第 127–190 行的 `for T in "${TARGETS[@]}"` 循环：5 个目标
（x86_64/aarch64 musl、x86_64 windows-gnu、x86_64/aarch64 apple-darwin）**逐个 `cargo build --target`**，
且每个目标都要跑 `cp + strip`。整段被一个 `script` 包住，总耗时 ≈ 5 个目标构建时长之和。

另外，环境准备（`apt-get install` 交叉工具链、装 Zig/cargo-zigbuild、rustup target add、写 Zig CC 封装脚本）
也全部塞在这一个 stage 开头（build-release.sh 41–123 行），进一步拉长单 stage 时间，并逼近 `timeout: 60m`。

产物落盘约定（build-release.sh:179）：`dist/${B}-${T}${BINEXT}`，例 `dist/sqlguard-x86_64-unknown-linux-musl`。
`package-release.sh:95` 已把 `dist/<bin>-<triple>` 列为二进制来源优先级 #2，故「扁平命名」是既成契约。

---

## 二、java-guard 并行构建参考实现（已验证）

`C:/Dev/Projects/java-guard/.cnb.yml` 的核心是：**在 `tag_push:` 下挂多个 pipeline 条目，CNB 让它们并行执行**。
每个 pipeline 自包含（自己装依赖、自己构建、自己上传），互不共享容器文件系统：

```yaml
$:
  tag_push:
    - name: release-linux-amd64      # Pipeline 1（并行）
      runner: { tags: cnb:arch:amd64 }
      docker: { image: rust:latest }
      stages:
        - name: create release        # git:release（每个 pipeline 各自建？见 §四 注意①）
          type: git:release
          ...
        - name: install java deps ...
        - name: build rust binary
          env: { CARGO_TARGET_DIR: ./target-release }
          script:
            - rustup target add x86_64-unknown-linux-musl
            - cargo build --release --target x86_64-unknown-linux-musl
            - cp ./target-release/x86_64-unknown-linux-musl/release/java-guard ./dist/java-guard-linux-amd64
        - name: upload release attachment
          image: cnbcool/attachments:latest
          settings: { attachments: ["./dist/java-guard-linux-amd64"] }
    - name: release-linux-arm64      # Pipeline 2（并行）
      ...
    - name: release-windows-amd64    # Pipeline 3（并行）
      ...
```

**可借鉴的三点：**
1. **平台维度拆 pipeline，CNB 自动并行**——无需手写调度。
2. **每个目标产出用「平台专属文件名」写入共享 `./dist/`**（java-guard-linux-amd64 / -linux-arm64 / -windows-amd64.exe），靠文件名区分避免覆盖。
3. **每个 pipeline 自包含**：环境安装、构建、上传全在自身 stages 内完成——因为 CNB 的 job/容器是隔离的，前一个 stage 的 `/usr/local/bin` 变更不会自动带到下一个并行 job。

---

## 三、CNB 并发模型（改造前必须清楚的 4 个事实）

| # | 事实 | 来源 / 推断 | 对改造的影响 |
|---|------|------------|--------------|
| 1 | **同 trigger 下的多个 pipeline 条目并行执行** | java-guard 3 个 pipeline 即实证 | 主推「多 pipeline 并行」路线 |
| 2 | **pipeline 内 stages 串行** | CNB 文档/llama.cpp issue | 顺序类步骤（建 Release→打包→上传）仍串行 |
| 3 | **同 stage 内 jobs 写成「对象形式」并行，数组形式串行** | CNB 文档 | 次选「单 pipeline + 对象形式 jobs」路线 |
| 4 | **每个 job 在独立 Docker 容器运行，文件系统隔离（copy-on-write）** | CNB 文档「独立的 Docker 容器」+ OverlayFS COW | ① 前一个 stage 的 apt/zig 安装**不会**自动传给并行 job；② 并行 job 写入 `dist/` 的变更默认**不互相可见、也不保证传到下一 stage**，必须用 artifacts 显式传递 |

> ⚠️ 第 4 点是本方案最大的坑：sql-guard 当前依赖「5 个目标产物最终都落在同一 `dist/` 供 `package-release.sh` 汇总」，
> 而并行隔离会切断这条隐含的数据流。下面 §五给出两条可行路线及各自解法。

---

## 四、改造方案（推荐 + 备选）

### 路线 A（推荐，最贴合 java-guard）：多 pipeline 并行 + 末段汇总 pipeline

把 5 个目标各拆成 1 个自包含 pipeline（build+upload 自身二进制），再补 1 个 **`release-package`** pipeline：
从刚建好的 Release 下载 5 个平台二进制 → 跑 `package-release.sh` 组装 `-all` 合包 + 各平台独立包 + SHA256SUMS → 上传压缩包。

```
$:
  tag_push:
    - name: create-release          # 仅此一个 pipeline 负责建 Release（避免 6 个 pipeline 各建一次）
      docker: { image: rust:1.80 }
      stages:
        - name: 创建 Release
          type: git:release
          options: { overlying: true, title: ${CNB_BRANCH}, description: "SqlGuard ${CNB_BRANCH}" }
    - name: build-linux-amd64       # 以下 5 个并行
      runner: { tags: cnb:arch:amd64, cpus: 4 }
      docker: { image: rust:1.80 }
      stages: [ ... 装 zig/mingw + rustup target + build x86_64-unknown-linux-musl + upload dist/sqlguard-*-x86_64-unknown-linux-musl ... ]
    - name: build-linux-arm64       # 并行
      ...
    - name: build-windows-amd64     # 并行
      ...
    - name: build-macos-amd64       # 并行（best-effort）
      ...
    - name: build-macos-arm64       # 并行（best-effort）
      ...
    - name: release-package         # 末段：依赖前述构建完成（通过 Release 附件已存在来保证时序）
      docker: { image: rust:1.80 }
      stages:
        - name: 下载各平台二进制
          script: [ "cnb ... download attachments of ${CNB_BRANCH} -> dist/" ]   # 见注意②
        - name: 组装发布包
          script: [ "bash scripts/package-release.sh" ]
        - name: 上传压缩包
          image: cnbcool/attachments:latest
          settings: { tag: ${CNB_BRANCH}, attachments: ["./dist/*.tar.gz","./dist/*.zip","./dist/SHA256SUMS"] }
```

**关键注意：**
- **① 建 Release 只放一次**：java-guard 是 3 个 pipeline 各自建 Release（`overlying: true` 幂等覆盖），但 sql-guard 若 6 个 pipeline 都建会互相覆盖/竞争。建议抽成独立的 `create-release` pipeline，其余只负责构建与上传二进制。
- **② 末段如何拿到 5 个二进制**：两条子路径
  - **A2a（推荐）**：`release-package` pipeline 用 CNB 附件下载接口（cnbcool 附件插件或 `cnb` CLI）把 `${CNB_BRANCH}` 下 5 个 `sqlguard-*-<triple>` 拉回本地 `dist/`，再 `package-release.sh`。**需先验证附件下载 API 可用**（这是路线 A 唯一待确认点）。
  - **A2b（零外部依赖）**：放弃「单 `-all` 合包」，改为 5 个 pipeline 各自 `package-release.sh` 产出**本平台**包后直接上传；`-all` 合包与 SHA256SUMS 降级为「可选/本地生成」。改动最小、最稳，但改变了发布物形态（无统一合包）。

### 路线 B（备选，单一 pipeline 内并行 jobs）：需先验证 artifacts 透传

单 pipeline：`stages` = [prepare(串行装环境) → build(对象形式 5 jobs 并行) → package(串行) → upload(串行)]。
每个 build job 构建 1 目标并 `cp` 到 `dist/sqlguard-*-<triple>`，并声明 `artifacts: paths: ["dist/sqlguard-*-<triple>"]`，
由 artifacts 机制把产物透传给 package stage。

**风险**：CNB 是否支持「job 间 / stage 间 artifacts 透传」以及并行 job 的 COW 隔离细节，本方案未从官方文档 100% 确认
（官方文档页返回 404）。**落地前必须用 2 目标最小样例 dry-run 验证；验证不通过则回退路线 A。**
此外，由于 job 容器从基础镜像起、不带 prepare 阶段的 apt/zig 安装，每个 build job 需**自带环境安装**（隔离安全、无 dpkg 锁冲突，但重复安装；用缓存或预置镜像可缓解）。

---

## 五、任务拆分策略

以路线 A 为例，每个 `build-*` pipeline 内部 stages：

| Stage | 职责 | 串行/并行 |
|-------|------|-----------|
| install-cross-tools | `apt-get install` mingw / binutils-aarch64 + 装 Zig + cargo-zigbuild + rustup target add | pipeline 内串行（仅本平台所需子集） |
| build | `cargo build --release --target <triple>`（macOS 走 `cargo zigbuild`） | 单 job |
| stage-binary | `cp target/<triple>/release/sqlguard* dist/sqlguard-*-<triple>`（+ strip，best-effort） | 单 job |
| upload | `cnbcool/attachments` 上传本平台扁平二进制 | 单 job |

`create-release` 与 `release-package` 为独立 pipeline，夹在并行构建前后。

---

## 六、需要修改的配置项清单

| 配置位置 | 修改项 | 说明 |
|----------|--------|------|
| `.cnb.yml` | 原「构建多平台二进制」单 stage 删除 | 不再用 `build-release.sh` 单脚本串 5 目标 |
| `.cnb.yml` | 新增 5 个 `build-*` pipeline 条目 | 并行构建各目标（路线 A） |
| `.cnb.yml` | 新增 `create-release` pipeline（仅建 Release） | 避免多 pipeline 重复建 Release |
| `.cnb.yml` | 新增 `release-package` pipeline | 汇总 + 装包 + 上传压缩包（路线 A2a） |
| `.cnb.yml` | 各 pipeline `runner.cpus` 上调 | 当前根级 `cpus: 4`；并行需更多核才真提速（见 §七③） |
| `.cnb.yml` | 镜像版本锁定 `rust:1.80` | 现状已是；多 pipeline 需保持一致 |
| `scripts/build-release.sh` | 拆出 `scripts/build-one.sh <triple>` | 单目标构建逻辑复用，供各 pipeline 调用 |
| `scripts/package-release.sh` | 确认 `dist/<bin>-<triple>` 来源优先级 #2 够用 | 已支持，基本无需改；若走 A2b 合包取消则改上传清单 |
| `package-release.sh` 陈旧检测 | 并行产物 mtime 应为新构建时间，不会被误判陈旧 | 正常；但若末段下载回来，mtime 较新，OK |

---

## 七、并行环境下的产物收集与依赖隔离注意事项（重点）

### ① 产物命名隔离（防覆盖）
所有并行产出统一写入 `dist/sqlguard-*-<triple>` / `dist/sqlguard-mine-*-<triple>` 这类**三元组专属文件名**
（沿用 build-release.sh:179 约定）。即使各 pipeline 共享同一 `dist/` 逻辑路径，因文件名不同不会互相覆盖。
`package-release.sh` 已按此命名收集，契约一致。

### ② 文件系统 / 容器隔离 → 用显式 handoff
CNB 并行 job/pipeline 是 COW 隔离的，前一个 stage 的 `/usr/local/bin/zig`、`rustup target` 等**不会**自动出现在并行 job 中。
→ 解决：每个 `build-*` pipeline **自带完整环境安装**（装 Zig、cargo-zigbuild、mingw、rustup target add、写 Zig CC 封装脚本），
自包含、不依赖其他 pipeline 的副作用。代价是重复安装，可用预置镜像或 CNB 缓存缓解。

### ③ CPU 资源：并行 ≠ 自动更快
当前根级 `cpus: 4`。若 5 个 `cargo build` 真并行却只分到 4 核，单个构建变慢、墙钟时间 ≈ 最慢目标时长（而非 5 倍和）。
→ 把各 `build-*` 的 `runner.cpus` 调到 8/16，让并行编译有真实算力；同时单目标 `timeout` 可下调（如 `30m`）。

### ④ Cargo Registry 缓存并发竞争
多个并行 cargo 同时下载/编译同一批 crate 时，若共享 `CARGO_HOME` 会走 cargo 的 registry 文件锁（安全但可能串行化下载）；
若各自独立 `CARGO_HOME` 则无竞争但失去缓存复用、5 倍网络下载。
→ 建议：用 CNB 缓存机制持久化 `~/.cargo/registry`（cargo 自带文件锁可协调并发），并在每个 `build-*` 开头加
`cargo fetch --target <triple>` 预热（fetch 是只读拉取，冲突风险低）。这是性能调优点，正确性不受影响。

### ⑤ Zig / cargo-zigbuild 安装竞争
Zig 装在 `/tmp/ziglang-wheel`、cargo-zigbuild 解到 `/tmp`。因各 pipeline 容器隔离，`/tmp` 互不可见，**无竞争**；
但 macOS 两个目标都依赖 Zig，各自装一份即可（best-effort，失败仅跳过该目标，不影响其他 4 个平台）。

### ⑥ Watchdog「10 分钟无输出即 kill」
现状 build-release.sh 已用 `--progress-bar` 规避。并行后**每个** build job 都必须在下载/编译时持续输出进度，
否则单个 job 被 kill 不会拖垮兄弟 job，但会少一个平台产物。→ 复用现有 `--progress-bar`/`--max-time` 写法。

### ⑦ 失败隔离（单平台失败不阻断整体）
路线 A 天然隔离：某个 `build-*` 失败只缺一个平台附件；`release-package` 在 assemble 时应对缺失平台**降级跳过**
（而非整体失败），并在 SHA256SUMS / VERSION 中标注实际产出平台，保持发布可用性。

### ⑧ 产物时序：末段必须等构建完成
路线 A2a 的 `release-package` 依赖 5 个附件已上传。CNB 同一 trigger 下多 pipeline 并行**无声明式依赖（needs）**，
→ 用「附件存在性轮询 / 最小延时 + 重试」或接受「Release 合包可能晚于单平台二进制」的弱时序；
更稳妥的是在 `release-package` 开头 `sleep`/轮询直到 5 个附件齐全再 assemble。

---

## 八、验证计划（落地前）

1. **最小验证（路线 B 或 A 任一）**：先只保留 2 个目标（如 linux-amd64 + windows-amd64）并行，其余注释掉，
   触发一次 tag，确认两个 pipeline 真正并行（看开始/结束时间戳）、`dist/` 产物齐全、上传成功。
2. **artifacts 透传验证（仅路线 B）**：确认 build job 的 `dist/sqlguard-*-<triple>` 能被 package stage 读到；读不到则回退路线 A。
3. **全量并行**：放开 5 个目标，记录端到端墙钟时间，对比改造前（约单 stage 60m 上限）的耗时，确认压到 ≈ 最慢单目标时长。
4. **合包验证（路线 A2a）**：确认 `release-package` 能下载回 5 个二进制并产出 `-all.tar.gz` / 各平台包 / SHA256SUMS。
5. **回滚**：`git revert` `.cnb.yml` 改动即可回到串行版本，单脚本路径不变，风险可控。

---

## 九、一句话结论

> 复用 java-guard 的「**同 trigger 下多 pipeline 并行**」范式，把 sql-guard 的 5 个交叉编译目标各拆成自包含 pipeline
> （自带环境安装 + 单目标构建 + 按平台命名上传），再以一个「末段汇总 pipeline」组装 `-all` 合包与校验和。
> 真正的工程难点不在并行本身，而在 **COW 容器隔离导致的产物 handoff** 与 **cargo registry / CPU 资源在并行下的竞争**——
> 用「平台专属文件名 + 显式附件下载 + Cargo 缓存 + 上调 cpus」四招即可化解。
