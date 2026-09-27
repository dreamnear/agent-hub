# desktop — Claude View 桌面壳（Tauri 2 sidecar）

P7 桌面打包：server release 二进制作 sidecar 嵌入 .app，壳启动即拉起 server
（`AGENT_HUB_BIND=127.0.0.1:7801`——7800 让给 web preview/frp 的 LAN 通路，两边共存）、
等端口就绪后开 WebView 窗口直连 `http://127.0.0.1:7801`；关窗即退出，
退出连坐杀 server（进程组 killpg，对齐 `drivers/acp/mod.rs` 模板）。

## 构建产物（macOS arm64）

```sh
# 1. CLI（一次性）：bun add -g @tauri-apps/cli
# 2. sidecar：release server → src-tauri/binaries/server-<triple>
./build-sidecar.sh
# 3. 图标（已有 icons/icon.png 时跳过）：python3 gen-icon.py && cd src-tauri && tauri icon icons/icon.png
# 4. 打包
cd src-tauri && tauri build
# 产物：src-tauri/target/release/bundle/macos/Claude View.app（+ macos/*.dmg）
```

## 运行语义

- sidecar 取 `.app/Contents/MacOS/server`（externalBin 打入）；裸跑壳时可用
  `AGENT_HUB_SERVER_BIN` 覆盖路径
- server 配置沿用既有 env 惯例（`AGENT_HUB_BIND` 等，见 crates/server config.rs），
  壳内显式清 `AGENT_HUB_ALLOW_LAN`（桌面壳固定 loopback 免认证形态）
- 7801 被占（如已有桌面版实例在跑）→ server bind 失败退出 → 壳检测后弹错退出，
  不会误连已占用端口的旧实例；7800 属 web/frp，与桌面版无关
- 不改 crates/server 业务代码、不改 web/，web 形态 `cargo run --release` 照常
