# CHANGELOG

本文件记录 agent-hub（claude-view）每个版本的显著变更。**版本规则（2026-10-13 立）**：版本 = main head 上的 `vX.Y.Z` tag（部署/发布后打）；语义化——新特性 +0.1.0，缺陷修复 +0.0.1；**每个版本必须给出条目**，缺条目不得打 tag。

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.7.0] - 2026-10-21

本批：插件体系 B2 会话能力桥（#31，epic agent-hub-mng#11）——插件的工具/MCP/skill/command 正式进入会话链路。

### Added — 会话能力桥（#31）

- **MCP 聚合代理**：hub 新增 `mcp-proxy --session` 入口（stdio MCP server），把启用插件的工具聚合暴露给会话（全名 `hubplug_<pluginId>_<tool>`）；一切调用经 hub 闸门（权限判定/凭据注入/scrub/留痕），**无直通路径**；工具描述按不可信输入处理（替换+截断+固定标注）
- **会话启动期注入**：claude 驱动经 `--mcp-config` 注入会话级配置（hub 数据目录生成，不碰用户 `~/.claude`；占位文件原地原子重写修 bg/daemon 晚读竞态，#32）；pi 驱动经 `pi.registerTool` 注册；stop/remove 按会话→路径映射回收
- **高危审批流**：高危类调用（hub 维护四类固定清单）先落审批单 → 会话内确认卡（HighRiskApprovalDialog）→ 批准执行/拒绝不执行，双侧留痕 + 120s 超时 busy 可重试 + 防重复提交；**工具描述夹带「跳过确认」类指令不影响确认流程**（防投毒红线实测）
- **command 贡献物**：插件命令注册进 `/` 补全——`target:{view}` 打开插件视图 / `target:{tool}` 触发只读工具（审计限 risk:"read"）
- **能力可发现面**：会话侧可见当前可用工具/skill/MCP 清单（来源插件 × 风险标注；描述标注「来自插件，非 hub 审定」）
- **留痕卡**：工具调用/错误/拒绝在会话流中的卡片呈现；violation/approval/rejection 记录入故障记录域
- **示例插件 v0.2.0**：新增高危演示工具 `add_issue_note`（`risk:"high"`，写 issue 备注）——端到端审批演示载体

### Fixed

- **B1 潜伏缺陷**：manifest `Permissions` 结构缺 `rename_all="camelCase"`，`permissions.highRisk` 键自 B1 起解析期被静默丢弃（审计高危面恒空）；修复并加集成断言
- **侧栏插件组遮挡**：sticky 覆盖式改为流内 flex 固定项——长列表/底栏零遮挡（桌面 + 375px）
- **bg/daemon 会话 MCP 注入竞态**（#32）：会话配置占位文件原地原子重写 + 会话→路径映射回收
- 管理页 update 卡移除必败安装钮（更新流随 B4，当前明示「继续使用已安装版本」）

### 工单索引

- 本批：#31（B2 会话能力桥）；缺口 #32（bg daemon 注入竞态，已修复关单）；mux_proxy flake 稳定性追踪 #30
- 遗留（不阻断）：M9 高危声明一致性审计规则（B3 最高优先）/ M2 tool-invoke 高危对称化（B3）/ 拦截档钩子（B4，v1 明确不开）/ 更新确认流（B4）

## [0.6.0] - 2026-10-21

本批：插件体系 B1 地基（#27，epic agent-hub-mng#11，ADR-0001..0010 首批架构决策）+ 三项预览反馈增量，一次部署上线。

### Added — 插件体系 B1 地基（#27）

- **装载与生命周期**：本地目录 drop-in（`~/.claude-view/plugins/<id>/`，一层深扫描）+ `--plugin-dir` 单次加载；发现（启动 + 10s 周期 + 手动刷新）≠安装，安装前静态审计卡展示贡献物清单与权限触达面（凭据只显名，值零回显）；安装/停用/启用/卸载一律立即生效且提示一致；注册表唯一事实源（单写者 + 同插件操作串行 + install 幂等）；卸载默认不删插件目录
- **能力侧进程**：每插件一进程 sidecar（插件自带运行时，hub 不内嵌引擎）、懒启动 + 300s 空闲回收 + 握手 10s 超时、崩溃退避 3 次转故障态（原因可查、可「重试加载」整体重登）、进程组终止；stdio 换行 JSON-RPC 2.0（B1 能力面：initialize / tool.invoke / health）
- **权限与凭据**：按调用类别的判定矩阵——RPC 数据方法判 `data` 域、涉凭据再判 `credentials` 名、未声明即拒绝 + 违规留痕；凭据 hub 代持（`plugin_credentials.json` 0600）、插件按名引用、运行时注入、全局 scrub 兜底
- **插件视图**：侧栏「插件视图」导航组（sticky 常驻、零插件不渲染）+ 整页 iframe 沙箱宿主（不含 `allow-same-origin`）+ 票据门禁（32B 随机 / 120s / 单插件作用域，路径段形态，票据≠hub token）+ postMessage 固定消息集 + `--c-*` token 主题桥（三态跟随）+ 视图故障层（宿主不白屏）
- **插件管理页**：新发现区（审计通过/失败/待确认三卡型）、已安装清单（五态芯片、启停/卸载/重试、故障原因可查）、生效时机提示
- **示例插件 `gitlab-issues-lite`**：1 个 backend 工具（fetch_issues）+ 1 个原生视图；**绑定到工程**——项目下拉列 `hub.projects.effective`，backend 只读 `git remote get-url origin` 反解 GitLab host + namespace/repo（ssh/https 形态全支持），凭据按名注入拉取 issue 清单；零直调 `/api/*`（静态检查门禁锁定）
- **扩展 API 门禁**：`hub.apiVersion` 整数主版本相等才通过；三段白名单（贡献物 / RPC 方法表 / postMessage 消息集）；宿主侧 bundle 新增 +8.5KB ≤ 15KB 自约束
- **零回归**：无插件时导航集合 / 可发现能力集合 / 主体行为 / bundle 口径与引入前完全一致（四条断言随卸载自动执行）

