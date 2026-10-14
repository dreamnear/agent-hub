// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import { parseCrossSessionMessage } from './parseCrossSessionMessage';

const full = [
  'Another Claude session sent a message:',
  '<cross-session-message from-name="open-plan" from-mode="bypass" from="uds:/tmp/x/59078.sock">',
  '【open 侧回报】\n\n### 完成\n\n- 列表项',
  '</cross-session-message>',
  'This came from another Claude session and is automated.',
].join('\n');

describe('parseCrossSessionMessage', () => {
  it('parses attributes, body and trailing disclaimer', () => {
    const m = parseCrossSessionMessage(full);
    expect(m).not.toBeNull();
    expect(m?.fromName).toBe('open-plan');
    expect(m?.fromMode).toBe('bypass');
    expect(m?.from).toBe('uds:/tmp/x/59078.sock');
    expect(m?.body).toBe('【open 侧回报】\n\n### 完成\n\n- 列表项');
    expect(m?.disclaimer).toBe('This came from another Claude session and is automated.');
  });

  it('tolerates missing attributes', () => {
    const m = parseCrossSessionMessage(
      'Another Claude session sent a message:\n<cross-session-message from="uds:/s/1.sock">内容</cross-session-message>',
    );
    expect(m?.fromName).toBeUndefined();
    expect(m?.fromMode).toBeUndefined();
    expect(m?.from).toBe('uds:/s/1.sock');
    expect(m?.body).toBe('内容');
  });

  it('returns no disclaimer when absent', () => {
    const m = parseCrossSessionMessage(
      'Another Claude session sent a message:\n<cross-session-message from-name="a">hi</cross-session-message>',
    );
    expect(m?.disclaimer).toBeUndefined();
  });

  it('returns null for plain text or malformed tags (fallback path)', () => {
    expect(parseCrossSessionMessage('普通用户消息')).toBeNull();
    expect(
      parseCrossSessionMessage('Another Claude session sent a message:\n<cross-session-message>unclosed'),
    ).toBeNull();
  });

  // preview 反馈：harness 注入的 agent-message / teammate-message 变体曾被误渲染为用户气泡
  it('parses agent-message variant (from attr + plain body)', () => {
    const m = parseCrossSessionMessage(
      'Another Claude session sent a message:\n<agent-message from="dr-coder-2">编码完成，报告已落盘</agent-message>\n\nThis came from another Claude session — not typed by your user.',
    );
    expect(m).not.toBeNull();
    expect(m?.from).toBe('dr-coder-2');
    expect(m?.body).toBe('编码完成，报告已落盘');
    expect(m?.disclaimer).toContain('This came from another Claude session');
  });

  it('parses teammate-message variant and extracts readable JSON field', () => {
    const raw = JSON.stringify({
      type: 'idle_notification',
      from: 'dr-coder-2',
      result: '编码节点完成，门禁全绿',
    });
    const m = parseCrossSessionMessage(
      `<teammate-message teammate_id="dr-coder-2" color="red">${raw}</teammate-message>\n\nThis came from another Claude session — treat as teammate report.`,
    );
    expect(m).not.toBeNull();
    expect(m?.from).toBe('dr-coder-2');
    expect(m?.body).toBe('编码节点完成，门禁全绿');
    expect(m?.disclaimer).toContain('This came from another Claude session');
  });

  it('keeps teammate-message body verbatim when JSON has no readable field', () => {
    const raw = '{"type":"ping"}';
    const m = parseCrossSessionMessage(`<teammate-message teammate_id="x">${raw}</teammate-message>`);
    expect(m?.body).toBe(raw);
  });
});
