# 远程实例接入部署指南（agent-hub 多实例）

本文说明把一台远程主机的 agent-hub server 接入本机仪表盘的部署要求与两种连接方式
（direct / ssh-tunnel）的配置。安全红线：**远程实例 token 等于该机器全部 agent 会话的
完全控制权**（可 stop/rm/发消息、可读含账密便签），明文暴露公网不可接受。

## 远程实例侧部署要求（三要素，缺一不可）

1. **`AGENT_HUB_ALLOW_LAN=1`**——监听 `0.0.0.0` 并**强制 token 认证**（不开则只听
   loopback，远程接不进来）。
2. **token 强制**——allow_lan 下所有非 loopback 请求都要求 token；推荐
   `AGENT_HUB_TOKEN=<强随机串>` 显式设置。未显式设置时 server 首启会随机生成并
   持久化到 `~/.claude-view/token`（不会裸奔，但接入前需读取该文件取值）。
3. **传输加密**——direct 模式必须 https（frp 已带证书即满足）；或改用 ssh-tunnel
   模式（SSH 加密内建，无需证书）。

启动示例（远程主机）：

```bash
AGENT_HUB_ALLOW_LAN=1 \
AGENT_HUB_TOKEN=<REDACTED> \
AGENT_HUB_BIND=0.0.0.0:7801 \
~/.local/bin/claude-view-server   # 实际二进制路径按部署为准
```

## Linux arm64 二进制获取与常驻

远程主机为 Linux aarch64（树莓派 / ARM 服务器）时：拿到二进制 → 装 systemd → 装
claude CLI。产物 **musl 静态链接**，无 glibc 版本依赖，任何 Linux arm64 直接跑。

### a. 一键安装（推荐：`install.sh`）

`deploy/install.sh` 自动探测 OS（linux/darwin）+ 架构（x86_64/aarch64），选对应二进制
装到 `/usr/local/bin`（不可写且无免密 sudo 时回退 `~/.local/bin`）。**只装服务端，不装
桌面壳 / tauri 任何产物。**

```bash
# 1) 把 install.sh + 对应架构二进制传到远程（产物在 dist/）
scp deploy/install.sh dist/claude-view-server-linux-aarch64 user@remote:~/hub-install/
# 2) 远程执行（--with-systemd 顺带装单元并 enable --now）
ssh user@remote 'cd ~/hub-install && ./install.sh --with-systemd'
```

- **二进制来源**二选一：① 同目录放 `claude-view-server-{os}-{arch}`（脚本目录或
  `./dist/` 子目录都认）；② 设 `RELEASE_BASE_URL=<分发 URL 前缀>` 由脚本 `curl` 下载
  （无默认源，留待日后自建分发）。
- **已产出**：`claude-view-server-linux-aarch64`（arm64 远程）、`claude-view-server-darwin-aarch64`
  （本机）。**x86_64 尚未构建**——脚本会明确报「本机需要 …-linux-x86_64 但目录内没有该
  文件」，扩展方式见 b 节把 `--platform` 换成 `linux/amd64`。
- **安全默认**：脚本不自动放开 LAN，只打印两档启动命令（loopback 免认证 / 显式
  `AGENT_HUB_ALLOW_LAN=1` + token 由用户选）；已有 `~/.claude-view/token` 不覆盖，重复
  执行安全。
- `--prefix DIR` 可自定义安装目录（自测/无 root 场景）。

不想用脚本时，手动等价三行：

```bash
sudo install -m755 claude-view-server-linux-aarch64 /usr/local/bin/claude-view-server
sudo cp claude-view-server.service /etc/systemd/system/     # 先改 User= 与 AGENT_HUB_TOKEN=
sudo systemctl daemon-reload && sudo systemctl enable --now claude-view-server
```

单元文件 `deploy/claude-view-server.service`：`User=` 必须是已 `claude` 登录的用户；
`AGENT_HUB_TOKEN=` 填强随机串（`openssl rand -hex 32`），留 `CHANGE_ME` 则首启自生成并
持久化到 `~/.claude-view/token`。验证：`systemctl status claude-view-server` +
`curl http://127.0.0.1:7801/health`。

**macOS 远程（场景少见）**：安装动作相同（装到 `/usr/local/bin` 或 `~/.local/bin`），
但 `--with-systemd` 不生效——常驻走 launchd：把 `claude-view-server` 写成 LaunchAgent
plist（`EnvironmentVariables` 设 `AGENT_HUB_*`、`RunAtLoad=true`、`KeepAlive=true`）
后 `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist`。仓库暂无现成
plist，需要时按此生成。

### b. 产物从哪来 / 怎么重编译