### Added — 预览反馈增量（用户实测驱动，同批上线）

- **凭据管理面**：管理页对插件声明的凭据名「设置/清除」——password 框不预填旧值、上行即清、已配置/未配置即时翻转；端点零回显四通道断言（响应/清单/记录/日志）
- **绑定工程 + git remote 反解**：替代手输项目名与固定 gitlab.com 缺省（自建 GitLab 实例 401 的根治）
- **`hub.projects.effective` 可验证工程清单**：工程注册表 ∪ 全部 agent cwd 去重（带来源标注）——插件下拉自动反映日常真实工程，无需手工维护注册表

### 工单索引

- 本批：#27（插件体系 B1 地基，epic agent-hub-mng#11）
- 遗留（不阻断，随批次承接）：permissions 高危分支（B2 启用）、HUB_VERSION 随发版收敛、`hub:rpc-error` message 结构化（B3）、mux_proxy_end_to_end 高载偶发 flake（单列稳定性 issue）

## [0.5.0] - 2026-10-18

本轮四单批次（#15 / #18 / #19 / #21），一次部署上线。

### Added — 会话历史扩展到 omp（#15）

- `omp` agent 行 `⋯` 菜单开放「历史会话」：数据源 `~/.omp/agent/sessions/<slug>/*.jsonl`（复用既有只读扫描，目录绝不写），会话名取转录 `title` 行、退化为可读文本，时间取 session 行 timestamp
- 恢复复用既有 ACP `session/load`；端点按驱动插拔（claude 分支逐行未动）；**pi 与非 omp 的 ACP agent 行不显示入口**（不出现死入口）
- hub 内建的 omp 会话在侧栏是 `acp` 行——路由按登记 agent 名识别（omp 生态双行型归一）

### Added — 死会话原地恢复（#21）

- 从历史弹窗选中**已死**会话且当前选中 agent **空闲**时，向该 TUI 注入 `/resume <id>` **原地装载**：行不变、任务 id 不变、不新开行；消息视图跟随呈现历史转录，继续发送追加在原转录
- 路由表：运行中→切视图复用；忙（含 `needs_input` 挂起提问）→回退 `--bg --resume` 新行 + toast 说明；异驱动/异工程/无活 agent→spawn 回归
- 注入 sessionId 走 UUID 白名单（防命令注入向量）；watcher 跟随有界（15s，超时明确报错视图不动）；行名跟随会话标题

### Fixed — 历史会话选中不切视图（#18）

- 根因：服务端 `status:'current'`（后端无需动作）被前端误读为「前端无需切换」，非选中行选其当前会话静默无操作。修复=三态统一回抛目标并原子切换（选中 agent + 活跃会话 + 主区三者同成败），失败不切、toast 反馈
- **草稿跨切换保留**（按会话键存储，A→B→A 不丢不串味）；spawn 后立即失效 agents 查询（高亮延迟 5s→1.6s）；顺带修远程实例 instanceId 缺失导致的错路由

### Fixed — 实例离线判定语义重做（#19）

- 旧 `offline = 列表失败 ∨ 事件WS断 ∨ everErrored 闩锁` 全是管理面信号，与 agent 可达性无关——用户实测「显示离线但通讯正常」且整栏变暗无解释（像素取证：行文本 194→105）
- 新三态机 `online / degraded / offline`：**被动存活**（实例域 API 2xx/事件帧/authProbe）立即回在线（实测 4ms）；清单失败→`degraded`（琥珀重试横幅，行不置灰）；**仅**连续 ≥3 次且跨度 ≥20s 且无存活信号→`offline`（组头「离线」文案 + 置灰）；`everErrored` 闩锁废除，WS 单断只算降级

### 工单索引

- 本批：#15（omp 会话历史）/ #18（选中原子切换+草稿）/ #19（健康状态机）/ #21（死会话原地恢复）
- backlog：#20（Esc 不关菜单/弹窗 + 透明 backdrop 吞点击）/ #5（sidecar 假失败）/ #6（MCP prompts）/ #7（nodelink 透传）/ #8（缓存加固）；#19 遗留 MINOR 两条并入 #20 同族小批

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
