# claude-view · Agent Hub

[English](README.md) | [简体中文](README.zh-CN.md)

Agent Hub 是一个本地优先的后台编码 agent 仪表盘：把 `claude --bg` 式无头会话变成可管理的工作区——浏览 agent、对话、查看 git 变更、阅读文件、在工作目录开终端，全部在浏览器完成。

后端是单个 Rust（axum）二进制，内嵌 React（Vite）前端；一次构建，无外部服务依赖。

## 功能

- **Agents**：跨项目聚合后台会话——列表 / 详情 / 新建（带工作目录选择器）/ logs / stop / rm，实时刷新。聊天视图渲染完整消息流（工具调用、diff、排队消息）；agent 忙时发送自动排队。
- **多驱动**：驱动无关的统一模型——opencode、dsh、omp 会话与 Claude Code 同工作区呈现。
- **Git 面板**：按仓库的工作区状态与提交 Timeline，文件清单 → 左右分栏 diff，图片对照，子仓感知。
- **文件面板**：以 agent 工作目录为根的文件树，内容查看按会话多 Tab。
- **终端**：在 agent 工作目录打开的网页终端（PTY），纯工具定位——不承载 agent 会话。
- **`/` 补全**：内置命令、技能、插件命令的补全菜单（模糊搜索、按来源分组），与 CLI TUI 对齐。
- **助手与定时**：hub 助手会话，支持会话内定时任务。
- **访问**：localhost 默认免认证；局域网/远程访问走 token 认证（扫码进入），附 frp HTTPS 指南；远程实例同一 UI 管理。
- **UI**：三态主题（auto / dark / light，design token 体系）、中英 i18n、移动端适配（≤768px）。

## 构建

```bash
cd web && npm install && npm run build   # 构建前端（web/dist）
cargo build --release                    # 编译 server（静态托管 web/dist）
```

> ⚠️ 前端改动后必须按上述顺序重建：rust 未改动时 cargo 增量编译不会重嵌新 dist（release 二进制内嵌的是构建期快照）。交付门禁脚本会 `touch crates/server/src/static_assets.rs` 强制重编译。

一步到位的发布门禁（前端 + 内嵌 + server，每次交付使用）：

```bash
npm --prefix web run release
```

## 运行

```bash
# 开发：分别起 server + vite dev（vite 代理 /api、/ws 到 server）
cargo run
npm --prefix web run dev

# 发布形态：单进程托管 web/dist
cargo run --release
# 打开 http://127.0.0.1:7800
```

## 配置（env 覆盖，默认值）

| 变量 | 默认 |
|---|---|
| `AGENT_HUB_BIND` | `127.0.0.1:7800` |
| `CLAUDE_BIN` | `claude` |
| `JOBS_DIR` | `~/.claude/jobs` |
| `POLL_SECS` | `10` |
| `PROJECTS_FILE` | `~/.claude-view/projects.json` |
| `CLAUDE_PROJECTS_DIR` | `~/.claude` |
| `AGENT_HUB_ALLOW_LAN` | 未设（仅 localhost 免认证；设 `1`/`true` 监听 `0.0.0.0` 并强制 token） |
| `AGENT_HUB_TOKEN` | 未设（allow_lan 时首启随机生成并持久化到 `~/.claude-view/token`） |
| `AGENT_HUB_TOKEN_FILE` | `~/.claude-view/token` |

**局域网访问**：`AGENT_HUB_ALLOW_LAN=1` 启动后，手机（同 WiFi）打开 `http://<本机IP>:7800`，输入 token（本机访问 `/api/auth/token` 可查，页面内可生成二维码扫码携带 token 直接进入）。localhost 始终免认证。

## 测试

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test
npm --prefix web exec -- vitest run
npm --prefix web run build && npx --prefix web tsc --noEmit
```

## 远程访问（frp）安全指南

- **frpc 的 `local_ip` 勿填 127.0.0.1**：frpd 与 Hub 同机时，loopback 流量在 Hub 看来来自本机——**直接绕过 token 认证**（localhost 直通设计）。必须用局域网 IP 并配合 `AGENT_HUB_ALLOW_LAN=1`。
- TLS 终结在 frps/边缘；Hub 自身是 HTTP，建议泛域名证书。
- token 覆盖全部 API 与 WS（含 query 参数路径），常量时间比较；无 cookie，CSRF 面天然免疫。
- 手机扫码：本机打开页面 → TokenGate「生成二维码」→ 内容已含 `?token=`，扫码即带凭证进入。

远程实例部署见 [docs/deploy-remote-instance.md](docs/deploy-remote-instance.md)。

## 更新日志

版本以 `vX.Y.Z` tag 打在部署后的 main head；每个版本在 [CHANGELOG.md](CHANGELOG.md) 必有条目。语义化：特性 +minor、修复 +patch。当前基线 **v0.2.0**（2026-10-13）。

## 许可

见 [LICENSE](LICENSE)。
