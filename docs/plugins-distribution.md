# agent-hub 插件分发指南（B4，ADR-0008）

> 面向插件使用者与作者的分发面说明。架构语义以 ADR-0008（分发档位）/ ADR-0007
> （信任与权限）/ ADR-0014（生命周期钩子）为正本；本文只讲「怎么装、怎么更、
> 怎么治理」。数据根 = `~/.claude-view/`（env 覆盖惯例统一）。

## 目录布局

```
~/.claude-view/
├── plugins/<id>/                # 插件本体（drop-in 位；一层深、不递归）
├── plugins/.versions/<id>/<hash>/ # 版本副本（B4 更新流的恢复源，保留 3 份；
│                                   独立于插件根，扫描一层深不误入）
├── plugins.json                 # 注册表持久态（0600）
├── plugin_credentials.json      # 凭据代持（0600，值零回显）
├── plugin_audit/<id>-<hash>.json# 审计留档（每插件保留 10 份，可配）
├── plugin_data/<id>/            # 插件自持数据（卸载默认保留）
└── plugin_marketplaces.json     # 索引市场源清单（0600）
```

## 安装方式（四档）

### 1. drop-in 目录（始终可用）

把插件目录放进 `~/.claude-view/plugins/<id>/` → 管理页「扫描」→ 出现在
「新发现 · 待确认」→ 审计卡核对贡献物与权限面 → 确认安装。

### 2. `--plugin-dir <path>`（临时加载，开发用）

可重复传参；不写注册表持久层，重启即失。

### 3. git 直装（B4）

`POST /api/plugins/install-git {repoUrl, subPath?}`（v1 为 API 面；索引市场的
「获取」按钮即走同一条流，UI 直输 URL 入口随后续迭代补齐）。

- URL 可带 `#<ref>`（tag / branch / commit sha）；缺省 = 默认分支 head；
- 支持 `https://` 与本机绝对路径；**ssh / http 明文 / 私有仓库认证 v1 不支持**
  （复用本机 git 凭据，hub 不代持 git token）；
- 流程：clone（120s 超时，临时目录用后即清）→ 定位 `plugin.json` → 静态审计
  → 落盘 `plugins/<id>/` → 进「新发现/更新待确认」确认面。**落盘 ≠ 安装**，
  装不装仍由你确认（可审计态 = 装前看得见全部贡献物与权限面）。

### 4. 索引市场（B4）

`marketplace.json` 仓库索引：hub 不托管代码、不自建 registry——索引源由你
显式添加（git 仓库或本地文件，根含 `marketplace.json`）：

```json
{
  "name": "my-index",
  "plugins": [
    { "id": "demo", "name": "Demo", "description": "…",
      "repoUrl": "https://…/repo.git", "subPath": "plugins/demo",
      "gitRef": "v1.2.0", "tags": ["tools"] }
  ]
}
```

管理页：添加索引源 → 浏览/检索（本地过滤）→「获取」触发 git 直装 → 照走
确认流。索引条目只是元数据，安装前的审计与确认不因来源省略。

## 更新与保持旧版（B4，BR-13）

- 事实源 = 插件目录内容 hash；`manifest.version` 是展示值。
- 目录内容变化（手工替换 / git 重装）→ 扫描即入「更新待确认」：新版副本入库
  （`plugins/.versions/<id>/<hash>/`），**插件目录恢复为已装版本**——确认前
  旧版持续可用（backend 懒启动跑的代码与注册表一致）。
- 审计卡展示**差异面**：贡献物增删 + 权限面变化（扩权红标）。
- 「应用更新」：副本再审计（hash 防篡改）→ 目录替换 → 立即生效；
  「保持旧版」：清待确认态 + 删新版副本。两个决定都进插件记录（可追溯）。
  **注意**：不要直接手改插件运行目录的内容——「保持旧版」会用旧副本回滚覆盖
  目录（M31 提示）；更新请经 git 重装 / drop-in 新版。
- 版本副本保留 3 份；B4 之前安装的存量插件首次更新无旧副本，目录无法回滚
  （注册表快照仍保证能力面/权限面不漂移）——确认过一次更新后恢复保证闭环。

## 审计产物治理（B4）

- 审计留档按每插件保留最近 10 份（`[plugins] audit_keep`，0 = 关闭自动清理），
  扫描期自动清理；管理页「清理审计产物」按钮手动触发并报清理数。
- 卸载时勾选「清除自持数据」会同时清掉该插件的全部审计留档与版本副本
  （`purgeData=true`）。

## 生命周期钩子（B4，ADR-0014 观察档）

- 插件声明 `contributes.hooks: [...]`（名集固定五项：`onSessionCreate` /
  `onSessionClose` / `onMessageAppend` / `onIdle` / `onJobsChanged`），backend
  经 stdio JSON-RPC notification（`hook/<钩子名>`，无 id 不等待）接收通知。
- **onIdle 为 best-effort 观察语义**：探针轮询（5s）+ 去抖确认（连续 3 次空
  观测 ≈15s 才判定忙→闲翻转）——单次空观测（读降级形态，如 claude CLI 高并
  发读在 list 内部静默降级空清单）不触发翻转。已知边界：并发读竞争下探针侧
  可能持续读空而 API 侧正常，此时 onIdle 缺席；需要确定性的请经声明面 RPC
  （如 `hub.sessions.list`）自建轮询。
- **at-most-once**：每插件 FIFO 队列（上限默认 64，`[plugins] hook_queue_cap`
  可调），溢出丢弃 + 丢弃计数（`/api/plugins` 的 `hookStats.dropped`，管理页
  可见）；不重试、不回压会话链路。插件离线期间的事件即丢——需要完整流请经
  声明面 RPC 自建轮询对账（如 `hub.sessions.list`，B4 已注册只读投影）。
- **载荷最小化**：`onMessageAppend` 默认只给 `{sessionId, seq}`；声明
  `messages:read` 才附 `{kind, text≤2000}`（scrub 后）。
- **拦截档 v1 不开**：`beforeMessageSend` / `onToolCall` / `afterToolResult`
  不存在；这些名字出现在 manifest 任何字段 = 审计失败。钩子处理中发起的
  一切 hub 调用照走权限域与高危确认——钩子不是绕行通道。
- 模板：`plugins/templates/hook-observer/`；类型：`plugins/plugin-types/`
  （`@agent-hub/plugin-types` 参考包，权威面 = `/api/plugins/api-schema`）。

## 明确不做（epic 范围，ADR-0008 §4）

归档包安装 / 自建 registry / 签名校验 / 自动更新 / 插件间依赖。
