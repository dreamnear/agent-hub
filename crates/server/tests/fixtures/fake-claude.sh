#!/bin/bash
# 测试用 mock claude CLI：agents --json 回放 fixture，--bg 返回固定 short id，
# logs/stop/rm 静默成功，attach 读 stdin 到文件供断言透传与 kill 验证
case "$1" in
  agents)
    # 真实 CLI 语义：--all = 全量（含 daemon 已死的退出态条目）；缺省 = 活跃清单
    # （活 daemon 才列，done+活 daemon 在列）。两份 fixture 分文件回放。
    all_flag=0
    for a in "$@"; do [ "$a" = "--all" ] && all_flag=1; done
    if [ "$all_flag" = "1" ]; then
      cat "$(dirname "$0")/agents.json"
    else
      cat "$(dirname "$0")/agents-active.json"
    fi
    ;;
  --bg)
    echo "abc123"
    ;;
  logs|stop|rm)
    exit 0
    ;;
  respawn)
    # 记录 respawn 调用供断言（Done 会话 send 复活链路，2026-10-02）
    echo "$2" >> "$(dirname "$0")/respawn-calls.log"
    exit 0
    ;;
  attach)
    # 真实 claude attach 是 TUI：raw mode（stdin 由应用接管）+ 回显键入文本。
    # agent-hub-first-send-ready：首发确认层以「回显可见且终端已 raw」为消费证据，
    # canonical 态行规程回显不算——故本 fake 必须 stty raw 后自行回显（tee），
    # 否则等价于黑洞会话，send 会被判失败。
    # 反馈轮 28-A：ensure_tui_ready 以 has_output（drain 读到过字节）为前提——
    # 先打印一行模拟 TUI 启动渲染（真实 TUI 会立即出 banner），否则注入门正确拒绝
    ID="${2:-unknown}"
    echo "[fake-tui ready]"
    stty raw -echo
    tee "$(dirname "$0")/attach-stdin-$ID.txt"
    ;;
  *)
    echo "fake-claude: unknown command $1" >&2
    exit 1
    ;;
esac
