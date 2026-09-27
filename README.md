# agent-hub

Self-hosted dashboard for **Claude Code** and **ACP** coding agents — aggregate local and remote background agents in one web UI, chat with them live, approve permission prompts, and take notes per project.

Single Rust binary (frontend embedded), no external services. Runs on your machine, talks to the `claude` CLI you already have.

## Features

- **Multi-instance aggregation** — manage several hub servers (local + remote) from one dashboard; instances can connect direct (https) or via SSH tunnel, with offline detection and auto-reconnect
- **Live agent sessions** — spawn / attach / chat with `claude --bg` background tasks; tool calls, diffs, thinking blocks and task notifications rendered as structured cards
- **Permission approval cards** — approve or deny tool permission requests inline, including cross-session (subagent) prompts
- **Sticky notes** — per-project notes with cross-instance isolation (a project's notes follow the project, per instance)
- **File & git panels** — browse workspace docs and git state of agent projects
- **Remote-friendly security** — token auth (constant-time compare, covers API + WebSocket), LAN mode with QR-code pairing, https enforcement for remote instances
- **Desktop shell** — optional Tauri 2 `.app` wrapper bundling the server as a sidecar (macOS aarch64)

## Quick start

### One-line install (prebuilt binary)

```bash
curl -fsSL https://raw.githubusercontent.com/dreamnear/agent-hub/main/deploy/install.sh | bash
```

Downloads the matching `claude-view-server-{os}-{arch}` from GitHub Releases to `/usr/local/bin` (falls back to `~/.local/bin`). On Linux, add `--with-systemd` to also install and enable a systemd unit. Requires the Claude Code CLI (`npm install -g @anthropic-ai/claude-code`) logged in for the user running the server.

### Run from source

```bash
# 1. build the frontend (embedded into the binary)
cd web && npm install && npm run build && cd ..

# 2. build & run the server
cargo run --release
# open http://127.0.0.1:7800
```

> The server embeds `web/dist` at compile time (rust-embed). After frontend changes, rebuild the frontend **before** `cargo build`, and `touch crates/server/src/static_assets.rs` to force re-embedding on incremental builds.

### Desktop app (macOS)

See [desktop/README.md](desktop/README.md) — builds a Tauri `.app` that spawns the server as a sidecar on `127.0.0.1:7801`.

## Configuration (env vars)

| Variable | Default | Notes |
|---|---|---|
| `AGENT_HUB_BIND` | `127.0.0.1:7800` | listen address |
| `AGENT_HUB_ALLOW_LAN` | unset | set `1`/`true` to listen on `0.0.0.0` **and enforce token auth** |
| `AGENT_HUB_TOKEN` | unset | auth token; auto-generated on first LAN start and persisted |
| `AGENT_HUB_TOKEN_FILE` | `~/.claude-view/token` | token persistence path |
| `CLAUDE_BIN` | `claude` | CLI binary to spawn |
| `JOBS_DIR` | `~/.claude/jobs` | background job state |
| `CLAUDE_PROJECTS_DIR` | `~/.claude` | session/project data root |
| `PROJECTS_FILE` | `~/.claude-view/projects.json` | UI project registration |
| `POLL_SECS` | `10` | job poll interval |

`localhost` access is always auth-free. For LAN/remote access the token is mandatory — read it at `/api/auth/token` on the host, or scan the QR code from the login page.

## Remote instances

To attach a remote host's hub server to your dashboard (direct https or SSH tunnel), see [docs/deploy-remote-instance.md](docs/deploy-remote-instance.md) — covers Linux arm64 (musl static binary + systemd), token requirements, and frp exposure safety notes.

## Development

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test   # Rust
npm --prefix web exec -- vitest run                              # web unit tests
npm --prefix web run build && npx --prefix web tsc --noEmit       # web build + types
```

## License

Apache-2.0. The `web/src/components/bui/` components are ported from [Beautiful UI](https://beautifului.dev) (MIT) — see [web/NOTICE](web/NOTICE).
