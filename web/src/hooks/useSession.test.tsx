// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { useState, type ReactNode } from 'react';
import { useSession } from './useSession';
import type { ChatMessage } from '../types';
import type { ChatConfig, MessagePage } from '../api';

const messagesPageMock = vi.fn();
const subagentMessagesPageMock = vi.fn();
const acpMessagesMock = vi.fn();
// mock 返回类型对齐真实 ChatConfig：字段名漂移（r39 BLOCKER）会被 tsc 拦截
const chatConfigMock = vi.fn((): ChatConfig => ({ pageSize: 20, bufferMax: 100 }));
vi.mock('../api', () => ({
  api: {
    messagesPage: (...args: unknown[]) => messagesPageMock(...args),
    subagentMessagesPage: (...args: unknown[]) => subagentMessagesPageMock(...args),
    acpMessages: (...args: unknown[]) => acpMessagesMock(...args),
  },
  chatConfig: () => chatConfigMock(),
}));

const msg = (n: number): ChatMessage => ({
  kind: 'user',
  rawType: null,
  text: `m${n}`,
  toolUseId: null,
  toolName: null,
  input: null,
  result: null,
  error: null,
  ts: null,
});

const page = (from: number, to: number, hasMore: boolean): MessagePage => ({
  messages: Array.from({ length: to - from }, (_, i) => msg(from + i)),
  firstLine: from,
  hasMore,
});

const renderSession = (subagentId: string | null = null) =>
  renderHook(() => useSession('a1', 'sess-1', subagentId), {
    wrapper: ({ children }: { children: ReactNode }) => {
      // client 必须 render 间稳定——每次 new 会让 query observer 反复重建
      const [client] = useState(() => new QueryClient());
      return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
    },
  });

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('useSession pagination', () => {
  it('loads the tail page initially and prepends older pages via loadOlder', async () => {
    // B12/B13：首屏末页；点击翻页以末页 firstLine 为游标 prepend，全量拉完 noMore
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    messagesPageMock.mockResolvedValue(page(10, 20, true));
    const { result } = renderSession();
    expect(result.current.isLoading).toBe(true);
    await waitFor(() => expect(result.current.messages).toHaveLength(10));
    expect(result.current.messages[0].text).toBe('m10');
    expect(result.current.hasMore).toBe(true);
    expect(result.current.noMore).toBe(false);

    messagesPageMock.mockResolvedValue(page(0, 10, false));
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(result.current.messages).toHaveLength(20);
    expect(result.current.messages[0].text).toBe('m0');
    expect(messagesPageMock).toHaveBeenLastCalledWith('a1', 10);
    expect(result.current.hasMore).toBe(false);
    expect(result.current.noMore).toBe(true);
  });

  it('prunes the oldest pages when the buffer exceeds buffer_max without cursor regression', async () => {
    // B12：缓冲超上限淘汰最旧整页（缺口即被淘汰区间）；游标保留——再翻页不重复拉已淘汰区间
    chatConfigMock.mockResolvedValue({ pageSize: 5, bufferMax: 12 });
    messagesPageMock.mockResolvedValue(page(15, 20, true));
    const { result } = renderSession();
    await waitFor(() => expect(result.current.messages).toHaveLength(5));

    // 翻第 1 页：10 ≤ 12 → 全保留
    messagesPageMock.mockResolvedValue(page(10, 15, true));
    await act(async () => {
      await result.current.loadOlder();
    });
    await waitFor(() => expect(result.current.messages).toHaveLength(10));

    // 翻第 2 页：15 > 12 → 最旧页（m5-9）淘汰，剩 m10-14 + 末页
    messagesPageMock.mockResolvedValue(page(5, 10, true));
    await act(async () => {
      await result.current.loadOlder();
    });
    await waitFor(() =>
      expect(result.current.messages.map((m) => m.text)).toEqual(
        ['m10', 'm11', 'm12', 'm13', 'm14', 'm15', 'm16', 'm17', 'm18', 'm19'],
      ),
    );

    // 游标停在 5（不回退到已淘汰区边界）：再翻页拉 0-5，消息无重复
    messagesPageMock.mockResolvedValue(page(0, 5, false));
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(messagesPageMock).toHaveBeenLastCalledWith('a1', 5);
  });

  it('does not loop when the tail page alone exceeds buffer_max with empty older pages', async () => {
    // r1 B-1 回归：older.pages 空 + 末页超限 → 淘汰 effect 必须返回原引用，
    // 否则 setState 新对象 → effect 重跑 → Maximum update depth 白屏
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 10 });
    messagesPageMock.mockResolvedValue(page(0, 20, false)); // 末页 20 条 > buffer 10
    const { result } = renderSession();
    await waitFor(() => expect(result.current.messages).toHaveLength(20));
    // 稳定期：再排空一轮 effect 队列，无深度超限抛出即为过
    await act(async () => {});
    expect(result.current.messages).toHaveLength(20);
  });

  it('uses the subagent endpoint and resets older pages on view switch', async () => {
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    subagentMessagesPageMock.mockResolvedValue(page(0, 5, false));
    const { result, rerender } = renderHook(
      ({ sub }: { sub: string | null }) => useSession('a1', 'sess-1', sub),
      {
        initialProps: { sub: null as string | null },
        wrapper: ({ children }: { children: ReactNode }) => {
          const [client] = useState(() => new QueryClient());
          return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
        },
      },
    );
    rerender({ sub: 'sub-1' });
    await waitFor(() => expect(result.current.messages).toHaveLength(5));
    expect(subagentMessagesPageMock).toHaveBeenCalledWith('a1', 'sub-1');
  });
});

describe('useSession source isolation (ocr-review 高)', () => {
  it('routes acp sessions to the acp endpoint and never pages claude history', async () => {
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    acpMessagesMock.mockResolvedValue(page(0, 5, false));
    const { result } = renderHook(() => useSession('a1', 'sess-1', null, 'acp'), {
      wrapper: ({ children }: { children: ReactNode }) => {
        const [client] = useState(() => new QueryClient());
        return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
      },
    });
    await waitFor(() => expect(result.current.messages).toHaveLength(5));
    expect(acpMessagesMock).toHaveBeenCalledWith('a1');
    expect(messagesPageMock).not.toHaveBeenCalled();
    // ACP 无分页（内存回放模型）：loadOlder 短路，不落 claude 翻页端点
    let got = true;
    await act(async () => {
      got = await result.current.loadOlder();
    });
    expect(got).toBe(false);
    expect(messagesPageMock).not.toHaveBeenCalled();
  });

  it('keeps acp and claude caches separate for the same agent id', async () => {
    // queryKey 不含 source 时同 id 两驱动复用缓存：切到 acp 命中 claude 数据不拉端点
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    acpMessagesMock.mockResolvedValue(page(0, 5, false));
    messagesPageMock.mockResolvedValue(page(0, 7, false));
    // 测试内两个 hook 实例共享同一 client（缓存隔离的观测前提）
    const client = new QueryClient();
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    );
    const claudeView = renderHook(() => useSession('a1', 'sess-1', null, 'claude'), { wrapper });
    await waitFor(() => expect(claudeView.result.current.messages).toHaveLength(7));
    const acpView = renderHook(() => useSession('a1', 'sess-1', null, 'acp'), { wrapper });
    await waitFor(() => expect(acpView.result.current.messages).toHaveLength(5));
    expect(messagesPageMock).toHaveBeenCalledTimes(1);
    expect(acpMessagesMock).toHaveBeenCalledTimes(1);
  });
});
