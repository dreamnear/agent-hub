#!/usr/bin/env python3
"""first-send-ready 单测 fake CLI（agent-hub-first-send-ready）。

模拟 `claude` 的 attach/agents 两面对话，并把 PTY 读出**原样回显**（TUI 应用回显的
等价物）——首发确认层以「回显可见」为消费证据，故 fake 必须能按场景控制回显与消费。

模式由 **id 后缀**决定（id 即测试场景名，避免并行测试共享 env 互踩）：
  fs-ready       立即 raw + 回显（正常路径）
  fs-window<N>   前 N 秒不读 stdin（canonical + ECHO：行规程会回显，但无人消费），
                 随后 `setraw(TCSAFLUSH)` —— 实测机制：转 raw 时 flush 掉窗口期
                 未读输入（reports/.../experiments.md §A），此后回显
  fs-blackhole   raw 但不回显（消费了却零证据 = 黑洞，无法确认投递）

每个读到的 chunk repr 追加到 `$FS_FAKE_DIR/fs-fake-<id>.log` 供断言。
`agents --json` 回放 `$FS_FAKE_IDS` 里的 id（含全部场景 id），使 send 守卫
（job_needs_respawn）判定会话在清单内、不触发 respawn。
"""
import json
import os
import sys
import time
import tty

args = sys.argv[1:]
cmd = args[0] if args else ""

if cmd == "agents":
    ids = [i for i in os.environ.get("FS_FAKE_IDS", "").split(",") if i]
    print(json.dumps([{"id": i, "state": "running", "kind": "background"} for i in ids]))
    sys.exit(0)

if cmd in ("respawn", "stop", "rm", "logs", "--bg"):
    sys.exit(0)

if cmd != "attach":
    sys.stderr.write("first_send_fake: unknown command %r\n" % cmd)
    sys.exit(1)

tid = args[1] if len(args) > 1 else "unknown"
log_path = os.path.join(os.environ.get("FS_FAKE_DIR", "/tmp"), "fs-fake-%s.log" % tid)
mode = "ready"
window = 0.0
if tid.endswith("blackhole"):
    mode = "blackhole"
elif "-window" in tid:
    mode = "window"
    window = float(tid.rsplit("-window", 1)[1])

sys.stdout.write("[fake-tui ready] %s\r\n" % tid)
sys.stdout.flush()
t_start = time.monotonic()

if mode == "window":
    # 窗口期：不读 stdin、不改终端模式 → 父进程能看到行规程回显（这正是误导来源）
    time.sleep(window)
    # init 收尾：转 raw（TCSAFLUSH）→ 窗口期未读输入被内核丢弃
    tty.setraw(0, tty.TCSAFLUSH)
else:
    tty.setraw(0, tty.TCSANOW)

while True:
    try:
        data = os.read(0, 4096)
    except OSError:
        break
    if not data:
        break
    with open(log_path, "a") as f:
        f.write("%.3f %r\n" % (time.monotonic() - t_start, data))
    if mode != "blackhole":
        os.write(1, data)  # 应用回显（TUI 渲染）
