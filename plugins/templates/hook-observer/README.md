# hook-observer 模板（ADR-0014，B4）

生命周期钩子·观察档的最小可用样例：声明 `contributes.hooks` 订阅观察档钩子，
backend 经 stdio JSON-RPC notification（`hook/<钩子名>`，无 id 不等待）接收通知。

## 观察档名集（固定五项）

`onSessionCreate` / `onSessionClose` / `onMessageAppend` / `onIdle` / `onJobsChanged`

- 投递语义 **at-most-once**：每插件 FIFO 队列（上限默认 64，`[plugins] hook_queue_cap`
  可调），溢出丢弃 + 丢弃计数（管理页可见）；不重试。插件离线期间的事件即丢——
  需要完整流请自建轮询/对账（走声明面 RPC，如 `hub.sessions.list`）。
- 载荷最小化：`onMessageAppend` 默认只给 `{sessionId, seq}`；manifest 声明
  `messages:read` 才附 `{kind, text≤2000}`（经 hub 侧 scrub）。
- 发射时机（per-driver 覆盖矩阵见技术设计 §12.5，编码期校正）：
  claude=spawn 成功/stop/remove；acp=session/new|load 成功/release；
  opencode=serve 会话发现/消失（list 对账）；dsh=mux 会话发现/消失 + 房间推送点；
  pi=ensure_started/stop；onJobsChanged=HubEvent 单点转发；onIdle=忙→闲轮询翻转点
  （**best-effort + 去抖确认**：连续 3 次空观测 ≈15s 才判闲；claude CLI 高并发
  读降级为已知边界——详见仓库 `docs/plugins-distribution.md`）。
  dsh 的 onMessageAppend 仅在会话房间存在（前端打开）时发射——dsh 推送与房间
  绑定是既有 backlog 语义，B4 未改推送链路。

## 红线（写进实现契约，作者必读）

1. **观察档只读**：通知只是「告知」；插件据通知发起的一切 hub 调用照走声明面
   权限与高危确认——钩子不是绕行通道。
2. **拦截档 v1 不开**：`beforeMessageSend` / `onToolCall` / `afterToolResult` 不存在；
   这些名字出现在 manifest 任何字段 = 审计失败（ADR-0014 第 1 条）。

## 使用

```sh
cp -r plugins/templates/hook-observer ~/.claude-view/plugins/my-observer
# 改 plugin.json 的 id/name（id 须 2–40 字符 kebab-slug）
# 重启 hub 或管理页「扫描」→ 新发现 · 待确认 → 安装
```

## 视图/渲染页资产的外链要求（B3 观察项承接注）

视图与 plugin-iframe 渲染页运行在 sandbox iframe（CSP `connect-src 'none'`、
`script-src 'self'`）：**禁止引用外部 URL 资源**（脚本/样式/字体/API 皆然）；
一切资产必须随插件目录自持，网络访问只经宿主代理（`hub.rpc`）。外链会被
CSP 静默拦截（不报错、不渲染），审计期的硬编码色值扫描同样只覆盖自持资产。
