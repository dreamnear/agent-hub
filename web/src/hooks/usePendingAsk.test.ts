// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../types';
import { detectPendingAsk } from './usePendingAsk';

function msg(partial: Partial<ChatMessage>): ChatMessage {
  return {
    kind: 'user',
    rawType: null,
    text: null,
    toolUseId: null,
    toolName: null,
    input: null,
    result: null,
    error: null,
    ts: null,
    ...partial,
  };
}

describe('detectPendingAsk', () => {
  it('flags AskUserQuestion without matching tool_result', () => {
    const msgs = [
      msg({ kind: 'tool_use', toolName: 'AskUserQuestion', toolUseId: 'call_1' }),
    ];
    expect(detectPendingAsk(msgs)).toEqual({ toolUseId: 'call_1' });
  });

  it('clears flag after matching tool_result', () => {
    const msgs = [
      msg({ kind: 'tool_use', toolName: 'AskUserQuestion', toolUseId: 'call_1' }),
      msg({ kind: 'tool_result', toolUseId: 'call_1' }),
    ];
    expect(detectPendingAsk(msgs)).toBeNull();
  });

  it('tracks multiple interleaved asks, last unanswered wins', () => {
    const msgs = [
      msg({ kind: 'tool_use', toolName: 'AskUserQuestion', toolUseId: 'call_1' }),
      msg({ kind: 'tool_use', toolName: 'AskUserQuestion', toolUseId: 'call_2' }),
      msg({ kind: 'tool_result', toolUseId: 'call_1' }),
    ];
    expect(detectPendingAsk(msgs)).toEqual({ toolUseId: 'call_2' });
  });

  it('ignores non-ask tool_use', () => {
    const msgs = [msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 'call_b' })];
    expect(detectPendingAsk(msgs)).toBeNull();
  });
});
