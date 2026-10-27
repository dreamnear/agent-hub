# CHANGELOG

本文件记录 agent-hub（claude-view）每个版本的显著变更。**版本规则（2026-10-13 立）**：版本 = main head 上的 `vX.Y.Z` tag（部署/发布后打）；语义化——新特性 +0.1.0，缺陷修复 +0.0.1；**每个版本必须给出条目**，缺条目不得打 tag。

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.9.2] - 2026-10-28

本批：P1 稳定性收口批（#45/#20/#30/#5）——行为缺陷与测试噪声一次清账。

### Fixed — ACP 占位行根修（#45）

- **ACP 注册表行移除通道**：`/rm` 端点对 acp 键可用（204）+ ⋯ 菜单「删除会话」项；移除同步原子删除 `acp_sessions.json` 持久化条目，重启不复活
- **驱动占位会话不再进用户可见面**：hub 拉起的 omp acp 驱动进程的零消息占位条目在恢复路径剪除（创建路径保持可见可移除）；omp 历史发现面（侧栏/历史/探测三面同源）过滤零消息转录
- **转录卫生**：hub 不再 eager spawn omp acp——无占位行、无转录追加

### Fixed — Esc 键控与 backdrop（#20）

- 新 `useEscapeKey` hook（document 级，焦点在 body 也生效）统一接管行 ⋯ 菜单 / 历史会话弹窗 / ⌘K 切换器三处 Esc 关闭
- 透明 backdrop 随层卸载——关闭后的同一次点击直达下层真实元素（几何 + 事件双断言）
- #19 两条 MINOR：ChatTab 挂 ErrorBoundary（畸形 payload 不再白屏）+ 选中快照兜底（error 态重探不再重挂 composer 清草稿）
- 顺带收紧：侧栏「删除会话」项仅 claude/acp 行显示（omp 文件行等基线误显修正）

### Test — 测试稳定性（#30/#5）

- #30：mux_proxy 测试泵时序放宽为停顿容忍（5ms/80ms → 250ms/15s/2s，**断言语句零改动**）——治理口径 10/10 全绿 + 8 核负载 3/3
- #5：desktop sidecar 不再覆盖 server 测试二进制（desktop 移出默认 workspace members + `build.rs` 构建期刷新）——全仓 `cargo test --no-fail-fast` 零 node_cli 假失败。**披露**：desktop 4 例单测退出默认门禁面，显式口令 `cargo test -p claude-view-desktop`
- 沿途修复存量隔离缺口：四套件补 opencode/dsh 隔离（本机真 opencode 下全仓 6 例确定性 500，基线 d7bb369 对照实锤非本批引入；/api/agents 对真 opencode 的 500 另立 #51）

### 工单索引

- 本批：#45 / #20 / #30 / #5；关联 #19（MINOR 清账）、#51（opencode 500，后续批）
- 门禁：cargo -p server 641/0 · 全仓 657/0 · vitest 680/680 · tsc 干净 · bundle gz 235,823B ≤300KB

## [0.9.1] - 2026-10-27

本批：hub 侧 claude spawn 全数静默失败的紧急修复（#48）——定时任务 spawn、UI 新建、恢复流全部受累。

### Fixed — hub spawn 的 claude 会话 init 即崩（#48）

- **现象**：定时任务 spawn / UI 新建会话在 CLI 2.1.309 下 hub 记 ok 但会话永不出现（无转录、daemon 无限 claimed-spare→crashed 循环）
- **根因**：CLI 解析器把 `--mcp-config <file>` 后尾随的位置参数 prompt 吞成该开关的第二个值（按 cwd 相对路径解析）→ `Invalid MCP configuration: MCP config file not found: <cwd>/<prompt>` → init 即崩；失败发生在 CLI daemon 内部，hub 视野之外
- **修复**：`--mcp-config` 改等号连写形态（`--mcp-config=<file>`，实测 spawn 正常到 working），prompt 保持尾随位置参数；回归锁钉死形态与参数序
- 触发面：B2 插件桥启用插件时即注入 `--mcp-config`——此前所有经 hub 的 claude 新建 spawn 均受累

### 工单索引

- 本批：#48（spawn 参数形态修复）；关联 #32（CLI 2.1.309 缺陷首次记录于 B4 现场注记）

## [0.9.0] - 2026-10-24

