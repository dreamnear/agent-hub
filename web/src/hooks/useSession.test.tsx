// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { useState, type ReactNode } from 'react';
import { useSession } from './useSession';
import type { ChatMessage, ChatMessageKind } from '../types';
import type { ChatConfig, MessagePage } from '../api';

const messagesPageMock = vi.fn();
const subagentMessagesPageMock = vi.fn();
const acpMessagesMock = vi.fn();
// mock 返回类型对齐真实 ChatConfig：字段名漂移（r39 BLOCKER）会被 tsc 拦截
const chatConfigMock = vi.fn((): ChatConfig => ({ pageSize: 20, bufferMax: 100 }));
vi.mock('../api', () => ({
  api: {
    baseAppUrl: '', // 镜像真实 makeApi('',null) 形状（wsEndpoint 依赖）
    messagesPage: (...args: unknown[]) => messagesPageMock(...args),
    subagentMessagesPage: (...args: unknown[]) => subagentMessagesPageMock(...args),
    acpMessages: (...args: unknown[]) => acpMessagesMock(...args),
  },
  chatConfig: () => chatConfigMock(),
  // wsEndpoint 真实实现镜像（本机/代理同源 + hub token；direct 协议升级 + per-instance token）
  wsEndpoint: (baseUrl: string, instanceId: string | null) => {
    const wsProto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    if (!baseUrl || baseUrl.startsWith('/')) {
      return {
        host: `${wsProto}//${location.host}${baseUrl}`,
        token: sessionStorage.getItem('hub_token'),
      };
    }
    const proto = baseUrl.startsWith('https:') ? 'wss:' : 'ws:';
    return {
      host: baseUrl.replace(/^https?:/, proto),
      token: instanceId == null ? null : sessionStorage.getItem(`hub_token_${instanceId}`),
    };
  },
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

// —— 可控 WebSocket 假体（WS 重连对账用例，agent-hub-queue-stale-banner）——
class WsFake {
  static instances: WsFake[] = [];
  url: string;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  close = vi.fn();
  send = vi.fn();
  constructor(url: string) {
    this.url = url;
    WsFake.instances.push(this);
  }
}

const mkText = (kind: ChatMessageKind, text: string): ChatMessage => ({
  kind,
  rawType: null,
  text,
  toolUseId: null,
  toolName: null,
  input: null,
  result: null,
  error: null,
  ts: null,
});

const textPage = (msgs: ChatMessage[]): MessagePage => ({
  messages: msgs,
  firstLine: 0,
  hasMore: false,
});

const flush = async (): Promise<void> => {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    // fake timers 下 react-query notifyManager 的调度也是 timer，推一拍防悬挂
    //（real timers 环境直接抛错，catch 掉即可）
    try {
      await vi.advanceTimersByTimeAsync(0);
    } catch {
      // not mocked
    }
  });
};

const pushLine = (ws: WsFake, kind: ChatMessageKind, text: string): void => {
  ws.onmessage?.({ data: JSON.stringify({ sessionId: 'sess-1', message: mkText(kind, text) }) });
};

beforeEach(() => {
  vi.stubGlobal('WebSocket', WsFake as unknown as typeof WebSocket);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  WsFake.instances = [];
  vi.unstubAllGlobals();
  vi.useRealTimers();
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
    expect(messagesPageMock).toHaveBeenLastCalledWith('a1', 10, 'claude');
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
    expect(messagesPageMock).toHaveBeenLastCalledWith('a1', 5, 'claude');
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
    expect(subagentMessagesPageMock).toHaveBeenCalledWith('a1', 'sub-1', undefined, 'claude');
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

// —— WS 重连对账（agent-hub-queue-stale-banner Q2）：盲续连永久丢失断连窗口内
// 落盘的行（含队列消息被 CLI 消费时的 user 行）→ 排队横幅对账失效滞留（生产
// frp 移动端实测）。重连成功拉末页权威快照，按 cache 尾条锚点增补缺失行。——
describe('useSession WS 重连对账（queue-stale-banner Q2）', () => {
  it('Q2 断连窗口补洞：重连成功拉末页快照，按尾条锚点增补缺失行', async () => {
    vi.useFakeTimers();
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    // 初始历史只有一行（断连前状态）
    messagesPageMock.mockResolvedValue(textPage([mkText('user', '第一条')]));
    const { result } = renderSession();
    // fake timers 下 waitFor 轮询冻结——用 flush 推进（microtask + notify 一拍）
    await flush();
    await flush();
    expect(result.current.messages).toHaveLength(1);
    const ws1 = WsFake.instances[0] as WsFake;
    ws1.onopen?.(); // 首连：不对账（历史 query 已拉）
    await flush();
    expect(messagesPageMock).toHaveBeenCalledTimes(1);
    // 断连 → 窗口内落盘两行（队列消费的 user 行 + 回复）→ 2s 盲重连
    ws1.onclose?.();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    const ws2 = WsFake.instances[1] as WsFake;
    expect(ws2).toBeTruthy();
    // 对账快照：权威末页含洞行
    messagesPageMock.mockResolvedValue(
      textPage([mkText('user', '第一条'), mkText('user', '排队消息'), mkText('assistant', '回复')]),
    );
    ws2.onopen?.();
    await flush();
    await flush();
    // 洞行补进消息流（横幅对账 effect 据此销账——ChatTab 层既有测试覆盖）
    const texts = result.current.messages.map((m) => `${m.kind}:${m.text ?? ''}`);
    expect(texts).toEqual(['user:第一条', 'user:排队消息', 'assistant:回复']);
  });

  it('首连不对账（历史 query 初始加载即权威，不重复拉取）', async () => {
    vi.useFakeTimers();
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    messagesPageMock.mockResolvedValue(textPage([]));
    renderSession();
    await flush();
    const ws1 = WsFake.instances[0] as WsFake;
    ws1.onopen?.();
    await flush();
    // 仅初始 query 一次；首连 onopen 不触发对账请求
    expect(messagesPageMock).toHaveBeenCalledTimes(1);
  });

  it('锚点找不到（cache 尾条未落盘）放弃对账——不覆盖不劣化', async () => {
    vi.useFakeTimers();
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    messagesPageMock.mockResolvedValue(textPage([mkText('user', '第一条')]));
    const { result } = renderSession();
    await flush();
    await flush();
    expect(result.current.messages).toHaveLength(1);
    const ws1 = WsFake.instances[0] as WsFake;
    ws1.onopen?.();
    await flush();
    ws1.onclose?.();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    const ws2 = WsFake.instances[1] as WsFake;
    // 重连后 tail 先推一行「落盘晚」的新行（cache 尾条=它）
    pushLine(ws2, 'assistant', '新输出');
    await flush();
    await flush();
    expect(result.current.messages).toHaveLength(2);
    // 对账快照不含「新输出」（落盘晚于快照）→ 锚点找不到 → 快照不应用
    messagesPageMock.mockResolvedValue(
      textPage([mkText('user', '第一条'), mkText('assistant', '快照里的旧行')]),
    );
    ws2.onopen?.();
    await flush();
    await flush();
    // tail 行保留；快照「旧行」未被错误合入（不覆盖不重复）
    expect(result.current.messages.map((m) => m.text)).toEqual(['第一条', '新输出']);
  });

  it('快照无新增（anchor 后无缺失）→ cache 原样（引用不变不触发重渲）', async () => {
    vi.useFakeTimers();
    chatConfigMock.mockResolvedValue({ pageSize: 20, bufferMax: 100 });
    messagesPageMock.mockResolvedValue(textPage([mkText('user', '第一条')]));
    const { result } = renderSession();
    await flush();
    await flush();
    expect(result.current.messages).toHaveLength(1);
    const ws1 = WsFake.instances[0] as WsFake;
    ws1.onopen?.();
    await flush();
    ws1.onclose?.();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    const ws2 = WsFake.instances[1] as WsFake;
    messagesPageMock.mockResolvedValue(textPage([mkText('user', '第一条')]));
    ws2.onopen?.();
    await flush();
    await flush();
    // 内容原样（快照与 cache 相同 → missing 为空 → 不写 cache）
    expect(result.current.messages).toHaveLength(1);
  });
});
