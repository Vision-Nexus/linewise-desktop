# linewise-desktop ← wave prototype：1:1 设计+交互移植方案

目标：Rust/Dioxus 桌面端(`lw-app`)的上传界面 **1:1 对齐** wave 网页原型（`wave-factory-partner-desktop`，分支 `cursor/batch-transfer-upload-status-ui` @ `bd38b5b`）的**视觉与交互**。

本文件是实施规格：每条任务带精确的文案/公式/状态/落点文件。基线是 `feat/ui-neutral-restyle` 分支（已完成中性 token、Inter 字体、图标那批）。

---

## 0. 原则与前提

1. **视觉+交互 1:1，但后端接真实引擎。** 原型是纯前端 mock（无真实上传/网络）；桌面端有真实 `lw-core` 引擎。移植 = 复刻原型的**外观、状态、交互模式**，但接到桌面已有的 `AppState` signals / `UploadRuntime` / `UploadEngine`。**不把真实功能降级成 mock。**
2. **数据零新增到 core。** batch overview 所需的所有数字（总数、各状态数、总字节、已传字节、聚合速度、合并 ETA）都能从现有 `AppState::upload_tasks` + `upload_progress` + `upload_speed` 派生（见 desktop map §5）。无需新增 lw-core 字段/迁移。
3. **保留桌面端更强的真实能力**：真实 4 档网络探测（RTT）、自动重试、断点续传——这些原型没有，**不能删**，只把**呈现形式**换成原型的样子。

---

## 1. 需要你先拍板的决策（会影响任务范围）

- **D1 转码**：原型**没有**任何转码 UI（注释写明"转码交服务端"）。桌面端当前有 `Transcoding` 状态 + "Preparing" 段 + Staged 行的 Transcode 开关。你已定"转码不在桌面让用户操作、交服务端"。→ **建议：移除桌面的 Transcode 开关 + "Preparing" 段 + `Transcoding` 状态的用户可见呈现**（引擎侧是否保留 transcode 能力另说；UI 不暴露）。**请确认。**
- **D2 元数据采集(Capture metadata / io.visionlab tags)**：这是桌面**独有**的真实功能（Add/Edit/Skip metadata，上传前写标签），原型**没有**。严格 1:1 会把它藏掉——但这是有用的真实特性。→ **决策：1:1 隐藏它？还是保留（作为对原型的有意增量）？** 我倾向**保留但收进 overflow / 折叠**，不占主视觉。**请拍板。**
- **D3 网络状态**：原型是 **3 档（Good/Slow/Offline）纯装饰**（degraded 只能靠 simulate 触发、不驱动任何行为）；桌面是 **4 档真实 RTT 探测**且**驱动自动重试/弱网横幅**。→ **建议：保留桌面真实探测，把呈现映射成原型的 3 档 pill**（Good→connected，Ok+Weak→"Slow"，Offline→offline），放到侧栏 brand。**不要**为了 1:1 砍掉真实探测。**请确认这个映射。**
- **D4 弱网横幅 `WeakNetworkBanner`**：原型没有独立横幅（把提示收进网络 pill 的 tooltip/dropdown）。桌面有真实的 30s 触发横幅。→ **决策：保留横幅，还是并入网络 pill 的下拉提示？** 建议保留（真实、对国内弱网有用）。
- **D5 Simulate/Preview 菜单**：原型的 Simulate/"Preview all states"/"Load N samples" 是设计演示工具。桌面要不要带？→ 建议 **dev-only（debug build 显示）或直接不移植**，生产 UI 不需要。

---

## 2. 差距总览（原型有 → 桌面现状 → 动作）

