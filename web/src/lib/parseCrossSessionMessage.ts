// 跨会话消息解析（P5 preview 反馈）：user 文本中被注入的其他会话消息标签
// → 结构化对象；非注入消息或解析失败返回 null（调用方回退原渲染路径，不抛错）。
// harness 注入形态随版本演进过三种标签：cross-session-message / agent-message /
// teammate-message（preview 反馈：后两种曾被误渲染为用户气泡）。

export interface CrossSessionMessage {
  fromName?: string;
  fromMode?: string;
  from?: string;
  body: string;
  disclaimer?: string;
}

const DISCLAIMER_PREFIX = 'This came from another Claude session';
const TAG_RE = /<(cross-session-message|agent-message|teammate-message)([^>]*)>/;

export function parseCrossSessionMessage(text: string): CrossSessionMessage | null {
  const open = TAG_RE.exec(text);
  if (!open) return null;
  const [, tag, attrSrc] = open;
  const close = `</${tag}>`;
  const closeIdx = text.indexOf(close);
  if (closeIdx === -1) return null;

  const raw = text.slice(open.index + open[0].length, closeIdx).trim();
  if (!raw) return null;

  const attr = (name: string): string | undefined =>
    new RegExp(`${name}="([^"]*)"`).exec(attrSrc)?.[1];

  // teammate-message 的 body 常为 JSON 信封（idle_notification 等）——提取可读字段展示，
  // 解析失败或无可读字段时原样展示（宁可丑也不丢内容）
  let body = raw;
  try {
    const j = JSON.parse(raw) as Record<string, unknown>;
    const pick = ['result', 'summary', 'message'].find(
      (k) => typeof j?.[k] === 'string' && (j[k] as string).trim(),
    );
    if (pick) body = j[pick] as string;
  } catch {
    // 非 JSON，原样
  }

  // 尾部英文免责声明（如有则与 body 分离）
  const after = text.slice(closeIdx + close.length).trim();
  const disclaimer = after.startsWith(DISCLAIMER_PREFIX) ? after : undefined;

  return {
    fromName: attr('from-name'),
    fromMode: attr('from-mode'),
    from: attr('from') ?? attr('teammate_id'),
    body,
    disclaimer,
  };
}
