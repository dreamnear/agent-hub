# CHANGELOG

本文件记录 agent-hub（claude-view）每个版本的显著变更。**版本规则（2026-10-13 立）**：版本 = main head 上的 `vX.Y.Z` tag（部署/发布后打）；语义化——新特性 +0.1.0，缺陷修复 +0.0.1；**每个版本必须给出条目**，缺条目不得打 tag。

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.4.0] - 2026-10-16

### Added — 工程历史会话浏览与恢复（#11）

- claude agent 行的 `⋯` 菜单新增「历史会话」：弹窗列出该 agent 工程的 **CLI 会话全量**（数据源 `~/.claude/projects/<slug>/*.jsonl`），行内相对时间 · 分支 · 体积 · worktree 名 · 会话名，口径对齐 CLI 原生 Resume 选择器；输入即过滤
- **会话名三级名源**（CLI 原生 customTitle > 转录 summary 行 > 首条可读用户文本）+ 分块扩展读（256KB 块、上限 2MB、行边界切分保多字节安全、命中早停）——XML/命令包装行过滤，附件巨行不再挤掉可读名（实测无名字条目 50% → 0）
- **恢复**：选中即 `claude --bg --resume <id>`（current/active 复用/resumed 三态 + spawn 前重复保护），消息视图呈现历史转录、可继续发送
- 排版与可用性：列表左对齐（几何断言 Δ≤0.01px）、名称最多 2 行省略、体积右对齐；⋯ 菜单 **Portal 到 body + 实测翻转放置**（任何视口/行位置完整可见，矮视口贴底兜底）；菜单项按驱动收紧（pi 等非 claude 行不再显示 Logs/历史会话死入口）

### Fixed — 带名会话 id 解析（#10）

- `parse_bg_id` 重写为结构锚定（`backgrounded` 标记后首个短 hex → 首个短 hex → 单 token 原样），**带名新建会话的 id 不再被解析成整行**；解析失败显式报错，不再静默返回不可用 id
- `extract_bg_id` 兜底分支加固：输出缺 `backgrounded` 字面不再整行变 id，hex 形名字（`dead`/`123`）不再被当 id
- 如实注记：主路径症状自 #11 起已被 `extract_bg_id` 遮蔽，本修消除缺陷本体与剩余可达路径并显式化失败

### 工单索引

- 本批：#11（历史会话浏览与恢复）/ #10（带名会话 id 解析）
- backlog：#15（会话历史扩展到 ACP 驱动，已确认范围 claude+omp）/ #5（sidecar 假失败）/ #6（MCP prompts）/ #7（nodelink 透传）/ #8（缓存加固）

## [0.3.0] - 2026-10-15

本轮为 issue 驱动的三单批次（#1 B 阶段 / #2 / #3），一次部署上线。

### Added — `/` 补全数据源切换为 SDK 精确校准源（#1，B 阶段）

- `/api/commands`（本机 claude agent）数据源由 A 阶段文件枚举切为 **CLI stream-json 控制协议**：spawn 一次性子进程（`claude --output-format stream-json --verbose --input-format stream-json`）→ 写 `initialize` 控制请求 → 读 `response.response.commands[]`。与 CLI TUI `/` 菜单精确一致（内置命令随 CLI 版本、插件启用态含项目作用域、项目级技能随 cwd），不再依赖「猜」的文件枚举
- 缓存：按 `(claude_root, cwd)`，SDK 源 TTL 300s／回退 60s（兼失败负缓存），同 cwd 二次请求命中缓存（实测 ~550ms → 0.6ms）
- 降级：子进程失败（CLI 缺失／超时／解析失败）回退 A 阶段文件枚举，HTTP 200 不报错
- 已知边界：**MCP prompts 不在校准源内**——CLI 在 stream-json 模式对 MCP 惰性连接，`commands[]` 推送不含 prompts（结构性行为；补枚举 follow-up 见 #6）。需求 B1 已就地修正为「校准源口径」

### Added — 首发投递确认层（#3）

- 新 spawn/attach 会话的**首次发送**改为「确认后提交」：以 TUI 回显 + raw 模式作为「CLI 已消费」的证据（canonical 态回显不采信），未确认则清行重写（≤3 次 × 2.5s），超限沿明确失败路径上报（HTTP 400 + 文案），**不再对未被消费的写入报成功**
- 与既有机制解耦：`\r`×3 提交原子性、忙态排队放行（10s 就绪闸）、中断 Esc 防吞行为均不变；每 attach 生命周期只布防首发