| 原型特性 | 桌面现状 | 动作 |
|---|---|---|
| 三大 tab（In progress/Completed/Failed）+ 计数 | ✅ 已有 | 保留 |
| Failed 二级 Quality/Network | ✅ 已有 | 保留 |
| In-progress **3 段**（Checking files / In queue / Uploading）| ⚠️ 现为 **5 段**（Uploading/Preparing/Ready to Upload/Hashing/Checking）| **合并为 3 段 + 改名** |
| 徽章文案集（Checking files/In queue/…）| ⚠️ 命名不一致（"Ready to Upload" 等）| **对齐文案** |
| 进度%+子文案（分段常量+lerp）| ⚠️ 有进度但公式/文案不同 | **对齐公式与文案** |
| **整批 overview 头**（%/X of N/字节/ETA/速度/分段条）| ❌ **无** | **新建**（纯派生，无 core 改动）|
| **网络状态 pill**（侧栏 brand，Good/Slow/Offline + 下拉）| ⚠️ 有 4 档 bar chip（在面板头）+ 弱网横幅 | **换成 pill 放侧栏 brand**，映射真实 4 档 |
| **侧栏每批次状态点**（idle/进行/完成/有失败）| ❌ **无**（侧栏完全不读上传状态）| **新建**（把 upload_tasks 按 tenant/project 分组接进侧栏）|
| Upload 菜单 File/Folder | ✅ 有(Select files…/Select folder…) | 对齐文案+图标 |
| 行内动作（Pause/Resume/Retry/Override/Remove/Cancel/Locate）| ✅ 基本齐 | 对齐文案 |
| In-progress **stage 过滤 pill**（All/[1]Checking/[2]In queue/[3]Uploading）| ❌ 无（现为纵向堆叠各段）| **新建过滤 pill 行** |
| Completed 二级（All/Completed/Already exists）| ⚠️ 有 already-exists 徽章，无子 tab | **加子 tab** |
| Transcode 用户选择 / Preparing 段 | 桌面独有 | **移除**（见 D1）|
| Capture metadata 流程 | 桌面独有 | **待定**（见 D2）|
| Simulate/Preview 工具 | 桌面有简版 | dev-only/略（见 D5）|

---

## 3. 详细任务清单（按工作流分组，含产品细节）

### 工作流 A — 状态模型 & 分段对齐（基础，先做）

**A1. In-progress 三段化 + 改名**（`transfer_panel/in_progress.rs`）
- 目标段（顺序 = step）：
  1. **Checking files** — 副标题 `Reading and verifying format, quality, and metadata.` — 匹配 `QualityChecking | Hashing`（把桌面现有 Checking+Hashing 两段**合并**）。
  2. **In queue** — 副标题 `Passed checks — waiting to start the next stage.` — 匹配 `Staged`（原 "Ready to Upload" 改名）。
  3. **Uploading** — 副标题 `Validating and transferring original files to the cloud.` — 匹配 `Pending | Validating | Creating | Uploading | Verifying | Paused`（把 Pending 等并进来）。
- 移除 "Preparing"(Transcoding) 段（见 D1）。
- 排序：段内先按管线顺序（QualityChecking→Hashing→Staged→Pending→Validating→Creating→Uploading→Paused→Verifying），再按加入时间升序。空段隐藏。

**A2. 徽章文案 + 配色对齐**（`transfer_panel/rows.rs` 的 `status label` + `statusBadgeClass` 等价物）
- 用户可见徽章串（仅这几种）：**Checking files / In queue / Uploading / Completed / Already exists / Rejected / Failed**。
  - `Staged` → "In queue"（不是 "Ready"）。
  - `QualityChecking|Hashing` → "Checking files"。
  - `Pending|Validating|Creating|Uploading|Verifying|Paused` → "Uploading"。
  - `Completed` + already-exists marker → "Already exists"。
- 配色（低饱和 tint，跟随 token）：completed=emerald；failed/rejected=destructive；uploading/verifying/pending/validating/creating=primary；staged="In queue"=sky；paused=amber；already-exists=amber；checking/hashing=muted。徽章高 `h-4`，超宽截断。
- 卡片 border/bg tint 跟随同一配色（`cardTone` 等价）。

