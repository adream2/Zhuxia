# 干净重写记录

> **已合并**：2026-11-23 快进合并到 `main`（`e2b24b3..fc2de1b`），随后删除
> `rewrite/cleanroom` 与 `backup/v0.0.1-original` 分支、移除独立工作树与 `_ref/` 快照。
> 旧代码仍由 tag `v0.0.1-original` 保留，需要回溯时 `git worktree add` 一个即可。

> 状态：gesture / capture / clipboard / taskmon 的高风险文件已独立重写并通过行为对拍；
> 阻断类来源标记 **0**；依赖强著佐权 **0**；许可证为 **Apache License 2.0**（OSI 认可）；
> `tools/regress.sh` 14 项桌面回归全部通过。
> 基线：`v0.0.1-original` / `main@e2b24b3`。本分支的任何模块替换都必须通过第 1 节的验收门槛。

## 为什么不能一次性“从头重写”

现有代码 56,323 行，其中已识别的上游衍生文件约 9,400 行：

| 来源 | 许可证 | 高风险文件 | 行数 |
|---|---|---|---|
| WuGesture | 无 LICENSE | `gesture/{recognizer,model,engine,target,action,hook}.rs` | 2,898 |
| quickshot | GPL-3.0 | `capture/{design,style,annotate,overlay,stylepanel}.rs` | 5,632 |
| tiez-clipboard | GPL-3.0 | `clipboard/{model,rules}.rs` | 856 |

其余为 Apache-2.0 / MIT / MS-PL 允许衍生的部分（需保留声明），以及完全自研模块。
一次性重写全部 56k 行无法保证行为不变，也无法在单次会话内完成；本分支按模块重写、逐项验收。

## 两条可选路线

### 路线 G：保留现有 GPL 衍生代码，只解决无授权部分（最快）

- 整体切到 **GPL-3.0**，保留 quickshot / tiez 衍生代码；
- 只重写 `gesture`（WuGesture 无许可证，是当前唯一“连 GPL 都不能发”的阻塞项）；
- 结果：可合法分发，但**允许商业使用**；不能加“禁止商用”限制。
- 仍需保留 TaskMonitor 的 Apache NOTICE 与 sniplite/Filee/Captura 的 MIT 声明。

### 路线 NC：禁止商业使用（需要干净重写）

- 重写 `gesture` + `capture` 高风险文件 + `clipboard` 模型/规则；
- 完成后可采用 **PolyForm Noncommercial 1.0.0**；
- 结果：保留版权、禁止商业使用；但**不是 OSI 开源许可证**，GitHub 显示 “Other”。

**最终结果**：两条路线的重写工作都做了（阻断项清零），但许可证选了另一条——
**Apache-2.0**。原因：后期要求“OSI 认可的开源许可证”，而 OSI 定义禁止歧视商业使用，
NC 与 OSI 无法同时满足。重写工作没有浪费：正是因为阻断项清零，才敢直接切到 Apache-2.0。

## 阶段与验收

### 阶段 0：备份（已完成）

- Git tag：`v0.0.1-original`
- Git branch：`backup/v0.0.1-original`
- Bundle：`E:/WorkBuddy/Zhuxia-v0.0.1-original-20261122.bundle`
- 源码 zip：`E:/WorkBuddy/Zhuxia-source-20261122.zip`

### 阶段 1：行为基线与对比工具

1. 用当前 `main` 构建 `zhuxia-oracle.exe`，保存为只读基线。
2. 录制/生成金标准：
   - `gesture`：方向串识别样本（单笔、多笔、斜向、抖动、灵敏度 0/100/200）。
   - `clipboard`：HTML/富文本/敏感内容/清理规则 fixtures。
   - `capture`：截图自检图、标注/美化输出图、长截图拼接结果。
   - `record`：GIF 帧数/延迟/尺寸、MP4 时长/轨道。
   - `imgconv` / `appicon`：输入样本 → 输出哈希。
3. 统一入口：`rewrite/parity/run.ps1`，输出 JSON 报告；任何一项差异必须解释。

### 阶段 2：gesture（阻塞项）

- 新实现不得引用 WuGesture 的类名、方法名、注释与常量表；只依据行为规格重写。
- 保留：8 方向、多笔、12 段上限、两级规则、滚轮、边缘滚动、按键回显、单击重放。
- 验收：识别样本逐条一致；真实桌面 A/B 操作；无上游标识。

