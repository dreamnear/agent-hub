#!/usr/bin/env python3
"""Fake ACP agent（hub 传输层测试用，批1 任务2-4）。

用法：python3 acp_fake_agent.py <mode>
mode:
  basic          - initialize/session/new 正常应答；prompt 推 5 类 update（含未知类型）后应答 end_turn
  permission     - basic 基础上，prompt 时先反向请求 session/request_permission，记录收到的应答
  permission-multi - prompt 时连续两个权限请求（id 9001/9002，逐个等应答），测多请求排队/逐卡应答
  crash-on-prompt - prompt 请求到达即退出码 70（模拟子进程异常退出，测 pending 清理）
  fork-child     - basic 基础上先 fork 一个 `sleep 30` 孙进程（测进程组 kill）
  v2             - initialize 应答 protocolVersion 99（测版本协商失败路径）
环境变量:
  ACP_FAKE_LOG - 事件追加写入该文件（供测试断言反向请求应答等）；
                 也可用第 3 个 argv 指定（并行测试优先，避免全局 env 竞态）
"""
import json
import os
import subprocess
import sys

MODE = sys.argv[1] if len(sys.argv) > 1 else "basic"
LOG = os.environ.get("ACP_FAKE_LOG") or (sys.argv[2] if len(sys.argv) > 2 else None)


def log(obj):
    if LOG:
        with open(LOG, "a") as f:
            f.write(json.dumps(obj) + "\n")


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def emit_updates(session_id):
    # 四类已知 + 一类未知（宽松忽略路径）；omp 实测还会推 thought/usage/info，协议同构
    for kind, extra in [
        ("agent_message_chunk", {"content": {"type": "text", "text": "chunk0"}}),
        ("tool_call", {"toolCallId": "tc1", "title": "Fake Tool", "kind": "edit"}),
        ("tool_call_update", {"toolCallId": "tc1", "status": "completed"}),
        ("plan", {"entries": [{"content": "step", "priority": "medium", "status": "pending"}]}),
        ("unknown_xyz_update", {"payload": {"weird": True}}),
    ]:
        update = {"sessionUpdate": kind}
        update.update(extra)
        send(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"sessionId": session_id, "update": update},
            }
        )


def ask_permission(req_id, session_id, title):
    """反向请求权限 + 阻塞等 hub 应答 + 记录（多模式共用）。
    等待期间收到通知帧（如 session/cancel）或 EOF 即退出（模拟 omp 停止）。"""
    send(
        {
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session_id,
                "toolCall": {"toolCallId": "tc%d" % req_id, "title": title},
                "options": [
                    {"optionId": "opt-allow", "name": "Allow", "kind": "allow_once"},
                    {"optionId": "opt-reject", "name": "Reject", "kind": "reject_once"},
                ],
            },
        }
    )
    while True:
        line = sys.stdin.readline()
        if not line:
            sys.exit(0)
        msg = json.loads(line)
        if "method" in msg:
            sys.exit(0)
        log({"permission_answer": msg, "req_id": req_id})
        return


def handle(req):
    method = req.get("method")
    if method == "initialize":
        version = 99 if MODE == "v2" else 1
        send(
            {
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {
                    "protocolVersion": version,
                    "agentCapabilities": {},
                    "agentInfo": {"name": "fake-acp", "version": "0.0.1"},
                    "authMethods": [],
                },
            }
        )
    elif method == "session/new":
        log({"session_new_params": req.get("params")})
        send(
            {
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {
                    "sessionId": "fake-session-1",
                    "configOptions": [],
                },
            }
        )
    elif method == "session/load":
        # 批3 任务13：loadable 模式支持恢复（同 id 返回 + 重放一条 chunk）；
        # 其他模式走兜底 method-not-found（模拟 omp 不支持）
        if MODE == "loadable":
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "result": {
                        "sessionId": req["params"]["sessionId"],
                        "configOptions": [],
                    },
                }
            )
            send(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": req["params"]["sessionId"],
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "replayed"},
                        },
                    },
                }
            )
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "error": {"code": -32601, "message": "method not found: session/load"},
                }
            )
    elif method == "session/prompt":
        sid = req["params"]["sessionId"]
        if MODE == "permission":
            ask_permission(9001, sid, "risky op")
        elif MODE == "permission-multi":
            ask_permission(9001, sid, "touch a.txt")
            ask_permission(9002, sid, "touch b.txt")
        emit_updates(sid)
        send(
            {
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {"stopReason": "end_turn", "usage": {"outputTokens": 3}},
            }
        )
    elif "id" in req:
        send(
            {
                "jsonrpc": "2.0",
                "id": req["id"],
                "error": {"code": -32601, "message": "method not found: %s" % method},
            }
        )


def main():
    if MODE == "fork-child":
        # 孙进程常驻：hub shutdown 必须进程组连坐清除
        subprocess.Popen(["sleep", "30"])
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            continue
        if req.get("method") == "session/cancel":
            log({"cancelled": True})
            sys.exit(0)
        if MODE == "crash-on-prompt" and req.get("method") == "session/prompt":
            sys.exit(70)
        handle(req)
    sys.exit(0)


main()