**A3. 进度% + 子文案对齐**（`rows.rs` 进度行）
- 仅这些状态显示进度条：checking(quality_checking/hashing) 与 upload-stage(pending/validating/creating/uploading/verifying/paused)。`staged/completed/rejected/failed` **无进度条**。
- 公式（分段常量 + lerp，确定性、由状态+进度字段算，无计时器）：
  - `quality_checking` → 固定 **8%**，文案 `Checking files 8% — Checking your file`
  - `hashing` → `round(lerp(12,100, done/total))`，文案 `Checking files N% — Reading your file`
  - `pending` → **2%** `Uploading 2% — Waiting to start`
  - `validating` → **10%** `Uploading 10% — Confirming file details`
  - `creating` → **18%** `Uploading 18% — Setting up your upload`
  - `uploading` → `round(lerp(22,96, done/total))`，文案 `Uploading N% — Transferring to the cloud — {done}/{total} — {speed}/s · ETA m:ss`（speed/ETA 有才显示；空段过滤）
  - `paused` → 同 uploading 的%，文案 `Uploading N% — Paused · {done}/{total}`（无 speed/ETA）
  - `verifying` → **99%** `Uploading 99% — Finishing up`
- 字节格式：base-1024，单位 B/KB/MB/GB/TB，值≥10 或 B 时 `.0`，否则 `.1`。ETA=`M:SS`。（桌面 `format_size`/`format_duration` 已有，核对规则一致。）

**A4. 移除转码用户选择**（见 D1，确认后）：删 Staged 行的 Transcode 开关、"Preparing" 段、`Transcoding` 的用户可见呈现。

---

### 工作流 B — 整批 Overview 头（最高可见价值，纯派生无 core 改动）

**B1. 派生汇总**（新建 `transfer_panel/overview.rs`，从 `tasks + upload_progress + upload_speed` 计算）
- 每任务"有效字节"：`completed`→{done:size,total:size}；`failed|rejected`→{done:0,total:size}；其余 `done = round(size * pipelinePct/100)`。
- 每任务 pipelinePct：completed=100；failed/rejected=0；checking=`round(checkingPct*0.08)`(0–8)；staged=10；upload-stage=`round(10 + uploadStagePct*0.9)`(10–~96)。
- 聚合：`totalFiles`、按 tab 分 `completed/failed/inProgress` 计数；stageCounts(checking/queued/uploading)；`totalBytes`/`transferredBytes`；`remainingBytes`；
  - **`overallProgressPct = round(transferred/total*100)`（按字节加权——头部%和进度条用这个）**；
  - `aggregateSpeedBps` = 所有 `uploading` 且有速度的任务的 `upload_speed` 之和；
  - `estimatedSecondsRemaining = aggregateSpeed>0 && remaining>0 ? remaining/aggregateSpeed : null`；
  - `batchFinished = inProgressFiles==0`。
- 桌面取字节：分母用 `transcoded_size.unwrap_or(size)`；已传用 `upload_progress[id].0`（单调，别信 `bytes_uploaded`），completed 记满 size；速度用 `upload_speed[id]`。（desktop map §5 已确认可行。）

**B2. 头部渲染**（放到 project header 右侧；参照原型 `BatchUploadOverview`）
- 未完成态：`{overallProgressPct}%` + muted "progress" + `Loader2` 旋转（有活跃任务时）+ 进度条。
- 完成态：`CheckCircle2`(emerald) + "Batch complete"（有失败则 `· N succeeded · N failed`）。无进度条。
- meta 行（` · ` 连接，截断+`title` 全文）：`{completed} of {total} videos` · `{transferred} / {total}` · 未完成时 `Est. {Hh Mm|Mm Ss|Ss} left`（ETA 未知则 `Estimating time…`，仅当有进行中）· 有聚合速度时 `{speed}/s`。

**B3. 分段填充条**（`BatchProgressBar` 等价）
- 轨道细条；填充宽 = `overallProgressPct%`；填充内按**文件数**分色段，固定顺序 completed(emerald)/uploading(primary)/checking(muted-fg)/queued(sky)/failed(destructive)，每段宽 `count/total*100`，0 段略过，hover tooltip `{count} {label}`。

---

### 工作流 C — 网络状态 pill（真实探测 → 原型呈现）

