// @vitest-environment happy-dom
// authFailed 自愈探测（凭据失效恢复修复）：查询层对 AuthError 不重试，查询停摆后
// 「任何一次成功请求即解除标记」永远等不到 → 标记挂死。本文件锁定探测循环的行为：
// 标记态每 AUTH_PROBE_MS 直连轻探一次，成功 → 解除标记 + invalidate 恢复轮询；
// 401 → 保持标记继续探；正常实例零请求。
import { act, cleanup, renderHook } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement, type ReactNode } from 'react';
import { AUTH_PROBE_MS, useAuthFailedProbe, type InstanceCtx } from './useAgents';
import { authFailedSnapshot, makeApi, storeToken } from '../api';

const fetchMock = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) =>
  Promise.resolve(new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } })),
);
vi.stubGlobal('fetch', fetchMock);

const ctx = (id: string, baseUrl: string): InstanceCtx => ({
  id,
  name: id,
  mode: 'ssh-tunnel',
  baseUrl,
  config: null,
});

const ok200 = (): void => {
  fetchMock.mockImplementation(() =>
    Promise.resolve(new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } })),
  );
};

/** 经 makeApi 真实 401 路径打标（探测定时器只认这个标记源） */
async function mark401(id: string, baseUrl: string): Promise<void> {
  fetchMock.mockImplementation(() =>
    Promise.resolve(new Response('unauthorized', { status: 401 })),
  );
  storeToken(`${id}-tok`, id);
  await expect(makeApi(baseUrl, id).listAgents(false)).rejects.toThrow();
  expect(authFailedSnapshot().has(id)).toBe(true);
  fetchMock.mockClear(); // 打标请求不进后续探针计数
}

/** 挂上探测 hook（真实 api 模块；hook 自订阅标记集合），返回 invalidateQueries spy */
function renderProbe(instances: InstanceCtx[]) {
  const client = new QueryClient();
  const invalidate = vi.spyOn(client, 'invalidateQueries');
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  renderHook(() => useAuthFailedProbe(instances), { wrapper });
  return { invalidate };
}

describe('useAuthFailedProbe（标记态低频自愈）', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockClear();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    sessionStorage.clear();
  });

  it('标记态下 30s 一探；探测成功 → 标记清除 + invalidate 恢复轮询', async () => {
    await mark401('h253', '/api/instances/h253/p');
    const { invalidate } = renderProbe([ctx('h253', '/api/instances/h253/p')]);

    ok200();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(AUTH_PROBE_MS - 1);
    });
    expect(fetchMock).not.toHaveBeenCalled(); // 未到期不发探
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/instances/h253/p/api/agents');
    // act 内微任务已排空：标记与 invalidate 均已落定，直接断言
    expect(authFailedSnapshot().has('h253')).toBe(false);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['agents', 'h253'] });
  });

  it('探测 401 → 标记保持，下个周期继续探', async () => {
    await mark401('h254', '/api/instances/h254/p');
    const { invalidate } = renderProbe([ctx('h254', '/api/instances/h254/p')]);

    // 探测仍 401（token 没修好）
    await act(async () => {
      await vi.advanceTimersByTimeAsync(AUTH_PROBE_MS);
    });
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(authFailedSnapshot().has('h254')).toBe(true);
    expect(invalidate).not.toHaveBeenCalled();

    // 第二周期仍在探
    await act(async () => {
      await vi.advanceTimersByTimeAsync(AUTH_PROBE_MS);
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(authFailedSnapshot().has('h254')).toBe(true);
  });

  it('正常实例零额外请求：集合为空不排任何定时器', async () => {
    renderProbe([ctx('h255', '/api/instances/h255/p')]);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(AUTH_PROBE_MS * 3);
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('标记清除后停探（探测成功解除 → effect 清理不再排探）', async () => {
    await mark401('h256', '/api/instances/h256/p');
    renderProbe([ctx('h256', '/api/instances/h256/p')]);
    ok200();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(AUTH_PROBE_MS);
    });
    expect(authFailedSnapshot().has('h256')).toBe(false);
    const calls = fetchMock.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(AUTH_PROBE_MS * 2);
    });
    expect(fetchMock.mock.calls.length).toBe(calls); // 解除后不再探
  });
});
