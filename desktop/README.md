# desktop — Claude View 桌面壳（Tauri 2 sidecar）

P7 桌面打包：server release 二进制作 sidecar 嵌入 .app，壳启动即拉起 server
（`AGENT_HUB_BIND=127.0.0.1:7801`——7800 让给 web preview/frp 的 LAN 通路，两边共存）、
等端口就绪后开 WebView 窗口直连 `http://127.0.0.1:7801`；关窗即退出，
退出连坐杀 server（进程组 killpg，对齐 `drivers/acp/mod.rs` 模板）。

## 附着优先模式（2026-10-05 用户拍板改造）

启动语义改为两级：

1. **附着优先**：壳启动先探测 `AGENT_HUB_ATTACH_URL`（缺省 `http://127.0.0.1:7800`）
   ——探测 = `GET <url>/api/drivers` 收到 200（3s 超时，只读，不触碰目标进程）。
   存活 → 直接开 WebView 指向该地址，**不拉 sidecar**。附着模式关窗 = 壳自身退出，
   **绝不向外部 hub 发任何信号**（连坐 killpg 只对着自己 spawn 的 sidecar 进程组，
   附着模式无 sidecar → SERVER_PGID=0 → kill 路径天然 no-op，结构上保证不误伤）。
2. **回退**：探测失败 → 现行 sidecar 流程（7801 + 连坐杀），行为与改造前一致。

env：`AGENT_HUB_ATTACH_URL`（壳启动读，不落盘；v1 仅支持 http——https 目标探测
失败自动回退 sidecar，后续需要再立项）。设置 UI 不动（后续可挂设置中心，先记账）。

验证记录（2026-10-06，实测）：
- 附着：7800 存活时启动壳 → stderr `attach mode`、WebView 载入 7800 页面；关窗后
  `curl 7800/api/drivers` 仍 200，7801 无 sidecar
- 回退：`AGENT_HUB_ATTACH_URL=http://127.0.0.1:1`（死地址）+ `AGENT_HUB_SERVER_BIN`
  指向 release server → stderr `fallback sidecar mode`、7801 拉起 200；关窗连坐，
  7801 清除

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