**C1. NetworkStatusIndicator pill**（放侧栏 brand 右侧；替换/补充现有面板头的 `NetworkChip`）
- 3 档呈现，映射桌面真实 `NetworkHealth`（4 档）：`Good→connected`、`Ok|Weak→degraded(Slow)`、`Offline→offline`。
- pill = 状态点(degraded 时脉冲 ping) + Wifi/WifiOff 图标(Offline 用 WifiOff) + 短标签 **Good/Slow/Offline**。
- 配色：connected=emerald，degraded=amber，offline=destructive。
- 文案（tooltip/下拉）：
  - connected：标题 `Connection stable`；说明 `Uploads and quality checks are proceeding normally.`
  - degraded：`Connection is slow or unstable`；`Uploads may pause, retry, or take longer until your network improves.`
  - offline：`No internet connection`；`Uploads and quality checks are paused until you're back online.`
  - 通用 callout（标题 `Keep the app open`）：connected/degraded/offline 各一句（见 wave map §3.1）。

**C2. 下拉**（点击 pill）：标题 "Network status"；真实态下不显示 "(simulated)"；列出 3 档状态说明（当前高亮）。**桌面用真实探测,所以去掉原型的 "Simulate 网络" 项**（那是 mock 专用）；保留只读的状态说明。

**C3. 收口 `NetworkChip` / `WeakNetworkBanner`**（见 D3/D4）：pill 取代面板头的 bar chip（或二者并存，建议 pill 为主）；`WeakNetworkBanner` 建议保留（真实弱网>30s 触发）。

---

### 工作流 D — 侧栏每批次状态点（新增数据接线）

**D1. 把上传状态接进侧栏**（`components/sidebar.rs` + 新 `batch_nav_status` 派生）
- 侧栏当前完全不读 `upload_tasks`。需 `use_context::<AppState>()` 读 `upload_tasks`，按 `(tenant_id, project_id)` 分组，每个 project 节点派生一个状态点。
- 点状态（原型 `getBatchNavStatus`）：
  - **idle**（0 任务）→ 不显示点。
  - **in_progress**（inProgress>0）→ 点 `bg-primary` **脉冲**；tooltip `{completed} of {total} videos · {inProgress} in progress`。
  - **complete_with_issues**（无进行中 & failed>0）→ 点 destructive，不脉冲；tooltip `Batch complete · {completed} succeeded · {failed} failed`。
  - **complete**（全完成无失败）→ 点 emerald，不脉冲；tooltip `Batch complete · {completed} of {total} videos`。
- 点 `size-2` 圆，脉冲变体叠一层同色 `animate-ping opacity-60`。tooltip 靠右,并入行 aria-label。

---

### 工作流 E — 交互 & 细节对齐

**E1. Upload 菜单文案/图标**（`transfer_panel/mod.rs` + 图标）：菜单项 **"File"**(FileUp) / **"Folder"**(FolderUp)（原型用这俩词）；主按钮已是 ↑ Upload ⌄。拖拽区行为保留。（需补 FileUp/FolderUp 两个 lucide 图标到 `icons/mod.rs`。）

**E2. In-progress stage 过滤 pill**（新增，`in_progress.rs`）：顶部一行 pill —— **All ({total})** ▸ **[1] Checking files (n)** ▸ **[2] In queue (n)** ▸ **[3] Uploading (n)**；编号圆 badge + ChevronRight 分隔 + `aria-pressed`；选中某段只显示该段，"All" 显示全部（按管线序，含副标题）。空态文案：`Nothing in {stage}` / `Nothing in progress`。

**E3. Completed 二级 tab**（`completed.rs`）：ToggleGroup **All (n) / Completed (n) / Already exists (n)**（already-exists = completed+marker）。段标题 All→"Completed"，completed→"Uploaded"，already_exists→"Already exists"。空态文案见 wave map §7.5。

