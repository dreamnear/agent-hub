import type { ChatMessage } from '../types';

/// 最新一条未应答的 ACP 权限请求（批3 任务11）：acp_permission 消息，
/// 且流内没有同 permId 的 acp_permission_resolved 回执。null = 无挂起请求。
export function detectPendingPermission(messages: ChatMessage[]): ChatMessage | null {
  const resolved = new Set(
    messages
      .filter((m) => m.rawType === 'acp_permission_resolved')
      .map((m) => m.toolUseId),
  );
  const pending = messages.filter(
    (m) => m.rawType === 'acp_permission' && m.toolUseId != null && !resolved.has(m.toolUseId),
  );
  return pending.length > 0 ? pending[pending.length - 1] : null;
}
