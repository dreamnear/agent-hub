/// 实例健康状态机（agent-hub-instance-health，#19 根修）。
/// 语义（主会话定案，参数可微调不可改语义）：每实例三态 online/degraded/offline——
/// 管理通道抖一下（清单拉取失败）不许把整栏压暗；实例真实响应（被动存活信号）
/// 随时把状态救回 online。替代旧 `offline = isError ∨ wsDown ∨ everErrored`
/// 三管理面信号直推导 + everErrored 无限闩锁（单次失败即置灰且长期记账）。
///
/// 信号与转移：
/// - 存活（最强）：被动（实例域 API 成功 / events 帧）或主动轮询成功 → 立即回
///   online，失败计数清零（无闩锁）。
/// - 清单拉取失败：连续失败 +1；计数 ≥1 即 degraded（不置灰，chip 提示）。
/// - offline（唯一置灰条件）：连续失败 ≥ OFFLINE_FAIL_THRESHOLD 且首轮失败跨度
///   ≥ OFFLINE_MIN_SPAN_MS 且期间无任何存活信号 → 置灰 + 显式文案。
/// - WS 断开不进本状态机（useRemoteInstances 的 wsDown 独立持有）：断开单独发生
///   → 至多 degraded（事件延迟 ≠ 实例不可达）；与 statusOf 合成时不拦 offline。
///
/// 本文件零 React 依赖：纯函数可测；store 是模块级单例（useSyncExternalStore 消费），
/// 快照只含各实例 status（被动心跳不换引用 → 心跳风暴零重渲染）。

export type HealthStatus = 'online' | 'degraded' | 'offline';

/** offline 判定的连续失败次数下限 */
export const OFFLINE_FAIL_THRESHOLD = 3;
/** offline 判定的最小失败跨度（首败起算，ms）。与清单 5s 错误重探节奏合拍：
 * ≥3 次通常 ~2s 内凑满，跨度 20s 是真实门槛——瞬态抖动远够不到。 */
export const OFFLINE_MIN_SPAN_MS = 20_000;

/** 单实例健康机内部状态（status 由 fails/firstFailAt 推导；lastAliveAt 记账用） */
export interface InstanceHealth {
  status: HealthStatus;
  /** 连续清单拉取失败计数（任何存活/成功即清零） */
  fails: number;
  /** 本轮连续失败的首败时刻（null = 无进行中的失败轮次） */
  firstFailAt: number | null;
  /** 最近一次存活信号时刻（被动 or 主动；判定窗口「期间无存活」由
   * 存活清零计数结构性保证，此字段为诊断/报告留痕） */
  lastAliveAt: number | null;
}

export function initialHealth(): InstanceHealth {
  return { status: 'online', fails: 0, firstFailAt: null, lastAliveAt: null };
}

/** 存活信号（被动 API 成功 / events 帧 / 主动轮询成功同型）：最强信号——立即回
 * online + 计数清零。重复 online 间的心跳只更新 lastAliveAt，不产生状态迁移。 */
export function onAlive(h: InstanceHealth, now: number): InstanceHealth {
  if (h.status === 'online' && h.fails === 0 && h.firstFailAt === null) {
    return h.lastAliveAt === now ? h : { ...h, lastAliveAt: now };
  }
  return { status: 'online', fails: 0, firstFailAt: null, lastAliveAt: now };
}

/** 清单拉取失败：计数 +1；≥阈值且跨度足 → offline，否则 degraded。
 * 期间若有存活信号，计数已在存活时清零（结构性保证「窗口内无存活」）。 */
export function onListFail(h: InstanceHealth, now: number): InstanceHealth {
  const fails = h.fails + 1;
  const firstFailAt = h.firstFailAt ?? now;
  const offline =
    fails >= OFFLINE_FAIL_THRESHOLD && now - firstFailAt >= OFFLINE_MIN_SPAN_MS;
  return {
    status: offline ? 'offline' : 'degraded',
    fails,
    firstFailAt,
    lastAliveAt: h.lastAliveAt,
  };
}

/** 时间推移重推：失败轮次仍在进行（未恢复）时，跨度到点即由 degraded 翻
 * offline——不等下一次失败事件（清单错误重探 5s 一拍，纯事件驱动会晚一拍）。 */
export function evaluate(h: InstanceHealth, now: number): InstanceHealth {
  if (
    h.status === 'degraded' &&
    h.fails >= OFFLINE_FAIL_THRESHOLD &&
    h.firstFailAt != null &&
    now - h.firstFailAt >= OFFLINE_MIN_SPAN_MS
  ) {
    return { ...h, status: 'offline' };
  }
  return h;
}

