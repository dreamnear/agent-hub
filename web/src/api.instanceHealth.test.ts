// @vitest-environment happy-dom
// 存活信号发射（agent-hub-instance-health）：实例域任何一次 API 成功（GET 2xx /
// POST 204 / events WS 真实帧）都须把该实例健康机拉回 online。api 层只发「存活」，
// 清单失败计数在 useAgents queryFn（见 useAgents.health.test.tsx）——本文件用真
// api 模块锁发射点，hook 消费侧另测。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { makeApi, openEventStream } from './api';
import { instanceHealth } from './lib/instanceHealth';

const fetchMock = vi.fn((): Promise<Response> =>
  Promise.resolve(new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } })),
);
vi.stubGlobal('fetch', fetchMock);

// 记录最近一条 WebSocket 实例（events 帧触发用；直接 this 入列不做局部别名）
const wsInstances: { onmessage: (() => void) | null }[] = [];
vi.stubGlobal(
  'WebSocket',
  class {
    onclose: (() => void) | null = null;
    onmessage: (() => void) | null = null;
    onerror: (() => void) | null = null;
    close = (): void => {
      this.onclose?.();
    };
    constructor() {
      wsInstances.push(this);
    }
  },
);

beforeEach(() => {
  instanceHealth.__reset();
  // mockReset：上例的 mockImplementation（500 等）不得泄入下例
  fetchMock.mockReset();
  fetchMock.mockImplementation(() =>
    Promise.resolve(new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } })),
  );
});
afterEach(() => {
  instanceHealth.__reset();
  sessionStorage.clear();
  localStorage.clear();
});

const degraded = (id: string): void => {
  instanceHealth.onListFail(id, Date.now());
  expect(instanceHealth.snapshot()[id]).toBe('degraded'); // 前置成立
};

describe('API 成功 → 存活信号（实例拉回 online）', () => {
  it('GET 列表 200：degraded → online', async () => {
    degraded('i1');
    await makeApi('https://hub.example.com', 'i1').listAgents(false);
    expect(instanceHealth.snapshot().i1).toBe('online');
  });

  it('POST 发送 204（用户核心场景：offline 实例发消息即救活）：degraded → online', async () => {
    degraded('i1');
    fetchMock.mockImplementation(() => Promise.resolve(new Response(null, { status: 204 })));
    await makeApi('https://hub.example.com', 'i1').sendMessage('a1', 'hello');
    expect(instanceHealth.snapshot().i1).toBe('online');
  });

  it('失败请求不发存活信号（degraded 保持）', async () => {
    degraded('i1');
    fetchMock.mockImplementation(() => Promise.resolve(new Response('boom', { status: 500 })));
    await makeApi('https://hub.example.com', 'i1').listAgents(false).catch(() => {});
    expect(instanceHealth.snapshot().i1).toBe('degraded');
  });

  it('实例隔离：i1 的成功不救活 i2', async () => {
    degraded('i1');
    degraded('i2');
    await makeApi('https://hub.example.com', 'i1').listAgents(false);
    expect(instanceHealth.snapshot().i1).toBe('online');
    expect(instanceHealth.snapshot().i2).toBe('degraded');
  });
});

describe('events WS 真实帧 → 存活信号', () => {
  it('onmessage 帧：degraded → online（帧是实例事件通道存活的证据）', () => {
    degraded('i2');
    const unsub = openEventStream(() => {}, 'i2', 'https://hub.example.com');
    const ws = wsInstances[wsInstances.length - 1];
    expect(ws).toBeTruthy();
    ws?.onmessage?.();
    expect(instanceHealth.snapshot().i2).toBe('online');
    unsub();
  });
});
