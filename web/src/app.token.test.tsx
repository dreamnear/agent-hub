// @vitest-environment happy-dom
// token 持久化回归（2026-09-23）：刷新/深链后首查必须带上回填的 token。
// 根因：react-query 首查 effect 先于 App 挂载 effect 里的 captureUrlToken 回填执行，
// 首个请求无 Authorization → 401 → clearStoredToken 误清持久层 → 每次刷新重输
// （r66/r67 遗留观察项「TokenGate reload 后重提 token」确认为真问题）。
// 修复：回填上移 api.ts 模块加载期；测试用 resetModules+动态 import 模拟整页加载。
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ComponentType } from 'react';
import { cleanup, render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// 记录每个 fetch 的 authorization 头；可按需换 401 桩
const requests: { url: string; auth: string | null }[] = [];
const fetch200 = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
  requests.push({ url: String(input), auth: new Headers(init?.headers).get('authorization') });
  return Promise.resolve(
    new Response('[]', { status: 200, headers: { 'Content-Type': 'application/json' } }),
  );
});
vi.stubGlobal('fetch', fetch200);

/// 模拟整页加载：重置模块图后重新 import App（连带 api.ts 模块级回填重新执行）
const loadApp = async (): Promise<ComponentType> => {
  vi.resetModules();
  const mod = await import('./App');
  return mod.default;
};

const renderApp = async (App: ComponentType): Promise<void> => {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <App />
    </QueryClientProvider>,
  );
};

afterEach(() => {
  cleanup();
  sessionStorage.clear();
  localStorage.clear();
  requests.length = 0;
  history.replaceState(null, '', location.pathname);
});

describe('token 持久化（reload/深链免输）', () => {
  it('持久层有 token：首个 API 请求即携带 Authorization（先回填后请求）', async () => {
    localStorage.setItem('agent_hub_token', 'saved-tok');
    renderApp(await loadApp());
    await waitFor(() => expect(requests.length).toBeGreaterThan(0));
    expect(requests[0].auth).toBe('Bearer saved-tok');
  });

  it('URL ?token= 深链：首个 API 请求即携带该 token 且 URL 已清理', async () => {
    history.replaceState(null, '', '/?token=url-tok');
    renderApp(await loadApp());
    await waitFor(() => expect(requests.length).toBeGreaterThan(0));
    expect(requests[0].auth).toBe('Bearer url-tok');
    expect(location.search).toBe('');
    expect(localStorage.getItem('agent_hub_token')).toBe('url-tok');
  });

  it('持久 token 失效：401 后清空两层并回到输入界面（不死循环）', async () => {
    localStorage.setItem('agent_hub_token', 'stale-tok');
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(new Response('unauthorized', { status: 401 }))),
    );
    renderApp(await loadApp());
    // 门禁弹出（role=dialog aria-label=Token gate）
    await waitFor(() =>
      expect(document.querySelector('[aria-label="Token gate"]')).not.toBeNull(),
    );
    // 两层存储都被清退：持久层无残留（重输一次后恢复，不自动重进）
    expect(localStorage.getItem('agent_hub_token')).toBeNull();
    expect(sessionStorage.getItem('hub_token')).toBeNull();
  });
});