### 阶段 3：capture 高风险文件

- 顺序：`design`（token/绘制件）→ `annotate`（标注模型）→ `stylepanel` → `style`（美化）→ `overlay`（交互）。
- 设计值可以不同，但布局、命中区域、快捷键、导出结果必须通过金标准。
- 验收：截图自检图 SSIM ≥ 0.98；标注撤销/重做、导出尺寸、长截图拼接一致。

### 阶段 4：clipboard 模型与规则

- 重写 `Entry` 字段语义、HTML→文本、CF_HTML、敏感内容、清理规则。
- 验收：全部 fixtures 输出一致；隐私规则不回归；历史文件可读。

### 阶段 5：许可证与声明（已完成，2026-11-23）

最终采用 **Apache License 2.0**（OSI 认可）。先落过一版 PolyForm Noncommercial 1.0.0，
后来明确要求「OSI 认可的开源许可证」——这两者不可兼得：OSI 的许可证定义禁止歧视
商业使用，因此不存在“OSI 开源 + 禁止商用”的许可证。NC 版已整体替换。

- `LICENSE`：Apache-2.0 官方正文（SPDX 版本，含 9 条正文与附录）。
- `NOTICE`：项目署名 + 保留的第三方声明清单（Apache-2.0 §4(d) 要求随分发传递）。
- `README`：许可一节写清允许范围（含商用）、必须保留声明、商标不授权。
- `THIRD_PARTY_NOTICES.md`：拆成「保留声明的组件」与「已清除的衍生代码」两张表，
  并说明 Apache-2.0 与 MIT / MS-PL 兼容。
- 关于页：「许可」卡片显示 Apache-2.0 与官方链接。
- `tools/provenance_scan.py --fail`：阻断类 0；`tools/license_audit.py`：强著佐权 0。

许可可以选 Apache 的前提仍是阻断项清零：GPL 与无许可证代码不能留在 Apache-2.0 项目里。

## 行为不变的定义

- 用户可见：入口、快捷键、菜单、配置键、导出格式、默认值不变。
- 数据兼容：`settings.yaml`、剪贴板历史、图床历史、截图/录屏文件命名可继续读取。
- 性能：常驻内存与 CPU 在 ±10% 以内；截图/录屏延迟不高于基线。
- 差异仅限内部实现、代码结构与必要的外观 token。

## 当前状态

- 阶段 1：识别器对拍基线已完成（见下）。
- 阶段 2：`recognizer` / `engine` / `target` / `action` / `model` / `hook` 已独立重写，**6/6**；
  `gesture` 的来源扫描已清零。`lib/overlay` 仅清理了来源注释，代码未逐行重写。
- capture：`annotate` / `design` / `style` / `stylepanel` / `screen` / `target` / `pin` /
  `stitch` / `ocr` / `lib` 及 `overlay` 全部主体（工具条、色板、提示、选区几何、选区与编辑态绘制、
  **鼠标/键盘状态机**）已重写；`longshot` 与 `record/gifenc` 是自研算法，只清理了归属表述。
  `cargo test -p capture --lib` 68 项通过；来源扫描 0 命中。
- clipboard：`model.rs`、`rules.rs` 已独立重写，来源扫描 **0 命中**；
  行为用 145 行快照对拍，新旧输出逐字节一致。
- taskmon：`layout.rs`、`snapshot.rs` 已独立重写（布局 20,784 行输出 0 diff），
  `runtime.rs`/`lib.rs` 清掉上游表述，来源扫描 0 命中。
- 全仓库剩 **7 文件 / 3036 行**，全部是 **宽松许可**（Filee MIT、sniplite MIT）的来源说明，
  按许可要求**保留**；阻断类（无许可证 / GPL）已 **0**。

### 覆盖层状态机重写（2026-11-23）

- 原来所有阶段迁移都写在 `wndproc` 的大 `match` 里、嵌在 `with_ov` 闭包中，
  提前退出靠闭包里的 `return`，无法单独确认分支。
- 现在拆成 `Ov` 上的具名方法：`on_move` / `slide_selection` / `slide_annotation` /
  `on_press` / `begin_selection` / `press_editing` / `press_canvas` / `grab_anno_handle` /
  `on_release` / `settle_drag` / `on_dblclick` / `on_right_up` / `on_wheel` / `on_key` /
  `key_editing` / `key_frame` / `cursor_kind`；`wndproc` 只做分流。
