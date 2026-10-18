// 实例健康状态机（agent-hub-instance-health）纯函数与 store 单测。
// 语义红线：清单失败 ≥1 即 degraded（不置灰）；offline 须连续 ≥3 次 且 跨度 ≥20s；
// 任何存活/成功立即回 online 并清零（无 everErrored 式闩锁）；WS 断开单独发生至多
// degraded。参数（3 次 / 20s）可微调、语义不可改——断言全部走导出常量。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  OFFLINE_FAIL_THRESHOLD,
  OFFLINE_MIN_SPAN_MS,
  createHealthStore,
  evaluate,
  initialHealth,
  onAlive,
  onListFail,
  statusOf,
} from './instanceHealth';

const T0 = 1_000_000;

describe('状态机转移（onAlive / onListFail / evaluate）', () => {
  it('初始即 online；单次清单失败 → degraded（不置灰档）', () => {
    expect(initialHealth().status).toBe('online');
    const s1 = onListFail(initialHealth(), T0);
    expect(s1.status).toBe('degraded');
    expect(s1.fails).toBe(1);
    expect(s1.firstFailAt).toBe(T0);
  });

  it('连续失败 <3 次不 offline（哪怕跨度很长）', () => {
    let s = onListFail(initialHealth(), T0);
    s = onListFail(s, T0 + OFFLINE_MIN_SPAN_MS);
    expect(s.status).toBe('degraded');
    expect(s.fails).toBe(2);
  });

  it('连续 ≥3 次但跨度 <20s 仍 degraded（双条件缺一不可）', () => {
    let s = initialHealth();
    for (let i = 0; i < OFFLINE_FAIL_THRESHOLD; i++) {
      s = onListFail(s, T0 + i * 1000);
    }
    expect(s.fails).toBe(OFFLINE_FAIL_THRESHOLD);
    expect(s.status).toBe('degraded');
  });

  it('连续 ≥3 次且跨度 ≥20s → offline（唯一置灰条件）', () => {
    let s = onListFail(initialHealth(), T0);
    s = onListFail(s, T0 + 10_000);
    s = onListFail(s, T0 + OFFLINE_MIN_SPAN_MS); // 首败起跨度恰 20s
    expect(s.status).toBe('offline');
  });

  it('跨度到点由时间推移翻 offline（evaluate，不等下一次失败事件）', () => {
    let s = initialHealth();
    for (let i = 0; i < 5; i++) s = onListFail(s, T0 + i * 1000); // 5 连败但跨度仅 4s
    expect(s.status).toBe('degraded');
    // 未到点不翻；到点才翻（offline 只经 onAlive 救回，时间不回退状态）
    s = evaluate(s, T0 + OFFLINE_MIN_SPAN_MS - 1);
    expect(s.status).toBe('degraded');
    s = evaluate(s, T0 + OFFLINE_MIN_SPAN_MS);
    expect(s.status).toBe('offline');
  });

  it('被动救活（用户核心要求）：degraded/offline 下任一存活信号立即回 online 并清零', () => {
    let s = initialHealth();
    for (let i = 0; i < 5; i++) s = onListFail(s, T0 + i * 10_000);
    expect(s.status).toBe('offline');
    const saved = onAlive(s, T0 + 60_000);
    expect(saved.status).toBe('online');
    expect(saved.fails).toBe(0);
    expect(saved.firstFailAt).toBeNull();
    expect(saved.lastAliveAt).toBe(T0 + 60_000);
  });

  it('主动轮询成功同型：清零 + 回 online（无 everErrored 式记账）', () => {
    let s = onListFail(initialHealth(), T0);
    s = onAlive(s, T0 + 100);
    expect(s.status).toBe('online');
    // 恢复后再失败：新一轮计数从头起算（旧 streak 不复活）
    s = onListFail(s, T0 + 200);
    expect(s.fails).toBe(1);
    expect(s.firstFailAt).toBe(T0 + 200);
    expect(s.status).toBe('degraded');
  });

  it('online 间重复心跳只更新 lastAliveAt（不产生迁移）', () => {
    const s = onAlive(initialHealth(), T0);
    const s2 = onAlive(s, T0 + 1);
    expect(s2.status).toBe('online');
    expect(s2.fails).toBe(0);
    expect(s2.lastAliveAt).toBe(T0 + 1);
  });
});

describe('statusOf（最终呈现合成：状态机 + WS 连通性）', () => {
  it('WS 断开单独发生 → degraded 不 offline（事件延迟 ≠ 实例不可达）', () => {
    expect(statusOf('online', true)).toBe('degraded');
  });

  it('WS 断 + 连续清单失败照判 offline（WS 不拦置灰）', () => {
    expect(statusOf('offline', true)).toBe('offline');
    expect(statusOf('offline', false)).toBe('offline');
  });

  it('WS 正常时呈现态 = 状态机态', () => {
    expect(statusOf('online', false)).toBe('online');
    expect(statusOf('degraded', false)).toBe('degraded');
  });
});

describe('store（useSyncExternalStore 单例语义）', () => {
  let store: ReturnType<typeof createHealthStore>;
  beforeEach(() => {
    vi.useFakeTimers();
    store = createHealthStore();
  });
  afterEach(() => {
    store.__reset();
    vi.useRealTimers();
  });

  it('快照只含 status；状态变化才换引用并通知（心跳不惊动订阅方）', () => {
    const seen: string[] = [];
    store.subscribe(() => seen.push(JSON.stringify(store.snapshot())));
    store.onListFail('i1', T0);
    expect(store.snapshot()).toEqual({ i1: 'degraded' });
    // online 间心跳（i2 从未失败）：lastAliveAt 变了但 status 没变 → 不通知不换引用
    store.onAlive('i2', T0);
    const before = store.snapshot();
    store.onAlive('i2', T0 + 1);
    expect(store.snapshot()).toBe(before);
    expect(seen).toEqual([JSON.stringify({ i1: 'degraded' })]);
    // 存活救活 degraded → 通知 + 回 online
    store.onAlive('i1', T0 + 2);
    expect(store.snapshot()).toEqual({ i1: 'online', i2: 'online' });
  });

  it('跨度到点自动翻 offline（内部定时器，无需新失败事件）', () => {
    let s = store.snapshot();
    store.onListFail('i1', T0);
    store.onListFail('i1', T0 + 10_000);
    store.onListFail('i1', T0 + 12_000);
    s = store.snapshot();
    expect(s.i1).toBe('degraded');
    vi.advanceTimersByTime(OFFLINE_MIN_SPAN_MS - 12_000 + 1);
    expect(store.snapshot().i1).toBe('offline');
    expect(s.i1).toBe('degraded'); // 旧快照引用不被原地改写
  });

  it('恢复清零后定时器不再翻 offline（到点空转）', () => {
    store.onListFail('i1', T0);
    store.onListFail('i1', T0 + 10_000);
    store.onListFail('i1', T0 + 12_000);
    store.onAlive('i1', T0 + 13_000); // 被动救活
    vi.advanceTimersByTime(OFFLINE_MIN_SPAN_MS * 2);
    expect(store.snapshot().i1).toBe('online');
  });

  it('retain 修剪清单外实例；__reset 清空', () => {
    store.onListFail('i1', T0);
    store.onListFail('i2', T0);
    store.retain(new Set(['i1']));
    expect(store.snapshot()).toEqual({ i1: 'degraded' });
    store.__reset();
    expect(store.snapshot()).toEqual({});
  });
});