**E4. 行内动作 & 文案对齐**（`rows.rs`）：
- `uploading`→**Pause**（amber outline）；`paused`→**Resume**；`failed|GaveUp`→**Retry**；`rejected`→**Override & upload**（amber outline，aria "Override quality check and upload"）。
- overflow(⋯)：failed/rejected→**Remove**；其它非完成态→**Cancel upload**；completed→无。
- 时间戳仅 completed/failed/rejected 显示（`Mon D, YYYY, h:mm`）。chevron 常在，单开手风琴。
- 失败/拒绝/警告文字前 ⚠（已在 restyle 分支做）。
- Retry all connection failures（Failed 的 All/Network 子 tab，有网络失败时显示）——桌面已有。

**E5. 展开详情**（`rows.rs` 折叠区）：`<dl>` — Source(`H264 · 3840x2160 · 60fps · 85.0 Mbps` 或占位 `— · — · — · —`)、Org/Batch、Local path(mono)、Size、有 videoInfo 则 Duration(+Telemetry)、状态时间戳行；already-exists 加 Note "Content already existed on the server; no new upload was performed."；completed 加 "Locate in sidebar"。

**E6. 空态/首页文案**（对齐原型精确串）：首页 "Select an organization" / org 页 "Select a batch"/"No batches yet" / 空批次 "Upload to {batch}"。

**E7. Simulate/Preview**（见 D5）：生产不做,或 dev-only。

---

## 4. 建议实施顺序（每阶段可独立编译+看效果）

- **Phase 1（基础）**：A1 三段化 + A2 徽章 + A3 进度文案 + A4 去转码。→ In-progress 立刻和原型一致。
- **Phase 2（头部总览）**：B1/B2/B3 overview。→ 最高可见价值,纯派生。
- **Phase 3（外壳）**：C 网络 pill(侧栏 brand) + D 侧栏状态点。→ 需要接线,改动侧栏。
- **Phase 4（交互补齐）**：E2 stage 过滤 + E3 completed 子 tab + E1/E4/E5/E6 文案与细节。
- 每阶段：`cargo build -p lw-app`（需 FFMPEG_DIR/LIBCLANG_PATH + lw-app npm 装 tailwind）→ 起 exe 目测。

## 5. 非目标 / 明确不做
- 不把真实引擎换成 mock；不删真实网络探测/自动重试/断点续传。
- 不移植原型的 Simulate/Preview 工具到生产 UI（除非 dev-only）。
- 交互/结构改动仅限 UI 层；`lw-core` 除非 D1 决定引擎侧也去 transcode,否则不动。

---

## 决策（已锁定 2026-07-03）
- **D1 转码 = UI 隐藏、引擎保留。** UI 层删除 Staged 行 Transcode 开关 + "Preparing" 段 + `Transcoding` 的用户可见呈现；**`lw-core` 引擎的 transcode 能力保留不动**（以后可后台用）。→ A4 据此执行；不改引擎。
- **D2 Capture metadata = 保留但收折。** 不删这个真实功能；从主行移到 overflow/展开详情里，不占主视觉。→ 新增子任务 A5。
- **D3/D4 网络 = 3 档 pill + 横幅并入下拉。** 保留真实 4 档探测,映射成 Good/Slow/Offline pill 放侧栏 brand；**移除独立 `WeakNetworkBanner`**,把它的弱网/离线提示文案并入网络 pill 的下拉/tooltip。→ C1/C2 执行；C3 改为"移除 banner,提示并入 pill 下拉"。
- **D5 Simulate/Preview = 仅 debug build 显示。** 用 `#[cfg(debug_assertions)]` 之类门控,release 不出现。→ E7 据此。

### 据决策新增/调整的任务
- **A5（新）Capture metadata 收折**：把 Staged 行的 Add/Edit/Skip metadata 从主行移到 overflow 菜单或展开详情区；保留全部功能与文案,仅降视觉层级。
- **C3（改）**：删 `WeakNetworkBanner` 组件与其在 `MainView` 的挂载；其弱网(>30s)/离线提示改为网络 pill 下拉里的一段（真实态驱动,非 mock）。
- **E7（改）**：Simulate/Preview 菜单包一层 `cfg(debug_assertions)`；release build 不编入。
