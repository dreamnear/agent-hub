// @vitest-environment happy-dom
// 输入卡顿根治回归锁（2026-10-02）：draft 每击键重渲染 ChatTab 壳层，但消息列表
// 区域（ChatMessageRegion，React.memo）props 稳定时不得重渲染——修前每键全量
// 协调数百条消息组件（N 次渲染），修后 0 次。计数器锁死不回归。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AgentSummary, ChatMessage } from '../types';

// 渲染计数器：包裹真实 ChatMessageView，函数组件被 React 重调用即计数
let renderCount = 0;
vi.mock('./ChatMessageView', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./ChatMessageView')>();
  const Wrapped = (props: Parameters<typeof actual.default>[0]): ReturnType<typeof actual.default> => {
    renderCount += 1;
    return actual.default(props);
  };
  return { default: Wrapped };
});

const { useSessionMock } = vi.hoisted(() => ({ useSessionMock: vi.fn() }));
vi.mock('../hooks/useSession', () => ({
  useSession: (...args: unknown[]) => useSessionMock(...args),
}));

import ChatTab from './ChatTab';

const agent: AgentSummary = {
  driver: 'claude',
  id: 'perf-session',
  name: 'perf',
  cwd: '/repo',
  kind: 'interactive',
  rawState: 'running',
  group: 'other',
  detail: null,
  tokens: null,
  context_tokens: null,
  startedAt: null,
  sessionId: 'perf-session',
  instanceId: null,
};

function msg(i: number): ChatMessage {
  return {
    kind: 'assistant',
    rawType: null,
    text: `消息内容 ${i}——足够长的文本让协调有真实成本`,
    toolUseId: null,
    toolName: null,
    input: null,
    result: null,
    error: null,
    ts: null,
  };
}

function renderChat(): void {
  useSessionMock.mockReturnValue({
    messages: Array.from({ length: 50 }, (_, i) => msg(i)),
    isLoading: false,
    error: null,
    refetch: () => {},
    hasMore: false,
    noMore: false,
    loadingOlder: false,
    loadOlder: async () => false,
  });
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <ChatTab agent={agent} />
    </QueryClientProvider>,
  );
}

afterEach(cleanup);

describe('输入卡顿根治（2026-10-02）：击键不重渲染消息列表', () => {
  it('draft 击键前后 ChatMessageView 渲染计数不变（memo 区域生效）', async () => {
    renderChat();
    const textarea = (await screen.findByRole('textbox')) as HTMLTextAreaElement;
    await waitForStable();
    renderCount = 0;

    fireEvent.change(textarea, { target: { value: '你' } });
    fireEvent.change(textarea, { target: { value: '你好' } });
    fireEvent.change(textarea, { target: { value: '你好，世界' } });

    expect(renderCount).toBe(0);
  });
});

async function waitForStable(): Promise<void> {
  // 首帧渲染计数基线：等一拍让 query effect 落定
  await new Promise((r) => setTimeout(r, 50));
}
