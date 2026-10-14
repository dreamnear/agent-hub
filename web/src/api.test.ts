// @vitest-environment happy-dom
// makeApi 每实例通道隔离（消息/发送链路补口）单测：
// 同实例 GET 串行（在途合一个）/ 不同实例互不阻塞 / 远端 10s deadline 降级 /
// 本机路径零变化（不排队不超时）/ 远端写请求不排队但挂超时。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { makeApi, REMOTE_TIMEOUT_MS } from './api';

interface HangCall {
  url: string;
  method: string;
  signal: AbortSignal | null;
  settle: (r: Response) => void;
  abort: () => void;
}

/** 可逐调用放行的 fetch 桩：每次调用记录 url/method/signal 并挂起；
 * signal abort 时以 AbortError 拒绝（模拟真 fetch 对 signal 的响应）。 */
function hangFetch(): { mock: ReturnType<typeof vi.fn>; calls: HangCall[] } {
  const calls: HangCall[] = [];
  const mock = vi.fn((url: string, init?: RequestInit) => {
    const call: HangCall = {
      url: String(url),
      method: init?.method ?? 'GET',
      signal: init?.signal ?? null,
      settle: () => {},
      abort: () => {},
    };
    const p = new Promise<Response>((resolve, reject) => {
      call.settle = resolve;
      call.abort = () => reject(new DOMException('The operation was aborted.', 'AbortError'));
    });
    call.signal?.addEventListener('abort', () => call.abort());
    calls.push(call);
    return p;
  });
  return { mock, calls };
}

const okJson = (body: unknown): Response =>
  new Response(JSON.stringify(body), { status: 200 });
const ok204 = (): Response => new Response(null, { status: 204 });

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('makeApi 每实例通道隔离（消息/发送链路）', () => {
  it('同实例并发多个 GET 只发一个在途，前序落定后按序放行', async () => {
    const { mock, calls } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const remote = makeApi('https://r.example', 'inst1');
    void remote.sessionActive('a');
    void remote.agentTasks('a');
    void remote.subagents('a');
    await Promise.resolve();
    // 在途合一个：另外两个排队等待
    expect(mock).toHaveBeenCalledTimes(1);
    expect(calls[0].method).toBe('GET');
    calls[0].settle(okJson(true));
    await vi.waitFor(() => expect(mock).toHaveBeenCalledTimes(2));
    calls[1].settle(okJson(null));
    await vi.waitFor(() => expect(mock).toHaveBeenCalledTimes(3));
    calls[2].settle(okJson([]));
  });

  it('不同实例互不阻塞（各通道独立在途）', async () => {
    const { mock, calls } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const a = makeApi('https://a.example', 'instA');
    const b = makeApi('https://b.example', 'instB');
    void a.sessionActive('x');
    void b.sessionActive('x');
    await Promise.resolve();
    expect(mock).toHaveBeenCalledTimes(2);
    expect(calls[0].url).toContain('https://a.example');
    expect(calls[1].url).toContain('https://b.example');
    calls.forEach((c) => c.settle(okJson(true)));
  });

  it('远端读超时以 AbortError 拒绝（10s 口径，与 agents 层降级一致）', async () => {
    vi.useFakeTimers();
    const { mock } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const remote = makeApi('https://r.example', 'inst1');
    const pending = remote.sessionActive('a');
    const guard = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    await vi.advanceTimersByTimeAsync(REMOTE_TIMEOUT_MS);
    await guard;
  });

  it('本机请求无超时：过超时点仍保持 pending（路径零变化）', async () => {
    vi.useFakeTimers();
    const { mock } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const local = makeApi('', null);
    let settled = false;
    void local.sessionActive('a').catch(() => {
      settled = true;
    });
    expect(mock).toHaveBeenCalledTimes(1); // 本机直达不排队
    await vi.advanceTimersByTimeAsync(REMOTE_TIMEOUT_MS + 1_000);
    expect(settled).toBe(false); // 无 deadline，挂起不被打断
  });

  it('本机并发 GET 全部直达（零变化：不排队）', async () => {
    const { mock, calls } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const local = makeApi('', null);
    void local.sessionActive('a');
    void local.agentTasks('a');
    void local.subagents('a');
    await Promise.resolve();
    expect(mock).toHaveBeenCalledTimes(3);
    calls.forEach((c) => c.settle(okJson(true)));
  });

  it('驱动路由参数化（dsh rm 400 修复）：agent 级端点按 driver 段路由', async () => {
    const { mock, calls } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const local = makeApi('', null);
    const ps = [
      local.removeAgent('session-x', 'dsh'),
      local.stopAgent('a', 'dsh'),
      local.respawnAgent('a', 'omp'),
      local.logs('a', 'dsh'),
      local.sessionActive('a', 'dsh'),
      local.agentTasks('a', 'dsh'),
      local.subagents('a', true, 'dsh'),
      local.subagentMessagesPage('a', 'sub-1', undefined, 'dsh'),
    ];
    await Promise.resolve();
    // 本机直达不排队：8 个请求同刻在途；前 3 个写 POST，其余 GET
    expect(mock).toHaveBeenCalledTimes(8);
    calls.slice(0, 3).forEach((c) => c.settle(ok204()));
    calls.slice(3).forEach((c) => c.settle(okJson({})));
    await Promise.all(ps);
    const path = (u: string): string => new URL(u, 'https://x').pathname + new URL(u, 'https://x').search;
    expect(calls.map((c) => `${c.method} ${path(c.url)}`)).toEqual([
      'POST /api/agents/dsh/session-x/rm',
      'POST /api/agents/dsh/a/stop',
      'POST /api/agents/omp/a/respawn',
      'GET /api/agents/dsh/a/logs',
      'GET /api/agents/dsh/a/session-active',
      'GET /api/agents/dsh/a/tasks',
      'GET /api/agents/dsh/a/subagents?finished=1',
      'GET /api/agents/dsh/a/subagents/sub-1/messages/page',
    ]);
  });

  it('driver 缺省 = claude（向后兼容，本机 claude 会话路径零变化）', async () => {
    const { mock, calls } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const local = makeApi('', null);
    const rm = local.removeAgent('a');
    const stop = local.stopAgent('a');
    await Promise.resolve();
    calls.forEach((c) => c.settle(ok204()));
    await Promise.all([rm, stop]);
    const path = (u: string): string => new URL(u, 'https://x').pathname;
    expect(path(calls[0].url)).toBe('/api/agents/claude/a/rm');
    expect(path(calls[1].url)).toBe('/api/agents/claude/a/stop');
  });

  it('远端写请求不排队（并发 POST 同刻在途）但同样挂超时；204 成功语义不变', async () => {
    vi.useFakeTimers();
    const { mock, calls } = hangFetch();
    vi.stubGlobal('fetch', mock);
    const remote = makeApi('https://r.example', 'inst1');
    const send = remote.sendMessage('a', 'hello');
    const interrupt = remote.interruptAgent('a');
    expect(mock).toHaveBeenCalledTimes(2); // 写请求不排队：同刻都在途
    expect(calls[0].method).toBe('POST');
    expect(calls[1].method).toBe('POST');
    // 204 成功语义不变（超时推进时钟前放行）
    calls[1].settle(ok204());
    await expect(interrupt).resolves.toBeUndefined();
    // 挂起的写请求在 deadline 处以 AbortError 拒绝
    const guard = expect(send).rejects.toMatchObject({ name: 'AbortError' });
    await vi.advanceTimersByTimeAsync(REMOTE_TIMEOUT_MS);
    await guard;
  });
});