本批：插件体系 B4 终批（#41，epic agent-hub-mng#11）——生命周期钩子观察档 + 会话只读投影 + 治理与分发面一次性收口。

### Added — 生命周期钩子（观察档）+ 会话只读投影（#41）

- **5 个观察档钩子**：onSessionCreate / onSessionClose / onMessageAppend / onIdle / onJobsChanged——事后通知、只读、at-most-once（队列 cap 64 + 溢出/失败丢弃计数，管理页 hookStats 可见）、fire-and-forget、载荷最小化（onMessageAppend 默认 {sessionId,seq}，需 messages:read + scrub + 截断）；卸载/停用清队列；**拦截档 v1 明确不开**（beforeMessageSend/onToolCall/afterToolResult 审计硬失败 + dispatch 名集门 + notification 无等待，红线六项全仓搜证闭环）
- **会话只读投影**：hub.sessions.list + hub.sessions.messages（hub.rpc 可发现面，只读；hub.sessions.send v1 永久不做，配有 unregistered 锚单测）
- **观察档语义边界**：onIdle = 全 hub 空闲翻转（全局 working 归零）+ IdleFlipDetector 去抖状态机（单次空观测吞掉、连续 3 次 ≈15s 确认窗才翻转、忙重置、未武装不发射，4 态纯单测锁 + 武装行为实机）；实机终验受共享环境制约（全局 CLI daemon working 恒 >0 不可构造归零窗），留专项 #42 受控环境执行；并发读假设受控 18/18 实验证伪
- **五驱动埋点对齐**：claude/omp/pi/dsh/opencode 的 create/close/jobs/hooks 探针闭包埋点（opencode/dsh 的 known_sessions 辅助抽 reconcile_session_hooks；dsh 三处拷贝→helper），per-driver running_count 容错聚合

### Added — 治理与分发（#41）

- **更新/扩权完整流**：hash 变化 → 待确认（差异面：贡献物增删 + 权限面扩权红标）→ 应用/保持旧版；`plugins/.versions/<id>/<hash16>/` 旧版恢复 + 保留 3 份；dismissed 账本（id+hash 精确对，hash 变化/重装即解除）；apply 的副本再审计 + hash 再校验防 TOCTOU；自撞豁免；更新前保留旧版语义；更新确认语境下 `oldVersionAvailable` 权威字段；B4 前存量插件首更新 pendingHint 按位分支文案（无旧副本场景不宣称「旧版可用」）
- **git 直装 + 索引市场**：https + 本机绝对路径白名单（http/ssh/相对路径拒装）；clone 120s 超时 + 临时目录两路清理；URL 凭据形态级脱敏（含单引号包裹）；subPath 禁 `..`/绝对路径；marketplace.json 源管理（300s 缓存 + force）、条目 id 预检、错误 scrub、零外部网络依赖
- **审计产物清理**：按每插件保留 10 份自动清理 + 管理页按量清点回报 + 卸载随清 + dismissed 防重生锁
- **模板补齐与示例优化**：hook-observer 模板（backend/main.js + plugin.json + README）入插件模板体系；gitlab-issues-lite 示例增 hook 演示

### Fixed — 存量收口

- **B3 承接单全清**：O-1 双留痕收敛（decide_approval 单点）；#32 治理留痕；观察项三条（自撞豁免/no_contrib 10 类/模板外链）；M14；M17 门禁口径注记
- **审查小钉**：M21 泵注释如实化 / M22 存量 pendingHint 分支 / M23 CONTRIBUTION_TYPES 6→10（ADR-0011 单一定义源）/ M25 分发文档树层级 / M26 语义类 `--c-*` token 样式 / M27 报告链口径（coder 499 vs tester 637 注记）/ M29 `--c-muted`→`--c-sub` / M30 无旧副本文案不矛盾 / M31 live 目录手改回滚提示

### 工单索引

- 本批：#41（B4 终批，hooks 观察档 + sessions 只读 + 治理分发）
- 专项：#42（onIdle 实机翻转证据，受控环境执行）
- 列后续：M26 视觉走查 / M28 实机证据（随 #42）/ M24 dsh 已合入；proxy 9 例 socket 竞争偶发为环境噪音（基线同现，#30 家族另行治理）

## [0.8.1] - 2026-10-23