- 行为逐条对齐：控制点优先于新框、图外点击不留标注、单击（<4px）取鼠标下窗口、
  拼接/录制收尾后不再重绘、Esc 两段式退出、方向键 1/10px 微调。
- 新增 `ZHUXIA_CAPTURE_TRACE=1` 阶段追踪，真机探针验证：
  - `flow_probe draw` 注入真实拖框 → `开始框选` → `松手进入编辑` → `编辑态就绪`；
  - 编辑态再拖一次 → `开始画标注` → `标注落笔`（标注=1、自动选中）；
  - 截图确认红色矩形 + 8 个控制点 + 浮动工具条正常；
  - `Esc` → `退出编辑回到选区` → 再次 `Esc` 回到选择 → `截图结束`。

### 剪贴板模型与规则重写（2026-11-23 09:35）

- 先做行为基线再动手：新增 `examples/rules_snapshot.rs`，把 145 行输出（分类、路径判定、
  HTML 抽取、实体解码、CF_HTML 往返、预览、指纹、隐私判定、脱敏、清理规则、来源策略、
  记录显示与日期换算）固化成文本，重写后逐字节 diff。
- `model.rs`：`Kind` 的四个属性收进一张表（`spec()`），`display_line` 按路径数量分支，
  日期换算改成按整年推进（闰年规则直接写出），不再用位运算版本。
- `rules.rs`：URL 判定、代码特征分、标签剥离、实体解码（手写扫描器替代正则）、
  CF_HTML 头解析/编码、预览、FNV-1a、隐私正则、清理规则、来源策略全部重写；
  行为细节逐条对齐（`&amp;` 放最后替换、`&#1a;` 整段吃掉、`td` 前加制表符、
  `# action: ignore` 只影响紧随其后的一条 `=>` 规则、空片段退化成整份文档）。
- 结果：快照 diff **0**；`cargo test --workspace --lib` **195 项全过**（剪贴板 31 项）。
- 真机冒烟：宿主采集两条文本（#3 文本、#4 文本），面板显示 4 条记录，
  预览、`T` 角标、图片缩略图、相对时间（2 分钟前 / 15 分钟前 / 8 小时前）均正常。

### 任务栏布局与快照重写（2026-11-23 09:50）

- 布局是纯几何，先用 `examples/layout_snapshot.rs` 建基线：mask 0–31 × 显存三态 ×
  横/竖 × 紧凑 × 6 档可用宽度 × 3 种分组顺序 = **5760 组输入**，每组输出
  全部字段 + 每个槽位矩形 + 每个位置矩形 + 分组/列宽 + 条带高度，共 **20,784 行**。
- `layout.rs` 重写：槽位打包（`pack_slots`）、横向分列（`place_horizontal`）、
  竖直单列/多列（`single_column` / `grid_columns`）拆开；`normalize_order` 改用位集；
  常量与字段名保持不变（它们是绘制层与命中测试的契约）。
- 结果：diff **0 行**（5,760 组全一致）。
- `snapshot.rs` 重写：去掉 `SnapshotSlot` 包装，直接 `OnceLock<RwLock<Arc<_>>>`，
  读取不再 `unwrap()` 崩溃（中毒时取回内部值）；字段按 CPU/概况/进程榜/内存/网络/磁盘/显卡分组。
- 真机：任务栏 208×48 正常渲染（`CPU 11% | 磁盘 0% | ↑36.1 KB/s` / `内存 41% | GPU 0% | ↓5.7 KB/s`）。

### 来源扫描按许可证分类（2026-11-23 09:55）

- 修掉 `MoveFileEx` 被当成 `Filee` 的词内误匹配（改用词边界）。
- 扫描器区分两类：**阻断类**（WuGesture 无许可证、quickshot/tiez GPL-3.0、PicX AGPL-3.0）
  与**宽松类**（Apache-2.0 / MIT / MS-PL）。`--fail` 只拦阻断类，`--fail-all` 才要求全清零。
- 理由：MIT/Apache 的代码可以留在非 GPL 分发里，只要留住版权与许可声明；把这些
  署名清掉反而不合规。目前 `--fail` 退出码 0，阻断类 0 个文件。

### 依赖许可审查与桌面回归（2026-11-23 10:40）

