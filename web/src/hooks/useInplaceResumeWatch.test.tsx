// @vitest-environment happy-dom
// watcher hook 行为锚定：启动即拉基线 / 心跳加速 / 命中静默收尾 / 有界超时报错
// / 实例维复合键命中（agent-hub-resume-inplace #21 路由 2 后半段）。

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import {
  RESUME_WATCH_HEARTBEAT_MS,
  RESUME_WATCH_TIMEOUT_MS,
  useInplaceResumeWatch,
} from './useInplaceResumeWatch';
import type { AgentSummary } from '../types';

function agent(over: Partial<AgentSummary>): AgentSummary {
  return {
    driver: 'claude',
    id: 'agt-sel',
    name: null,
    cwd: '/repo/proj',
    kind: null,
    rawState: null,
    group: 'other',
    detail: null,
    tokens: null,
    context_tokens: null,
    startedAt: null,
    sessionId: 'live-sess',
    instanceId: null,
    ...over,
  };
}

interface Harness {
  refresh: ReturnType<typeof vi.fn>;
  onTimeout: ReturnType<typeof vi.fn>;
  start: ReturnType<typeof useInplaceResumeWatch>;
  rerender: (agents: AgentSummary[]) => void;
}

function mount(initial: AgentSummary[]): Harness {
  const refresh = vi.fn();
  const onTimeout = vi.fn();
  const h = renderHook(({ agents }: { agents: AgentSummary[] }) => {
    const start = useInplaceResumeWatch({ agents, onTimeout, refresh });
    return start;
  }, { initialProps: { agents: initial } });
  return {
    refresh,
    onTimeout,
    start: h.result.current,
    rerender: (agents) => h.rerender({ agents }),
  };
}

describe('useInplaceResumeWatch', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const target = { agentId: 'agt-sel', instanceId: null, targetSessionId: 'dead-sess' };

  it('启动即 refresh 拉基线，心跳期内每跳 refresh 一次', () => {
    const h = mount([agent({})]);
    act(() => h.start(target));
    expect(h.refresh).toHaveBeenCalledTimes(1);
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_HEARTBEAT_MS);
    });
    expect(h.refresh).toHaveBeenCalledTimes(2);
    expect(h.onTimeout).not.toHaveBeenCalled();
  });

  it('watcher 跟随命中（sessionId 变为目标）→ 静默收尾，不再心跳', () => {
    const h = mount([agent({})]);
    act(() => h.start(target));
    act(() => {
      h.rerender([agent({ sessionId: 'dead-sess' })]);
    });
    expect(h.onTimeout).not.toHaveBeenCalled();
    const calls = h.refresh.mock.calls.length;
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_HEARTBEAT_MS * 3);
    });
    expect(h.refresh.mock.calls.length).toBe(calls); // 收尾后心跳停止
  });

  it('超时（deadline 到）→ onTimeout 一次 + 心跳停止', () => {
    const h = mount([agent({})]);
    act(() => h.start(target));
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_TIMEOUT_MS - RESUME_WATCH_HEARTBEAT_MS);
    });
    expect(h.onTimeout).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_HEARTBEAT_MS);
    });
    expect(h.onTimeout).toHaveBeenCalledTimes(1);
    const calls = h.refresh.mock.calls.length;
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_HEARTBEAT_MS * 3);
    });
    expect(h.refresh.mock.calls.length).toBe(calls);
  });

  it('同 agentId 不同实例不命中（复合键实例维）', () => {
    const h = mount([agent({ instanceId: 'remote-1', sessionId: 'dead-sess' })]);
    act(() => h.start(target)); // 目标 instanceId=null
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_TIMEOUT_MS);
    });
    expect(h.onTimeout).toHaveBeenCalledTimes(1); // 实例不匹配 → 视为未跟随
  });

  it('重复 start 覆盖前一个目标（重置 deadline）', () => {
    const h = mount([agent({})]);
    act(() => h.start(target));
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_TIMEOUT_MS - 1000);
    });
    expect(h.onTimeout).not.toHaveBeenCalled();
    act(() => h.start({ ...target, targetSessionId: 'dead-sess-2' }));
    act(() => {
      vi.advanceTimersByTime(RESUME_WATCH_TIMEOUT_MS - 2000);
    });
    expect(h.onTimeout).not.toHaveBeenCalled(); // deadline 已重置
  });
});