本批：文档树自动刷新缺陷修复（#36）+ green-text 暗色对比度口径修正（B3 存量欠账随批闭环）。

### Fixed — 文档树不随文件变化自动刷新（#36）

- agent 在工程里新建/删除文件后，文档树现在会**自动出现/移除**（面板挂载期对已展开目录 20s 周期重取，错误时停止轮询防空转）
- 折叠再展开目录 = 手动刷新（立即反映最新清单）；未展开目录仍按需懒加载（大仓语义不变）
- 打开中的文件被外部修改后，内容同样随周期更新
- 修复实现：`toggleDir` 首载守卫移除 + `staleTime` 清零 + 定时器随组件卸载清理（附单测与回归锁）

### Fixed — green-text 暗色对比度口径（B3 存量欠账闭环）

- 暗色态 `--c-green-text` 由 `var(--c-done)` 字面化为 `#10a37f`，并纳入语义配对表校验
- 说明：运行时对比度实测 5.61:1（达标）；此前的「3.37:1 不达标」系配对表校验器不解析 `var()` 引用链的**误报**——字面化后校验口径与运行时一致，观感零变化

### 工单索引

- 本批：#36（文档树自动刷新）
- 遗留：proxy 测试依赖远端实例的预存环境失败（单跑基线核对可过，随 #30 家族另行治理）

## [0.8.0] - 2026-10-23

本批：插件体系 B3 前端家族（#35，epic agent-hub-mng#11）——渲染器 / 主题 / 装饰 / 视图 API 一次上线；B1/B2 遗留承接单清零。

### Added — 前端家族扩展点（#35）

- **内容渲染器（声明档）**：插件在 manifest 声明「格式→宿主内置引擎」，宿主转译进 docRender 注册表（**插件零代码进宿主**）；首批引擎 mermaid（动态 chunk、strict 安全级、离屏容器 + 产物清扫）/ plantuml-server / image / html-sanitized；match 全 hub 唯一（跨插件冲突审计拒装）、内置 id 禁覆盖；渲染服务地址一律用户设置（manifest 内嵌 URL = 审计失败）；失败/超时/未配置三径回退源码展示（不假渲染）
- **内容渲染器（iframe 档，ADR-0012 双轨）**：复杂渲染由插件自带沙箱页承载——`DeclarativeIframeRenderer`（票据 + 无 `allow-same-origin`、零宿主上下文执行）+ 三条消息契约 + IntersectionObserver 懒挂载 + 15s 超时；纯渲染器插件（无 views）资产正常服务
- **主题 token 覆盖**：插件提交覆盖表（139 个 `--c-*` 白名单、亮暗成对）；语义配对表 4.5:1 硬校验不过**拒装**（报具体对与比例）；同刻仅一个主题插件（单槽互斥含启动残留确定性规则）；停用即恢复原主题、三态自动跟随
- **文件装饰**：扩展名映射装饰（SVG 经 `<img>` 票据路由，零 inline 注入面；装饰色 3:1 非文字标准）
- **视图 API 完整化**：`view.data` 持久化（示例插件绑定迁移验收）+ `hub.notes.read/write` + `hub.tasks.list/toggle`（toggle 走高危审批门）；CSS 硬编码色值扫描正式化（`:root` 兜底行豁免）+ 视图体积展示

### Fixed — B1/B2 遗留承接单清零

- M9：`risk:high` 工具缺 `highRisk` 声明现在审计拒装（补齐 ADR-0001 拍板 4）
- M2：tool-invoke 高危调用跨驱动对称走审批（F-12 回归锁在位）
- M7：permissions 高危死逻辑清理对账
- M1：mcp_proxy 会话级实例复用（OnceCell，无 per-call 泄漏）
- 途中修复：manifest `Contributes` 缺 camelCase 解析（与 B2 `Permissions` 同款缺陷，三连前科已入记忆）；iframe 档资产路径口径三层统一（F-14）；commit 阴影清除（F-15）；mermaid 错误横幅泄漏（F-7）

### 工单索引

- 本批：#35（B3 前端家族）
- B4 承接：O-1 双留痕收敛 / #32 残留竞态语义 / sessions 族只读投影（send v1 永久不做，主会话裁决留痕）/ green-text 暗色 3.37:1 基线色值修正（随下个 patch）/ 观察项三条

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