### Fixed

- 模糊查找求值缓存（#2）：`fuzzyFilter` 按（清单引用, query）memo 化，命中复用同一结果引用——2000 条清单实测 117µs → 0.01µs（~12500×），冷路径不劣化；行为零变化（打分/排序算法逐字未动）
- 命令枚举缓存统一层（吸收 #2 后半）：文件枚举回退路径并入同一缓存机制，负缓存生效
- `AttachSession` Drop 兜底 kill（#3 附带）：未 release 的 drop 曾使输出 drain 永久阻塞、挂死 tokio blocking pool（开发中被单测以 8 分钟挂起实证）

### 工单索引

- 本批：#1（B 阶段 SDK 校准源）/ #2（命令枚举性能）/ #3（首发时序加固）
- 测试中发现并另立：**#10**（`cli.rs::parse_bg_id` 遇 `--name` 解析错位——UI 建带名会话即坏）
- backlog：#5（Tauri sidecar 致全 workspace cargo test 4 例假失败）/ #6（MCP prompts 枚举）/ #7（nodelink 远程透传）/ #8（缓存加固：LRU + 并发去重）

## [0.2.0] - 2026-10-13

**今日发布**（7800 正式环境部署态；2026-10-14 仓库历史重置，本条目随重置后的初始提交基线，内容等同重置前 `b3c1b6b` 树）。主体为本轮 slash-skills 三轮交付 + v0.1.1（2026-09-28）以来 445 提交的能力块补录（粒度摘要；重置前逐提交史仅存本地 bundle 备份）。

### Added（本轮：/ 补全体系）

- **`/` 补全数据源扩展**：全局技能（`~/.claude/skills`）、项目级 skills/commands（API 带 `?cwd=`，子目录 `ns:name`）、插件命令与技能（`plugin:` 前缀 + `enabledPlugins` 启用闸 + project 作用域 cwd 匹配）、frontmatter name/description 解析——补全面板数据源对齐 TUI
- **菜单过滤模糊查找**：`lib/fuzzy` 子序列匹配（大小写不敏感，前缀 > 连续 > 词首 > 靠前计分，同分短名优先，乱序/缺字符不命中，空查询回落全量）
- **菜单键盘操控**：Enter 选中候选仅替换 draft（尾空格关菜单，二次 Enter 才发送）、`↓↑` 移动高亮、`Esc` 关闭回落直发——修复菜单打开态 Enter 误提交中间态 draft 的缺陷
- **菜单可用性**：按来源分组展示（内置/技能/插件）+ 候选上限 50 + 滚动

### Added（区间补录：v0.1.1 → v0.2.0 主要能力块）

- **Agent 体系**：claude/opencode/dsh/omp 多驱动统一（driver-unify）、助手会话（hub-assistant）与单例闸、助手内定时任务（assistant-cron）、模型/思考档控制、AskUserQuestion 提问卡结构化渲染
- **Git 面板**：提交 Timeline 与文件清单两级联动、变更区行组件统一复用、子仓不进父仓变更列表、meta 行去重、图片对照 diff（binary 双窗格）、D 文件全删镜像、路径语义对齐 git-status（cwd≠仓库根 400 根修）
- **文件体系**：文件查看进右面板 + 主窗口多 Tab（面板-主功能区模型）、diff 页签化与跳源、文件树 cwd 泛化
- **终端**：网页端终端回归（纯工具定位，PTY 既有依赖不变）
- **远程/网关**：gateway-node 系列（p1-p3 + sweep）、nodelink 远程实例、多实例管理、远程安装
- **工程化**：设置页 i18n、三态主题体系（auto/dark/light + token 红线）、bundle 预算门禁（gz ≤300KB）、r 系列行为回归

### Fixed

- 菜单打开态 Enter 误提交中间态（本轮 round 3 人审缺陷，根因=菜单无键控）
- 变更区 meta 行与分区头同文重复、tree 行居中回归、pi 忙态用户消息双渲染等（详见各工单报告）

### 工单索引

- 本轮 slash-skills：管理仓需求 `docs/requirements/agent-hub-slash-skills/`（调研定案先A后B，A 已落地）
- B 阶段（SDK 精确校准源）：dreamnear/agent-hub#1；优化批：#2；spawn 首发时序加固：#3

## [0.1.1] - 2026-09-28

- fix(test): git panel 子模块测试允许 file 协议（git≥2.38.1 CVE-2022-39253 默认禁，6d48075 回流）

## [0.1.0]

- 初始版本基线（详见 git log）
