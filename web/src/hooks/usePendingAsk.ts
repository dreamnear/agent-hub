import type { ChatMessage } from '../types';

export interface PendingAsk {
  toolUseId: string;
}

/// 检测会话末尾是否存在未应答的 AskUserQuestion tool_use
/// （无对应 tool_result 应答）。P4 功能块 2。
export function detectPendingAsk(messages: ChatMessage[]): PendingAsk | null {
  const pending: string[] = [];
  for (const m of messages) {
    if (m.kind === 'tool_use' && m.toolName === 'AskUserQuestion') {
      pending.push(m.toolUseId ?? '');
    } else if (m.kind === 'tool_result' && m.toolUseId != null) {
      const i = pending.indexOf(m.toolUseId);
      if (i >= 0) pending.splice(i, 1);
    }
  }
  const id = pending[pending.length - 1];
  return id != null ? { toolUseId: id } : null;
}
