// @vitest-environment happy-dom
// H253 请求风暴：清单每次重拉都是新数组引用，若 WS effect 跟着引用拆建，
// 新 WS 的 onopen 又各自 invalidate agents → 半通慢隧道上滚成排队风暴。
// 本文件锁定「实例集没变（签名不变）就不拆建 WS」，与退避/冷却的纯函数用例
// （useAgents.test.ts）分开是因为它要 vi.mock 模块，模块 mock 是文件级的。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { useRemoteInstances } from './useAgents';
import type { InstanceConfig } from '../types';

const openEventStreamMock = vi.fn((..._args: unknown[]) => () => {});
const fetchInstancesMock = vi.fn((): Promise<InstanceConfig[]> => Promise.resolve([]));
const tunnelStartMock = vi.fn((_id: string) => Promise.resolve({ localPort: 58111 }));

vi.mock('../api', () => ({
  api: {
    tunnelStart: (id: string) => tunnelStartMock(id),
    tunnelStatus: () => Promise.resolve({ running: true, localPort: null, state: 'up', retries: 0 }),
  },
  openEventStream: (...args: unknown[]) => openEventStreamMock(...args),
  makeApi: () => ({}),
  AuthError: class AuthError extends Error {},
  subscribeAuthFailed: () => () => {},
  authFailedSnapshot: () => new Set<string>(),
}));

vi.mock('../instances', () => ({
  fetchInstances: () => fetchInstancesMock(),
  // 镜像真实实现（ssh-tunnel → 同源代理前缀；direct → 配置 url）
  instanceBaseUrl: (inst: InstanceConfig) =>
    inst.mode === 'direct' ? inst.url : `/api/instances/${encodeURIComponent(inst.id)}/p`,
}));

function remote(id: string): InstanceConfig {
  return {
    id,
    name: id,
    mode: 'ssh-tunnel',
    url: null,
    token: null,
    ssh: null,
    remotePort: 22,
    localPort: 58111,
    nodeMeta: null,
  };
}

function renderInstances() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return { client, ...renderHook(() => useRemoteInstances(), { wrapper }) };
}

describe('useRemoteInstances 的 WS 订阅生命周期（H253）', () => {
  beforeEach(() => {
    openEventStreamMock.mockClear();
    tunnelStartMock.mockClear();
    fetchInstancesMock.mockReset();
  });
  afterEach(cleanup);

  it('清单内容变但端点没变（localPort 翻转）不拆建 WS', async () => {
    fetchInstancesMock.mockResolvedValue([remote('h253')]);
    const { client, result } = renderInstances();
    // 本机 + h253 各一条；h253 的 ensure 幂等 start 会 invalidate(['instances'])
    // → 清单重拉，这正是旧实现每轮全量拆建 WS 的触发点
    await waitFor(() => expect(result.current.instances).toHaveLength(2));
    await waitFor(() => expect(fetchInstancesMock.mock.calls.length).toBeGreaterThan(1));
    const calls = openEventStreamMock.mock.calls.length;
    // 首帧清单还空（1 条本机）→ 数据到齐后 +2，共 3；此后只增不减不再动
    expect(calls).toBe(3);

    // 隧道重分配端口：清单字段变了 → 新数组引用，但 WS 端点（id/baseUrl）没变
    for (let i = 0; i < 3; i++) {
      fetchInstancesMock.mockResolvedValue([{ ...remote('h253'), localPort: 58200 + i }]);
      await act(async () => {
        await client.invalidateQueries({ queryKey: ['instances'] });
      });
    }
    await waitFor(() => expect(result.current.instances[1]?.config?.localPort).toBe(58202));
    expect(fetchInstancesMock.mock.calls.length).toBeGreaterThan(4);
    expect(openEventStreamMock.mock.calls.length).toBe(calls);
  });

  it('实例集真的变了才拆建 WS（没有把重建整个关掉）', async () => {
    fetchInstancesMock.mockResolvedValue([remote('h253')]);
    const { client, result } = renderInstances();
    await waitFor(() => expect(result.current.instances).toHaveLength(2));
    const calls = openEventStreamMock.mock.calls.length;

    fetchInstancesMock.mockResolvedValue([remote('h253'), remote('h254')]);
    await act(async () => {
      await client.invalidateQueries({ queryKey: ['instances'] });
    });
    await waitFor(() => expect(result.current.instances).toHaveLength(3));
    expect(openEventStreamMock.mock.calls.length).toBe(calls + 3); // 全量拆建 3 条
  });
});
