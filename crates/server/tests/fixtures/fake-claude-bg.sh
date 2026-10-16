#!/bin/bash
# 测试用 mock claude CLI（agent-hub#10 `--bg` 输出 id 解析专用）：回放
# claude 2.1.300 于 2026-10-15 实测的 stdout 形态——
#   带 --name：`backgrounded · 96bad8a7 · <name>` + 4 行提示尾巴（实测 5 行）
#   不带 --name：`backgrounded · 96bad8a7`
#   --resume：首行 note + `backgrounded · <id> · <name> (idle — …)`（#11 形态）
# logs/stop/rm 仅当 id 恰为有效短 hex id 时成功：id 被污染（含空格的整行、
# 名字、或其它 token）即非零退出，复现真实下游「id 不可用」的失败。
ID=96bad8a7
RESUME_ID=e5b57ea3
case "$1" in
  --bg)
    name=""
    resume=""
    prev=""
    for a in "$@"; do
      [ "$prev" = "--name" ] && name="$a"
      [ "$prev" = "--resume" ] && resume="$a"
      prev="$a"
    done
    if [ -n "$resume" ]; then
      echo "note: woke session ${resume} with its saved options (--name, --model)."
      echo "backgrounded · ${RESUME_ID} · hub-test-bgid-resume (idle — send a prompt to start)"
      echo "  claude agents             list sessions"
    elif [ -n "$name" ]; then
      echo "backgrounded · ${ID} · ${name}"
      printf '  claude agents             list sessions\n'
      printf '  claude attach %s    open in this terminal\n' "$ID"
      printf '  claude logs %s      show recent output\n' "$ID"
      printf '  claude stop %s      stop this session\n' "$ID"
    else
      echo "backgrounded · ${ID}"
    fi
    ;;
  logs|stop|rm)
    case "$2" in
      "$ID"|"$RESUME_ID") exit 0 ;;
      *)
        echo "fake-claude-bg: unusable id '$2'" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "fake-claude-bg: unknown command $1" >&2
    exit 1
    ;;
esac
