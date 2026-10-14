# claude-view · Agent Hub

[English](README.md) | [简体中文](README.zh-CN.md)

Agent Hub is a local-first web dashboard for background coding agents. It turns `claude --bg` style headless sessions into a manageable workspace: browse agents, chat with them, watch their git changes, read their files, open a terminal in their working directory — all from the browser.

Backend is a single Rust (axum) binary that embeds the React (Vite) frontend; build once, run anywhere with no external services.

## Features

- **Agents** — aggregated background sessions across projects: list / detail / create (with working-directory picker) / logs / stop / remove, live refresh. Chat view renders the full message stream (tool calls, diffs, queued messages); send is queued while the agent is busy.
- **Multi-driver** — a driver-agnostic model beyond Claude Code: opencode, dsh, omp sessions appear in the same workspace.
- **Git panel** — per-repository working-tree status and commit timeline, file list → side-by-side diff, image compare, submodule awareness.
- **Files panel** — project file tree rooted at the agent's working directory, content viewer with per-session tabs.
- **Terminal** — web terminal (PTY) opened in the agent's working directory, pure tool — it does not host the agent session.
- **Slash completion** — `/` menu with built-in commands, skills, and plugin commands (fuzzy search, grouped by source), mirroring the CLI TUI.
- **Assistant & cron** — a hub assistant session with in-chat scheduled jobs.
- **Access** — localhost is open by default; LAN/remote access is token-authenticated (QR-code sign-in), frp HTTPS guidance included. Remote instances are managed from the same UI.
- **UI** — three-state theme (auto / dark / light) on a design-token system, English/Chinese i18n, mobile-ready layout (≤768 px).

## Build

```bash
cd web && npm install && npm run build   # build frontend (web/dist)
cargo build --release                    # compile server (serves web/dist statically)
```

> ⚠️ After frontend changes, rebuild in this order: a plain `cargo build` will NOT re-embed the new `dist` (the release binary embeds a build-time snapshot) — the release script `touch`es `crates/server/src/static_assets.rs` to force recompilation.

One-step release gate (frontend + embedding + server, used for every delivery):

```bash
npm --prefix web run release
```

## Run

```bash
# development: server + vite dev separately (vite proxies /api and /ws to the server)
cargo run
npm --prefix web run dev

# production shape: single process serving web/dist
cargo run --release
# open http://127.0.0.1:7800
```

## Configuration (env overrides, defaults)

| Variable | Default |
|---|---|
| `AGENT_HUB_BIND` | `127.0.0.1:7800` |
| `CLAUDE_BIN` | `claude` |
| `JOBS_DIR` | `~/.claude/jobs` |
| `POLL_SECS` | `10` |
| `PROJECTS_FILE` | `~/.claude-view/projects.json` |
| `CLAUDE_PROJECTS_DIR` | `~/.claude` |
| `AGENT_HUB_ALLOW_LAN` | unset (localhost only, auth-free; `1`/`true` binds `0.0.0.0` and enforces token) |
| `AGENT_HUB_TOKEN` | unset (with `ALLOW_LAN`, generated on first start and persisted to `~/.claude-view/token`) |
| `AGENT_HUB_TOKEN_FILE` | `~/.claude-view/token` |

**LAN access**: start with `AGENT_HUB_ALLOW_LAN=1`, open `http://<host-ip>:7800` on the same WiFi, enter the token (local clients can read it at `/api/auth/token`; the page can generate a QR code that carries `?token=` for one-tap sign-in). Loopback access is always auth-free.

## Tests

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test
npm --prefix web exec -- vitest run
npm --prefix web run build && npx --prefix web tsc --noEmit
```

## Remote access (frp) security notes

- **Never set `local_ip` to 127.0.0.1** in frpc: when frpd and the Hub share a host, loopback traffic bypasses token auth entirely (localhost is trusted by design). Use the LAN IP together with `AGENT_HUB_ALLOW_LAN=1`.
- Terminate TLS at frps/the edge; the Hub itself speaks HTTP. A wildcard certificate is recommended.
- The token guards all API and WS routes (including query-parameter paths) with constant-time comparison; there are no cookies, so the CSRF surface is nil.
- QR sign-in: open the page locally → TokenGate → "generate QR code" → content already contains `?token=`.

See [docs/deploy-remote-instance.md](docs/deploy-remote-instance.md) for remote-instance deployment.

## Changelog

Releases are tagged `vX.Y.Z` on `main` after deployment; every version has an entry in [CHANGELOG.md](CHANGELOG.md). Semver-ish: features bump minor, fixes bump patch. Current baseline: **v0.2.0** (2026-10-13).

## License

See [LICENSE](LICENSE).
