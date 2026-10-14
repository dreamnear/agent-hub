# CLAUDE.md

claude-view（Agent Hub，Rust axum + React）源码仓的 Claude Code 指引。工程全量规则（需求流程、主题 token、端口分级等）在管理仓 `agent-hub-mng/CLAUDE.md`，本文件只列本仓日常要点。

## 流程规则（2026-10-13/14 用户立规，强制）

1. **main 禁直推**：分支（`feat/*` `fix/*` `docs/*` `release/*`）→ push 分支 → `gh pr create` → `gh pr merge` → 本地 `git checkout main && git merge --ff-only origin/main`（服务端分支保护已生效）
2. **提交必须关联 issue**：消息含 `#编号`（本仓）或 `agent-hub-mng#编号`（跨仓）；`.githooks/commit-msg` 强制（clone 后执行 `git config core.hooksPath .githooks`）
3. **英文为工作语言**（2026-10-14 起）：commit message（标题祈使句）、issue、PR/issue comment 一律英文；2026-10-14 历史重置前的中文消息不追溯
4. **CHANGELOG**：每个版本（`vX.Y.Z` tag）必须给出条目；tag 只打部署后的 main head。基线：2026-10-14 历史重置后的初始提交（v0.2.0 重挂于此）
5. 需求/BUG 先记 issue：`gh issue create -R dreamnear/agent-hub`

## 工程要点

- **发布门禁**：`web/` 下 `npm run release`（vite build → touch static_assets.rs → cargo build --release，脚本串联勿拆）；bin 内嵌构建期 dist 快照，前端改动不重跑门禁不会生效
- **bundle 预算**：app 页 JS gz <300KB（`gzip -9 | wc -c` 口径）；报数与产物同源
- **主题 token**：一切色值走 `--c-*`（`web/src/App.css` + `bui/theme.css` 映射），禁 `bg-white`/`#hex` 等绕过类
- **消息数据层**（combineToolCalls/lib/ansi）是行为基线，UI 改动不碰
- **密钥零回显**：配置文件只打键名与结构，值打码
- 深层规则（端口分级、kill 甄别、产物新鲜度、测试靶子纪律等）见管理仓 `agent-hub-mng/CLAUDE.md`
