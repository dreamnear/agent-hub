#!/usr/bin/env node
// hook-observer 模板 backend（stdlib only，Node ≥18）。
//
// 契约（ADR-0014，B4）：
// - hub → 插件的钩子是 JSON-RPC **notification**（`hook/<钩子名>`，无 id、不等待应答）；
// - at-most-once：不重试、可能丢（队列满/插件离线期间）；需要完整流请自建轮询对账；
// - 观察档只读：通知只是「告知」，插件据通知发起的任何 hub 调用照走声明面权限
//   与高危确认——钩子不是绕行通道；
// - 拦截档（beforeMessageSend / onToolCall / afterToolResult）v1 不存在，
//   manifest 出现这些名字会直接审计失败。
const readline = require('readline').createInterface({ input: process.stdin });

const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
const log = (line) => process.stderr.write(`[hook-observer] ${line}\n`);

const hookCounts = {};

readline.on('line', (line) => {
  let f;
  try {
    f = JSON.parse(line);
  } catch {
    return; // 非法帧静默丢弃（hub 侧护栏兜底）
  }
  if (f.method === 'initialize') {
    send({ jsonrpc: '2.0', id: f.id, result: { ok: true, capabilities: ['hooks'] } });
    return;
  }
  if (f.method === 'health') {
    send({ jsonrpc: '2.0', id: f.id, result: { ok: true } });
    return;
  }
  if (f.id !== undefined) {
    send({ jsonrpc: '2.0', id: f.id, error: { code: -32601, message: 'unknown method' } });
    return;
  }
  // 钩子通知：method 形如 hook/onSessionCreate，无 id
  if (typeof f.method === 'string' && f.method.startsWith('hook/')) {
    const hook = f.method.slice(5);
    hookCounts[hook] = (hookCounts[hook] ?? 0) + 1;
    // 观察档演示动作：只写 stderr 日志（hub 侧采集并脱敏）。
    // 需要持久化/查询 hub 数据时走 hub.* 请求方向（声明面权限内）。
    log(`hook ${hook} #${hookCounts[hook]} ${JSON.stringify(f.params ?? {})}`);
  }
});