本机 macOS 产出（arm64 容器内**原生**编译 musl 目标，需 Docker；不加 `--target` 时宿主
三元组即 `aarch64-unknown-linux-musl`，产物落在 `release/` 而非 `<target>/release/`）：

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/app -v /tmp/hub-arm64-target:/build/target \
  -w /app -e CARGO_TARGET_DIR=/build/target rust:1-alpine \
  sh -c 'cargo build -p server --release --locked &&
         cp /build/target/release/server dist/claude-view-server-linux-aarch64'
```

x86_64 换 `--platform linux/amd64`、输出名 `-linux-x86_64`。挂 `/tmp/hub-arm64-target`
只为跨次复用编译缓存（首次约 3 分钟）。server 依赖无 openssl/native-tls，musl 目标零
依赖改动。

### c. 远程原生编译（备选）

远程需先装 rust 工具链（`https://sh.rustup.rs`）与 node（前端产物 `web/dist` 被
gitignore，rust-embed 编译期需要它）：

```bash
cd web && npm ci && npm run build && cd ..   # 先建前端
cargo build -p server --release              # 产物 target/release/server
sudo install -m755 target/release/server /usr/local/bin/claude-view-server
```

小树莓派上全量编译较慢（10 分钟级），优先 a / b。

### d. claude CLI（server 依赖它 spawn 会话）

远程主机还需安装 claude CLI，并以 unit / install.sh 运行的用户**登录一次**（登录态落在
`~/.claude`，server spawn 的子进程依赖它）：

```bash
npm install -g @anthropic-ai/claude-code
claude   # 交互式登录一次
```

### e. token 获取（接入本机仪表盘时用）

- `AGENT_HUB_TOKEN` 已显式设置 → 直接读该值；
- 未设置让 server 自生成 → `ssh user@remote 'cat ~/.claude-view/token'`。

## frp 侧建议（direct 模式）

- frps 开 `auth.token`，frpc 配同 token——防 frp 端口被扫后直连。
- 有条件时在 frps 侧加 IP 白名单（`allowPorts` / 防火墙），只放行常用出口。
- frpc 的 `https` 插件带证书（用户环境已就绪）；证书过期 = 实例离线，按
  「实例管理」连接测试排查。
- 远程实例对公网只暴露 frp 端口，`AGENT_HUB_BIND` 不必再加防火墙例外。

## 方式一：direct（直连 frp 域名）

「实例管理 → 新建实例」：

| 字段 | 值 |
|------|-----|
| 模式 | direct |
| URL | `https://hub.example.com`（frp 域名；**远程非 https 拒绝保存**，本机 http 例外） |
| Token | 远程实例的 `AGENT_HUB_TOKEN` |

填完点「连接测试」（打 `/health`）再保存。`wss://` 前缀也接受，保存时自动归一化为
`https://`。

## 方式二：ssh-tunnel（hub server 代建 SSH 端口转发）

适合没有公网证书、但可 SSH 登录的机器。原理：本机 hub server spawn
`ssh -N -L 127.0.0.1:<localPort>:127.0.0.1:<remotePort>`，前端访问
`http://127.0.0.1:<localPort>`，SSH 加密内建。

「实例管理 → 新建实例」：

| 字段 | 值 |
|------|-----|
| 模式 | ssh-tunnel |
| SSH 主机 / 端口 / 用户 | 远程主机（端口默认 22） |
| 认证方式 | 三选一（见下） |
| 远程目标端口 | 远程 server 的端口（如 7801） |

**认证方式取舍**：

1. **证书路径（正路）**——`ssh -i` 等价；密钥须已加进远程主机 `authorized_keys`。
2. **authsock（正路）**——复用本机 `SSH_AUTH_SOCK` 的 agent；适合已有 ssh-agent 场景。
3. **密码（兜底）**——依赖 `sshpass`（经环境变量传给子进程，不进 argv 不落盘）；
   **macOS 不预装**，缺失时保存可成功但启动隧道会报引导性错误，安装：
   `brew install hudochenkov/sshpass/sshpass`（或改用证书/authsock）。

隧道生命周期由 server 管理：断线指数退避自动重连（上限 5 次，耗尽后前端探测会自动
重新拉起）；删除实例先停隧道；server 停机回收全部 ssh 子进程。

## 安全说明

- `~/.claude-view/instances.json` 存实例配置（含 token / ssh 密码），**权限 0600**。
- `GET /api/instances` 会把各实例 token 下发给浏览器——浏览器直连方案的固有属性，
  信任域等同 hub token（能开仪表盘 = 能控制全部已接入实例）。不可接受时的升级路径是
  联邦网关（hub 代理聚合），本期未实现。
- CORS 仅 `allow_lan=true` 时对 `/api`、`/ws` 前缀放行；Bearer 认证无 cookie，不开
  allow-credentials；loopback 单机模式零 CORS，行为与多实例之前完全一致。
