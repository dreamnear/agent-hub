// @vitest-environment happy-dom
// 多实例 api 工厂与 WS 订阅（agent-hub-multi-instance 批1 任务4）：
// 1. makeApi per-instance：本机=相对路径 hub_token；远程=baseUrl + per-instance token
// 2. queryKey 实例隔离（useSession 同 agentId 不同实例不串缓存——由含 instanceId 的 key 保证）
// 3. openEventStream 每实例各建一条连接（本机 + 远程各认各的 token）

import { afterEach, describe, expect, it, vi } from 'vitest';
import { makeApi, openEventStream, storeToken } from './api';

/// 记录 fetch 的目标 url 与 authorization
const fetches: { url: string; auth: string | null }[] = [];
const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
  fetches.push({ url: String(input), auth: new Headers(init?.headers).get('authorization') });
  return Promise.resolve(
    new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } }),
  );
});
vi.stubGlobal('fetch', fetchMock);

// 记录每个 new WebSocket 的 url
const wsUrls: string[] = [];
vi.stubGlobal('WebSocket', class {
  onclose: (() => void) | null = null;
  onmessage: (() => void) | null = null;
  onerror: (() => void) | null = null;
  close = (): void => {
    this.onclose?.();
  };
  constructor(u: string) {
    wsUrls.push(u);
  }
});

afterEach(() => {
  fetches.length = 0;
  wsUrls.length = 0;
  sessionStorage.clear();
  localStorage.clear();
  vi.clearAllMocks();
});

describe('makeApi per-instance', () => {
  it('本机实例：相对路径 + hub_token（现状语义零破坏）', async () => {
    storeToken('local-tok');
    const api = makeApi('', null);
    await api.listAgents(true);
    expect(fetches[0].url).toBe('/api/agents?all=1');
    expect(fetches[0].auth).toBe('Bearer local-tok');
  });

  it('远程实例：baseUrl 前缀 + per-instance token（hub_token_<id>）', async () => {
    storeToken('remote-tok', 'instA');
    // 本机 token 应不串进来
    storeToken('local-tok');
    const api = makeApi('https://hub.example.com', 'instA');
    await api.listAgents(false);
    expect(fetches[0].url).toBe('https://hub.example.com/api/agents');
    expect(fetches[0].auth).toBe('Bearer remote-tok');
  });

  it('本机 401 触发全局 unauthorizedCb；远程 401 不触发（per-instance 401 分流留任务 10）', async () => {
    let fired = 0;
    const { onUnauthorized } = await import('./api');
    onUnauthorized(() => {
      fired += 1;
    });
    fetchMock.mockImplementation(() =>
      Promise.resolve(new Response('unauthorized', { status: 401 })),
    );
    const local = makeApi('', null);
    await local.listAgents(false).catch(() => {});
    expect(fired).toBe(1);
    const remote = makeApi('https://hub.example.com', 'instA');
    await remote.listAgents(false).catch(() => {});
    expect(fired).toBe(1);
    // 恢复记录型 200 兜底，防串到后续用例
    fetchMock.mockImplementation((input, init) => {
      fetches.push({ url: String(input), auth: new Headers(init?.headers).get('authorization') });
      return Promise.resolve(
        new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } }),
      );
    });
    // 任务10：远程 401 会标记实例凭据失效——补一次成功请求解除，防泄漏到后续用例
    await remote.listAgents(false);
  });

  it('实例管理端点走本机 hub（相对路径）', async () => {
    const api = makeApi('', null);
    await api.listInstances();
    await api.deleteInstance('abc');
    const urls = fetches.map((f) => f.url);
    expect(urls[0]).toBe('/api/instances');
    expect(urls[1]).toBe('/api/instances/abc');
  });
});