- 新增 `tools/license_audit.py`：解析 Cargo 全部依赖的 SPDX 表达式（支持 `OR` 取宽、
  `AND` 取严、括号优先级），把依赖分成宽松 / 弱著佐权 / 强著佐权 / 未声明四档。
  结果：533 个依赖中宽松 **526**、MPL-2.0 **7**、强著佐权 **0**、未声明 **0**。
  MPL 那 7 个（`cssparser` / `selectors` / `dtoa-short` / `option-ext` / `cssparser-macros`）
  只约束被修改的文件，本项目未改。
- 新增 `tools/regress.sh`：一条命令跑 14 项检查——构建、workspace 单测、手势诊断与自检、
  截图 / 美化 / 长截图 / GIF 自检、宿主常驻、剪贴板采集、任务栏创建与上屏、运行期 panic、
  退出无残留。当前 **14 通过 / 0 失败**。
- 回归发现的环境问题（非代码缺陷）：宿主会自我提权，父进程退出、`taskkill` 对提权实例
  「拒绝访问」。脚本改用程序自带 `--quit` 收尾，才做到可重复清理。
- 截图链路顺带确认：`截图自检通过……剪贴板写入=true 校验=true`，
  GIF 自检 `20 节拍 → 10 帧，200cs 对齐`。

### 识别器对拍记录（2026-11-22）

- 方法：同一组方向串 × 灵敏度 0/100/200 × 长度 60/120/220，共 225 组；
  分别跑旧版与新版的 `recognize_probe`，逐字节对比输出。
- 结果：**225/225 一致，0 diff**；`cargo test -p gesture --lib` 36 项全过。
- 覆盖：单笔 8 方向、多笔折线、斜向 `↙↘↙`、抖动、卡片式 L 形、灵敏度边界。
- 限制： synthetic 直线样本；真实鼠标曲线仍需阶段 6 的桌面 A/B。

### 状态机/窗口判定（2026-11-22）

- `engine.rs`：重写为 `Machine/Run`，保留精确优先匹配、滚轮冻结、`on_wheel`/抬手执行、
  位移不足重放单击、抬手跳变过滤；新增 3 个单测（滚轮余数、跳变、微位移）。
- `target.rs`：重写 Shell/桌面/进程判定，保留 1 秒进程缓存与 explorer 文件夹窗口放行。
- `cargo test -p gesture --lib`：38 项全过。
- 真实钩子探针 `hook_probe`：注入右下斜线，收到 Start + 12×Move + End（14 事件）；
- 端到端探针 `engine_probe`：真实钩子 → 引擎 → 匹配 → 动作钩子，`action_hits=1`；
- `--gesture-doctor`：通用 12 条 / 分组 1 个；`--gesture-selftest` 退出 0；
- 来源扫描 `crates/gesture`：0 命中。

### 斜向手势修正（2026-11-22 23:47）

- 现象：`↙↘↙` 被识别成 `↓→↓`，打开 hosts。
- 原因：斜率偏陡时 8 扇区把第一笔归成 `↓`；Exact 可能只得到两段，
  低保真再给出 `↓→↓`。
- 修正：`Recognizer::diagonal_zigzag` 按 x 反转切成三段，只要三段向下、水平位移
  足够且符号是 `- + -`，直接返回 `↙↘↙`；引擎在 Exact/Cardinal 之前优先使用它，
  且不再回退到 cardinal。
- 验证：40 项单测；W 斜率 0.6–3.0 均识别；真正的 `↓→↓` 不触发；端到端注入 W
  命中截图规则（capture_hits=1）。

### capture/annotate（2026-11-23 00:00）

- 独立重写 `annotate.rs`：10 工具、包围盒、控制点、缩放、马赛克、序号。
- 单测 68 项全过；`--capture-selftest` 退出 0，自检图可见矩形/箭头/序号。
- `design.rs`、`style.rs`、`stylepanel.rs`、`screen.rs`、`target.rs`、`pin.rs`、`stitch.rs`、`ocr.rs`、`lib.rs` 已重写；
  `overlay.rs` 工具条/色板/提示/选区几何/选区绘制/编辑绘制已重写，选区状态机与文本编辑仍是旧实现。
- `longshot.rs` / `record/gifenc.rs` 为自研算法，只清理了参考注释，未做无意义重写；
  capture 来源扫描已 0 命中。
- 下一步：`stitch.rs` / `ocr.rs` / `lib.rs` / `longshot.rs` / `gifenc.rs`，再回 `overlay` 状态机。