/** 最终呈现态合成：状态机结果 + WS 连通性。offline 不被 WS 拦（断 + 连续失败
 * 照判 offline）；WS 断单独发生（状态机 online）→ degraded（事件延迟 ≠ 不可达）。 */
export function statusOf(machineStatus: HealthStatus, wsDown: boolean): HealthStatus {
  if (machineStatus === 'offline') return 'offline';
  if (wsDown || machineStatus === 'degraded') return 'degraded';
  return 'online';
}

// —— 模块级 store（useSyncExternalStore）——

export interface HealthStore {
  /** 订阅快照变化；返回退订函数 */
  subscribe: (cb: () => void) => () => void;
  /** 快照：实例 key（instanceId ?? ''）→ status。仅 status 变化才换引用，
   * 心跳（lastAliveAt）不进快照 → 订阅方零无效重渲染。 */
  snapshot: () => Readonly<Record<string, HealthStatus>>;
  /** 存活信号（被动 or 主动轮询成功） */
  onAlive: (key: string, now?: number) => void;
  /** 清单拉取失败 */
  onListFail: (key: string, now?: number) => void;
  /** 时间推移重推（跨度到点 degraded→offline；store 自持定时器，也暴露给测试） */
  evaluateAll: (now?: number) => void;
  /** 实例集修剪：从清单移除的实例不再保留状态（重加同 id 从头判定） */
  retain: (keys: ReadonlySet<string>) => void;
  /** 测试隔离：清空全部状态并停定时器（生产不调用） */
  __reset: () => void;
}

export function createHealthStore(): HealthStore {
  const states = new Map<string, InstanceHealth>();
  const listeners = new Set<() => void>();
  let snap: Readonly<Record<string, HealthStatus>> = {};
  let timer: ReturnType<typeof setTimeout> | null = null;

  const stateOf = (key: string): InstanceHealth => states.get(key) ?? initialHealth();

  const recompute = (): void => {
    const next: Record<string, HealthStatus> = {};
    for (const [k, s] of states) next[k] = s.status;
    // 键集合不变且各 status 不变 → 沿用旧引用（心跳只改 lastAliveAt 的场景）
    const oldKeys = Object.keys(snap);
    if (
      oldKeys.length === states.size &&
      oldKeys.every((k) => snap[k] === next[k])
    ) {
      return;
    }
    snap = next;
  };

  const notify = (): void => {
    listeners.forEach((l) => l());
  };

  /** 最近一个「待跨点」（degraded 且次数已够、只欠跨度）→ 定时器到点重推 */
  const armTimer = (): void => {
    if (timer != null) {
      clearTimeout(timer);
      timer = null;
    }
    const now = Date.now();
    let due = Infinity;
    for (const s of states.values()) {
      if (
        s.status === 'degraded' &&
        s.fails >= OFFLINE_FAIL_THRESHOLD &&
        s.firstFailAt != null
      ) {
        due = Math.min(due, s.firstFailAt + OFFLINE_MIN_SPAN_MS - now);
      }
    }
    if (due !== Infinity) {
      timer = setTimeout(() => {
        timer = null;
        evaluateAll();
      }, Math.max(due, 0));
    }
  };

  const apply = (key: string, next: InstanceHealth): void => {
    const prev = stateOf(key);
    states.set(key, next);
    // 状态机结果没变（含 lastAliveAt 心跳）→ 不换快照不通知；定时器仍需复核
    if (
      prev.status !== next.status ||
      prev.fails !== next.fails ||
      prev.firstFailAt !== next.firstFailAt
    ) {
      recompute();
      notify();
    }
    armTimer();
  };

  function evaluateAll(now: number = Date.now()): void {
    let changed = false;
    for (const [k, s] of states) {
      const next = evaluate(s, now);
      if (next !== s) {
        states.set(k, next);
        changed = true;
      }
    }
    if (changed) {
      recompute();
      notify();
    }
    armTimer();
  }

  return {
    subscribe: (cb) => {
      listeners.add(cb);
      return () => {
        listeners.delete(cb);
      };
    },
    snapshot: () => snap,
    onAlive: (key, now = Date.now()) => apply(key, onAlive(stateOf(key), now)),
    onListFail: (key, now = Date.now()) => apply(key, onListFail(stateOf(key), now)),
    evaluateAll,
    retain: (keys) => {
      const stale = [...states.keys()].filter((k) => !keys.has(k));
      if (stale.length === 0) return;
      for (const k of stale) states.delete(k);
      recompute();
      notify();
      armTimer();
    },
    __reset: () => {
      if (timer != null) clearTimeout(timer);
      timer = null;
      states.clear();
      recompute();
      // 不 notify：测试隔离用，订阅方自行重取快照
    },
  };
}

/** 全局单例：api 层存活信号与 hooks 层消费共用一条状态线 */
export const instanceHealth = createHealthStore();