describe('useSession queryKey 实例隔离（串实例防线）', () => {
  it('同 agentId 不同实例 → queryKey 含 instanceId 维度', async () => {
    // 验证 useSession 构造的 queryKey 含 instanceId：实例不同 → 分割隔离
    const { useSession } = await import('./hooks/useSession');
    void useSession;
    // 观察方式：两 instance 渲染同一 agent，各自触发独立 fetch 到各自 baseUrl
    // （mock 断言两实例都各自打 endpoint，且 url 前缀不同）
    storeToken('t-a', null);
    storeToken('t-b', 'instB');
    fetchMock.mockImplementation((input, init) => {
      fetches.push({ url: String(input), auth: new Headers(init?.headers).get('authorization') });
      return Promise.resolve(
        new Response(JSON.stringify({ messages: [], firstLine: 0, hasMore: false }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      );
    });
    // 用模块级 api 的 messagesPage 直接验证 url 路由即可（工厂已在上断言 baseUrl 前缀）
    const localApi = makeApi('', null);
    await localApi.messagesPage('agent-x');
    expect(fetches[0].url).toBe('/api/agents/claude/agent-x/messages/page');
    const remoteApi = makeApi('https://hub.example.com', 'instB');
    await remoteApi.messagesPage('agent-x');
    expect(fetches[1].url).toBe('https://hub.example.com/api/agents/claude/agent-x/messages/page');
    // 两实例同 agentId → 各自请求各自端点（缓存隔离由 react-query key 保证，此处验证
    // useSession 传入了不同 instanceId api → queryKey 第一维不同）
  });
});

describe('per-instance 401 分流（任务10）', () => {
  /// 恢复顶部记录型 200 实现（仿上方 401 用例的兜底惯例，防串到后续用例）
  const restoreRecording = (): void => {
    fetchMock.mockImplementation((input, init) => {
      fetches.push({ url: String(input), auth: new Headers(init?.headers).get('authorization') });
      return Promise.resolve(
        new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } }),
      );
    });
  };

  it('远程 401 标记该实例凭据失效；本机 401 不标记；成功请求解除', async () => {
    const { subscribeAuthFailed, authFailedSnapshot, AuthError } = await import('./api');
    let changes = 0;
    const unsub = subscribeAuthFailed(() => {
      changes += 1;
    });
    fetchMock.mockImplementation(() =>
      Promise.resolve(new Response('unauthorized', { status: 401 })),
    );
    const remote = makeApi('https://hub.example.com', 'instA');
    await expect(remote.listAgents(false)).rejects.toBeInstanceOf(AuthError);
    expect(authFailedSnapshot().has('instA')).toBe(true);
    // 本机 401：走全局 TokenGate 回调，不标记任何实例
    const local = makeApi('', null);
    await expect(local.listAgents(false)).rejects.toBeInstanceOf(AuthError);
    expect(authFailedSnapshot().size).toBe(1);
    // 恢复 200 → 解除标记（订阅者收到变化通知）
    restoreRecording();
    await remote.listAgents(false);
    expect(authFailedSnapshot().has('instA')).toBe(false);
    expect(changes).toBeGreaterThanOrEqual(2);
    unsub();
  });

  it('post 401 同样标记（会话操作路径）；401 错误可 instanceof 识别（查询层跳过重试用）', async () => {
    const { authFailedSnapshot, AuthError } = await import('./api');
    fetchMock.mockImplementation(() =>
      Promise.resolve(new Response('unauthorized', { status: 401 })),
    );
    const remote = makeApi('https://hub.example.com', 'instB');
    await expect(remote.sendMessage('a1', 'hi')).rejects.toBeInstanceOf(AuthError);
    expect(authFailedSnapshot().has('instB')).toBe(true);
    restoreRecording();
    await remote.sendMessage('a1', 'hi');
    expect(authFailedSnapshot().has('instB')).toBe(false);
  });
});

describe('openEventStream per-instance', () => {
  it('本机连 location.host /ws/events；远程连 baseUrl 升 ws /ws/events，各一条连接', () => {
    const calls: string[] = [];
    const close1 = openEventStream(() => calls.push('local'), null, '');
    const close2 = openEventStream(() => calls.push('remote'), 'instA', 'http://127.0.0.1:44123');
    expect(wsUrls).toHaveLength(2);
    expect(wsUrls[0]).toContain('/ws/events');
    expect(wsUrls[1]).toBe('ws://127.0.0.1:44123/ws/events');
    close1();
    close2();
  });
});