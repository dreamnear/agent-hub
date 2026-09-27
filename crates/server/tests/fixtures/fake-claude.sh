#!/bin/bash
# 测试用 mock claude CLI：agents --json 回放 fixture，--bg 返回固定 short id，
# logs/stop/rm 静默成功，attach 读 stdin 到文件供断言透传与 kill 验证
case "$1" in
  agents)
    cat "$(dirname "$0")/agents.json"
    ;;
  --bg)
    echo "abc123"
    ;;
  logs|stop|rm|respawn)
    exit 0
    ;;
  attach)
    # 注意：真实 claude attach 是 TUI，raw mode 下只认 \r 为提交；本 fake 是 cat 直读，
    # 不模拟 TUI 语义，仅验证 stdin 透传（\r 语义锚点在 api/messages.rs 发送注释）
    # 反馈轮 28-A：ensure_tui_ready 以 has_output（drain 读到过字节）为前提——
    # 先打印一行模拟 TUI 启动渲染（真实 TUI 会立即出 banner），否则注入门正确拒绝
    ID="${2:-unknown}"
    echo "[fake-tui ready]"
    cat > "$(dirname "$0")/attach-stdin-$ID.txt"
    ;;
  *)
    echo "fake-claude: unknown command $1" >&2
    exit 1
    ;;
esac
