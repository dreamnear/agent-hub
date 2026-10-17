// @vitest-environment happy-dom
// 健康状态机 hook 接线（agent-hub-instance-health）：清单失败计数、被动/主动救活、
// WS 断开单独至多 degraded——在真实 useAgents 数据流（react-query 重试/重探节奏）
// 里验证。api 层存活发射点由 api.instanceHealth.test.ts 锁定；呈现分支由
// AgentList.test.tsx 锁定。纯转移矩阵见 lib/instanceHealth.test.ts。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { useAgents } from './useAgents';
import { instanceHealth } from '../lib/instanceHealth';
import type { InstanceConfig } from '../types';

/** per-instance 清单行为：本机恒通；远程按脚本走（可随时切换） */
let remoteFail = true;
const listAgentsMock = vi.fn();
const defaultList = (instanceId: string | null): Promise<never[] | []> => {
  if (instanceId == null) return Promise.resolve([]);
  return remoteFail ? Promise.reject(new Error('list down')) : Promise.resolve([]);
};

// 转发式 mock：args 原样记录供测试按位取；实现（返回 closer）在 beforeEach 注入，
// 避免实现形参未用触发 no-unused-vars
const openEventStreamMock = vi.fn<(...args: unknown[]) => (() => void)>();

// useSyncExternalStore 快照契约：getSnapshot 必须返回稳定引用（新引用会触发
// React 无限重渲）。镜像真 api.ts 的模块级缓存集合形态。
const EMPTY_AUTH_FAILED = new Set<string>();

vi.mock('../api', () => ({
  api: {
    tunnelStart: () => Promise.resolve({ localPort: 58111 }),
    tunnelStatus: () => Promise.resolve({ running: true, localPort: null, state: 'up', retries: 0 }),
    projectTree: () => Promise.resolve([]),
  },
  openEventStream: (...args: unknown[]) => openEventStreamMock(...(args as Parameters<typeof openEventStreamMock>)),
  makeApi: (_baseUrl: string, instanceId: string | null) => ({
    listAgents: () => listAgentsMock(instanceId),
  }),
  AuthError: class AuthError extends Error {},
  probeInstanceAuth: () => Promise.resolve(false),
  subscribeAuthFailed: () => () => {},
  authFailedSnapshot: () => EMPTY_AUTH_FAILED,
  REMOTE_TIMEOUT_MS: 10_000,
}));

vi.mock('../instances', () => ({
  fetchInstances: (): Promise<InstanceConfig[]> =>
    Promise.resolve([
      {
        id: 'instR',
        name: '远程机',
        mode: 'direct',
        url: 'https://r.example.com',
        token: null,
        ssh: null,
        remotePort: 0,
        localPort: null,
        nodeMeta: null,
      },
    ]),
  instanceBaseUrl: (inst: InstanceConfig) => inst.url ?? `/api/instances/${encodeURIComponent(inst.id)}/p`,
}));

function renderAgents() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return renderHook(() => useAgents(), { wrapper });
}

beforeEach(() => {
  vi.useFakeTimers();
  instanceHealth.__reset();
  remoteFail = true;
  // mockReset：上例的 mockImplementation 不得泄入下例
  listAgentsMock.mockReset();
  listAgentsMock.mockImplementation(defaultList);
  openEventStreamMock.mockReset();
  openEventStreamMock.mockImplementation(() => () => {});
});
afterEach(() => {
  cleanup();
  instanceHealth.__reset();
  vi.useRealTimers();
});

/** fake timers 下的条件等待：推进假时钟 + 排空微任务（testing-library 的 waitFor
 * 内部定时器被假时钟冻住会挂死，authProbe 测试同款规避——全部显式推进） */
async function until(cond: () => boolean): Promise<void> {
  for (let i = 0; i < 100 && !cond(); i++) {
    await act(() => vi.advanceTimersByTimeAsync(50));
  }
  expect(cond()).toBe(true);
}

/** 本机 + instR 两条查询都就位（instances 清单已并、首次拉取已发） */
async function waitForBothInstances(): Promise<void> {
  await until(() => listAgentsMock.mock.calls.some((c) => c[0] === 'instR'));
}

describe('useAgents 健康状态机接线', () => {
  it('瞬态单次失败 → degraded；下一次成功即回 online、计数清零', async () => {
    let remoteCalls = 0;
    listAgentsMock.mockImplementation((instanceId: string | null) => {
      if (instanceId == null) return Promise.resolve([]);
      remoteCalls += 1;
      return remoteCalls === 1 ? Promise.reject(new Error('hiccup')) : Promise.resolve([]);
    });
    const { result } = renderAgents();
    await waitForBothInstances();

    // 首败落地：degraded（chip 档），本机不受影响（until 在 degraded 一出现即返回，
    // 不会推进过 2s 的重试点）。快照语义：无迁移的实例缺席 = online。
    await until(() => instanceHealth.snapshot().instR === 'degraded');
    expect(instanceHealth.snapshot()[''] ?? 'online').toBe('online');
    expect(result.current.health['instR']).toBe('degraded');

    // 重试成功（retryDelay 2s）：救活 + 计数清零
    await act(() => vi.advanceTimersByTimeAsync(2_500));
    expect(instanceHealth.snapshot().instR).toBe('online');
    expect(result.current.health['instR']).toBe('online');
  });

  it('持续失败（拦截 ≥20s、≥3 次）→ offline；本机全程 online', async () => {
    const { result } = renderAgents();
    await waitForBothInstances();
    await until(() => instanceHealth.snapshot().instR === 'degraded');

    // 5s 错误重探持续推进：20s 跨度到点后翻 offline（事件驱动 + 到点重推双保险）
    await act(() => vi.advanceTimersByTimeAsync(30_000));
    expect(instanceHealth.snapshot().instR).toBe('offline');
    expect(result.current.health['instR']).toBe('offline');
    expect(instanceHealth.snapshot()[''] ?? 'online').toBe('online');
    expect(result.current.health['']).toBe('online');
  });

  it('WS 断开单独发生 → 呈现 degraded，状态机不记 offline（事件延迟 ≠ 实例不可达）', async () => {
    remoteFail = false;
    const { result } = renderAgents();
    await waitForBothInstances();
    // 全程成功无迁移：hook 合成态即 online（快照缺席 = online 语义）
    await until(() => result.current.health['instR'] === 'online');

    // 捕获 instR 的 WS 连通性回调并模拟断开
    const call = openEventStreamMock.mock.calls.find((c) => c[1] === 'instR') as unknown as [
      () => void,
      string | null,
      string,
      (up: boolean) => void,
    ];
    expect(call).toBeTruthy();
    act(() => call[3](false));
    expect(instanceHealth.snapshot().instR ?? 'online').toBe('online'); // 状态机没记失败（无迁移缺席）
    expect(result.current.health['instR']).toBe('degraded'); // 呈现层降级
    act(() => call[3](true));
    expect(result.current.health['instR']).toBe('online');
  });

  it('被动救活（用户核心要求）：offline 下任一存活信号立即回 online、置灰解除', async () => {
    const { result } = renderAgents();
    await waitForBothInstances();
    await act(() => vi.advanceTimersByTimeAsync(30_000));
    expect(instanceHealth.snapshot().instR).toBe('offline');
    expect(result.current.health['instR']).toBe('offline');

    // 模拟 api 层存活发射（实例域任一 API 成功/events 帧——发射点已在
    // api.instanceHealth.test.ts 用真 api 模块锁定）
    act(() => instanceHealth.onAlive('instR'));
    expect(instanceHealth.snapshot().instR).toBe('online');
    expect(result.current.health['instR']).toBe('online');
  });
});
