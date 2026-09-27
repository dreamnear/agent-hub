import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../types';
import { detectPendingPermission } from './usePendingPermission';

const perm = (toolUseId: string, title = 'touch a.txt'): ChatMessage => ({
  kind: 'other',
  rawType: 'acp_permission',
  text: null,
  toolUseId,
  toolName: title,
  input: null,
  result: null,
  error: null,
  ts: null,
});

const resolved = (toolUseId: string, text = '已应答: opt-allow'): ChatMessage => ({
  kind: 'other',
  rawType: 'acp_permission_resolved',
  text,
  toolUseId,
  toolName: null,
  input: null,
  result: null,
  error: null,
  ts: null,
});

describe('detectPendingPermission', () => {
  it('无权限请求 → null', () => {
    expect(detectPendingPermission([])).toBeNull();
    expect(detectPendingPermission([resolved('perm:s:1')])).toBeNull();
  });

  it('已应答（回执在流内）→ 不再视为挂起（应答/超时/取消均出回执）', () => {
    const msgs = [perm('perm:s:1'), resolved('perm:s:1', '等待超时，已自动拒绝（不默认放行）')];
    expect(detectPendingPermission(msgs)).toBeNull();
  });

  it('多个未应答请求 → 取最新一张（排队展示，不互相覆盖）', () => {
    const msgs = [perm('perm:s:1'), perm('perm:s:2')];
    expect(detectPendingPermission(msgs)?.toolUseId).toBe('perm:s:2');
  });

  it('最新卡应答后 → 露出更早的挂起卡（队列推进，每张可独立应答）', () => {
    const msgs = [
      perm('perm:s:1'),
      perm('perm:s:2'),
      resolved('perm:s:2'),
    ];
    expect(detectPendingPermission(msgs)?.toolUseId).toBe('perm:s:1');
  });

  it('同 id 重复卡与回执配对：全部有回执 → null，不再弹', () => {
    const msgs = [perm('perm:s:1'), perm('perm:s:1'), resolved('perm:s:1')];
    expect(detectPendingPermission(msgs)).toBeNull();
  });
});
