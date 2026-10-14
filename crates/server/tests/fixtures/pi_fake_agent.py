#!/usr/bin/env python3
"""Fake pi RPC agent（agent-hub-pi-hub-agent P1 传输层测试用）。

用法：python3 pi_fake_agent.py <mode> [log_file]
mode:
  basic          - get_state 应答 model=claude-fake；prompt 受理后推
                   agent_start → message_end(user) → message_end(assistant) → agent_end
  resume         - basic 基础上，get_messages 应答预置历史两条（user+assistant，
                   assistant-resume 回灌链路验证）；prompt 消息同 basic
  exit-on-start  - 启动立即 exit 3，不发任何协议帧（restart-feedback BLOCKER
                   回修：spawn Ok 但 RPC 握手立即挂的失败面回写验证）
  ui-dialog      - basic 基础上，assistant 完成前发 extension_ui_request(confirm)
                   并阻塞等 hub 应答（记录收到的应答，测自动取消通路）
  api-error      - prompt 应答 success:false + error（模拟凭据缺失）
  crash-on-prompt- prompt 命令到达即退出码 70（测 send 上抛与 Dead 收场）
  abort-aware    - prompt 受理后慢速流式；收到 abort 命令即应答并推 agent_end 后退出
环境变量:
  PI_FAKE_LOG - 事件追加写入该文件（供测试断言收到的命令/应答）；
                也可用第 2 个 argv 指定（并行测试优先，避免全局 env 竞态）
"""
import json
import os
import sys

MODE = sys.argv[1] if len(sys.argv) > 1 else "basic"
LOG = os.environ.get("PI_FAKE_LOG") or (sys.argv[2] if len(sys.argv) > 2 else None)


def log(obj) -> None:
    if LOG:
        with open(LOG, "a") as f:
            f.write(json.dumps(obj) + "\n")


def send(obj) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def respond(req_id, command, success=True, data=None, error=None) -> None:
    resp = {"id": req_id, "type": "response", "command": command, "success": success}
    if data is not None:
        resp["data"] = data
    if error is not None:
        resp["error"] = error
    send(resp)


def run_prompt(req_id, message) -> None:
    """受理 + 完整事件序列（basic 形态）。"""
    respond(req_id, "prompt", success=True)
    send({"type": "agent_start"})
    # 真 pi 0.73.1 的 user content 是 blocks 数组（非 string）——对齐实测形态
    send(
        {
            "type": "message_end",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": message}],
                "timestamp": 1,
            },
        }
    )
    if MODE == "ui-dialog":
        # dialog 类 UI 请求：阻塞等应答（hub 应自动取消，助手不被卡死）
        send(
            {
                "type": "extension_ui_request",
                "id": "ui-1",
                "method": "confirm",
                "title": "Proceed?",
                "timeout": 30000,
            }
        )
        while True:
            line = sys.stdin.readline()
            if not line:
                sys.exit(0)
            msg = json.loads(line)
            if msg.get("type") == "extension_ui_response":
                log({"ui_response": msg})
                break
    send(
        {
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "fake thought"},
                    {"type": "text", "text": "FAKE-REPLY"},
                ],
                "stopReason": "stop",
                "timestamp": 2,
            },
        }
    )
    send({"type": "agent_end", "messages": []})


def main() -> None:
    # D5：记录 pi 配置目录 env（spawn 注入断言用；所有模式统一记录）
    log({"env_PI_CODING_AGENT_DIR": os.environ.get("PI_CODING_AGENT_DIR")})
    if MODE == "exit-on-start":
        sys.exit(3)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmd = json.loads(line)
        except Exception:
            continue
        ctype = cmd.get("type")
        log({"command": ctype, "id": cmd.get("id")})
        if ctype == "get_state":
            respond(
                cmd.get("id"),
                "get_state",
                data={
                    "model": {"id": "claude-fake", "provider": "fake"},
                    "isStreaming": False,
                    "sessionFile": "/tmp/fake.jsonl",
                },
            )
        elif ctype == "get_messages":
            # 泵启动即问全量历史（assistant-resume 回灌）：默认空（新会话语义）；
            # resume 模式回预置历史（形态对齐真 pi AgentMessage：blocks content）
            msgs = []
            if MODE == "resume":
                msgs = [
                    {
                        "role": "user",
                        "content": [{"type": "text", "text": "早前问题"}],
                        "timestamp": 1,
                    },
                    {
                        "role": "assistant",
                        "content": [{"type": "text", "text": "早前回答"}],
                        "stopReason": "stop",
                        "timestamp": 2,
                    },
                ]
            respond(cmd.get("id"), "get_messages", data={"messages": msgs})
        elif ctype == "prompt":
            if MODE == "api-error":
                respond(cmd.get("id"), "prompt", success=False, error="No API key found for fake.")
            elif MODE == "crash-on-prompt":
                sys.exit(70)
            elif MODE == "abort-aware":
                respond(cmd.get("id"), "prompt", success=True)
                send({"type": "agent_start"})
                # 慢速流式：等 abort（测试在收 message_end 前发 abort）
                while True:
                    got = sys.stdin.readline()
                    if not got:
                        sys.exit(0)
                    abort = json.loads(got)
                    log({"command": abort.get("type")})
                    if abort.get("type") == "abort":
                        respond(abort.get("id"), "abort", success=True)
                        send({"type": "agent_end", "messages": []})
                        sys.exit(0)
            else:
                run_prompt(cmd.get("id"), cmd.get("message", ""))
        elif ctype == "abort":
            respond(cmd.get("id"), "abort", success=True)
        else:
            respond(cmd.get("id"), ctype or "unknown", success=True)
    sys.exit(0)


main()
