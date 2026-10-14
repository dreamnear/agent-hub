import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { useQuery, useQueryClient, useQueries } from '@tanstack/react-query';
import {
  api,
  openEventStream,
  AuthError,
  probeInstanceAuth,
  subscribeAuthFailed,
  authFailedSnapshot,
  REMOTE_TIMEOUT_MS,
  type Api,
} from '../api';
import { makeApi } from '../api';
import { fetchInstances, instanceBaseUrl } from '../instances';
import type { AgentSummary, GitTreeGroup, Group, InstanceConfig } from '../types';

export interface GroupedAgents {
  needsInput: AgentSummary[];
  working: AgentSummary[];
  completed: AgentSummary[];
  other: AgentSummary[];
}

export function groupAgents(agents: AgentSummary[]): GroupedAgents {
  const needsInput: AgentSummary[] = [];
  const working: AgentSummary[] = [];
  const completed: AgentSummary[] = [];
  const other: AgentSummary[] = [];
  for (const a of agents) {
    switch (a.group as Group) {
      case 'needs_input':
        needsInput.push(a);
        break;
      case 'working':
        working.push(a);
        break;
      case 'completed':
        completed.push(a);
        break;
      default:
        other.push(a);
        break;
    }
  }
  return { needsInput, working, completed, other };
}

/// interactive 会话过滤（P5 preview 语义修正）
export function filterInteractive(agents: AgentSummary[], showInteractive: boolean): AgentSummary[] {
  if (showInteractive) return agents;
  return agents.filter((a) => a.kind !== 'interactive');
}

/// 已结束 subagent 过滤（r78）
export function filterFinishedSubagents(agents: AgentSummary[]): AgentSummary[] {
  return agents.filter((a) => !(a.kind === 'subagent' && a.group !== 'working' && a.group !== 'needs_input'));
}

/// 工程族归属（反馈轮 23 主判据）
function familyOf(p: string, anchors: string[]): string | null {
  let best: string | null = null;
  for (const a of anchors) {
    if (p === a || p.startsWith(a.endsWith('/') ? a : `${a}/`)) {
      if (best === null || a.length < best.length) best = a;
    }
  }
  return best;
}

/// 工程过滤（反馈轮 23）：仅本机实例启用工程族锚（远程 cwd 不落本机族锚，整组保留）
export function filterAgents(
  agents: AgentSummary[],
  checked: ReadonlySet<string>,
  anchors: string[] = [],
  remoteIds: ReadonlySet<string | null> = new Set(),
): AgentSummary[] {
  if (checked.size === 0) return agents;
  const activeFamilies = new Set<string>();
  for (const c of checked) {
    const f = familyOf(c, anchors);
    if (f !== null) activeFamilies.add(f);
  }
  return agents.filter((a) => {
    // 远程实例组不套本机族锚——远程 cwd 不落本机族锚是预期而非漏网（任务6 语义前移）
    if (a.instanceId != null && remoteIds.has(a.instanceId)) return true;
    if (a.cwd == null) return false;
    const f = familyOf(a.cwd, anchors);
    return f !== null && activeFamilies.has(f);
  });
}

/// git 树 → 工程族锚
export function familyAnchors(tree: GitTreeGroup[]): string[] {
  return tree.map((g) => g.main.path);
}

/** 实例 id → 该实例的 per-instance api（供各消费组件按 agent.instanceId 路由）。
 * 本机（null）→ 现状 api。 */
export interface InstanceRegistry {
  /** 本机 + 各远程的实例清单（含隐式本机） */
  instances: InstanceCtx[];
  /** agent.instanceId → api（本机为 api） */
  apiFor: (instanceId: string | null) => Api;
}

// —— 实例上下文（批1 任务4：per-instance 请求路由）——

export interface InstanceCtx {
  id: string | null; // null = 本机
  name: string;
  mode: 'direct' | 'ssh-tunnel' | 'node-link';
  baseUrl: string;
  config: InstanceConfig | null;
}

/** 实例集稳定签名：只用 WS 建立真正需要的字段（id + baseUrl）。
 * H253 请求风暴根因之一——WS effect 原以 ctxs 引用为依赖，而清单每次重拉都是
 * 新数组，于是每次失效都拆建全部 WS，新 WS 的 onopen 又各自 invalidate agents。
 * 签名不变 = 实例集没变 = 不许拆建。 */
export function instanceSetKey(ctxs: InstanceCtx[]): string {
  return ctxs.map((c) => `${c.id ?? ''}:${c.baseUrl}`).join('|');
}

/** agents 失效节流窗（新会话卡死诊断定值）：hub 的 spawn_poll 每 POLL_SECS 无条件
 * 广播 Tick，新会话启动期 timeline.jsonl 连续写入再叠加 JobsChanged 风暴——窗口过窄
 * 会让每次重取都打一次真 claude CLI spawn（hub 侧 0.2-2s）+ 逐 agent 读盘，与浏览器/
 * 新 claude 进程同机抢 CPU = 页面卡死体感。5s 上限把风暴重取率砍 60%，列表新鲜度
 * 人眼无感；首条仍立即放行（onopen 恢复/首个事件即刻刷新不受节流）。 */
export const AGENTS_INVALIDATE_THROTTLE_MS = 5_000;

/** 事件节流（首条立即 + 窗口内合并 + 末条补发）：失效重取是幂等的，合并只影响
 * 「什么时候刷新」不影响最终态；末条补发保证不丢最后一次变更。 */
export function throttleEvent(
  fn: () => void,
  ms: number,
): { run: () => void; cancel: () => void } {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending = false;
  const run = (): void => {
    if (timer != null) {
      pending = true;
      return;
    }
    fn();
    timer = setTimeout(() => {
      timer = null;
      if (!pending) return;
      pending = false;
      run();
    }, ms);
  };
  return {
    run,
    cancel: () => {
      if (timer != null) clearTimeout(timer);
      timer = null;
      pending = false;
    },
  };
}

export function useRemoteInstances(): {
  instances: InstanceCtx[];
  registry: InstanceRegistry;
  isLoading: boolean;
  /** 实例 id（null=本机，键 ''）→ WS 断开（任务10 离线信号源） */
  wsDown: Record<string, boolean>;
} {
  const qc = useQueryClient();
  const { data = [], isLoading } = useQuery<InstanceConfig[]>({
    queryKey: ['instances'],
    queryFn: fetchInstances,
    staleTime: 30_000,
  });

  // ssh-tunnel 实例：确保隧道已启动（App 加载时幂等 start）——批1 只做清单解析；
  // 隧道确保启动的完整流（start → 取 localPort → 连）在任务9 实现，此处仅解析本地端口。
  const ctxs = useMemo(() => {
    const list: InstanceCtx[] = [
      { id: null, name: '本机', mode: 'direct', baseUrl: '', config: null },
    ];
    for (const inst of data) {
      // 设备隔离修复：base url 统一经 instances.ts 单一来源（ssh-tunnel=同源代理，
      // 不再拼 127.0.0.1:{localPort}——那只在本机可达）
      list.push({
        id: inst.id,
        name: inst.name,
        mode: inst.mode,
        baseUrl: instanceBaseUrl(inst) ?? '',
        config: inst,
      });
    }
    return list;
  }, [data]);

  const registry: InstanceRegistry = useMemo(() => {
    const byId = new Map<string | null, InstanceCtx>();
    for (const c of ctxs) byId.set(c.id, c);
    return {
      instances: ctxs,
      apiFor: (instanceId) => {
        const ctx = byId.get(instanceId);
        if (ctx == null || ctx.id == null) return api;
        return makeApi(ctx.baseUrl, ctx.id);
      },
    };
  }, [ctxs]);

  // App 加载幂等确保 ssh-tunnel 隧道已启动（任务9）：无条件调 start（服务端幂等——
  // 已运行返回现有 localPort，未运行拉起；已停/重启后遗留 localPort 的隧道同样复活）。
  // 随后 invalidate 重拉清单 → localPort 就位 → baseUrl 可达 → WS/agents 连上。
  // 会话内用户手动 stop 不被对抗：依赖串含 localPort，清单未变 effect 不重跑。
  const configuredSsh = data.filter((c) => c.mode === 'ssh-tunnel');
  useEffect(() => {
    let alive = true;
    const ensure = async (): Promise<void> => {
      for (const inst of configuredSsh) {
        try {
          await api.tunnelStart(inst.id);
          if (alive) qc.invalidateQueries({ queryKey: ['instances'] });
        } catch {
          // 隧道启动失败：留给离线降级（任务10 显示置灰）；此处不报错不循环
        }
      }
    };
    void ensure();
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 每次清单变化判定一次即可
  }, [configuredSsh.map((c) => `${c.id}:${c.localPort}`).join('|')]);

  // 每实例一条事件流 WS（各认各的 token、各自 5s 重连）。
  // 任务10：WS 连通性是离线信号源——HTTP 查询有缓存数据时实例死亡不会触发重发，
  // WS close 才是第一手信号；重连成功回调里顺带失效重取（api 层 onopen）。
  // H253：依赖从 ctxs 引用换成实例集稳定签名——清单引用抖动不再拆建全部 WS。
  const wsKey = instanceSetKey(ctxs);
  const [wsDown, setWsDown] = useState<Record<string, boolean>>({});
  useEffect(() => {
    const closers: (() => void)[] = [];
    const gates: { cancel: () => void }[] = [];
    for (const ctx of ctxs) {
      const key = ctx.id ?? '';
      // 事件合并（2s）：远端高频事件（JobsChanged 批量变更）每条都重拉 agents，
      // 会把半通慢隧道打爆成排队请求；首条立即发（onopen 恢复即刻刷新不受节流），
      // 窗口内多条合并为一次，末条补发保证最终态不丢。
      const ev = throttleEvent(
        () => qc.invalidateQueries({ queryKey: ['agents', ctx.id ?? null] }),
        AGENTS_INVALIDATE_THROTTLE_MS,
      );
      gates.push(ev);
      closers.push(
        openEventStream(
          ev.run,
          ctx.id,
          ctx.baseUrl,
          (up) =>
            setWsDown((m) => {
              const next = !up;
              return m[key] === next ? m : { ...m, [key]: next }; // 值不变不触发重渲染
            }),
        ),
      );
    }
    return () => {
      closers.forEach((c) => c());
      gates.forEach((g) => g.cancel());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 依赖稳定签名 wsKey：清单引用抖动不拆建 WS
  }, [wsKey, qc]);

  return { instances: ctxs, registry, isLoading, wsDown };
}

// —— ssh-tunnel 自动隧道复活：退避 + 上限 + 在途去重（r80 自激风暴修复）——
// 此前 agents 拉取失败即无条件 tunnelStart + invalidate(['instances'])：远端持续不可达时
// 每 ~3s 一对 tunnel/start 风暴 → WS 拆建 + 清单重拉循环 → 侧栏 render↔loading 抖动长卡。
// 现改为指数退避（2s/4s/8s/16s…封顶 30s），连续 5 次失败后停止自动拉起（实例保持既有
// 离线置灰态）；实例管理面板手动「启动隧道」（InstancesSection）直调 api.tunnelStart，
// 不经此路径，不受退避限制。
const TUNNEL_BACKOFF_BASE_MS = 2_000;
const TUNNEL_BACKOFF_MAX_MS = 30_000;
const TUNNEL_MAX_AUTO_STARTS = 5;
/** 自动拉起「成功」后的冷却期（ms）。H253：慢隧道让 agents 失败/成功交替，
 * 每次成功都 resetTunnelBackoff 清空退避计数与 5 次上限，退避被无限 re-arm；
 * 冷却期独立存 resetTunnelBackoff 之外，成功复位打不穿它。 */
export const TUNNEL_AUTO_START_COOLDOWN_MS = 60_000;

interface TunnelRetryState {
  /** 已发生的自动拉起次数（agents 恢复成功即整条复位） */
  fails: number;
  /** 下次允许自动拉起的时间戳（Date.now() 口径） */
  nextAt: number;
  /** 在途去重：拉起未落定不再发起（react-query retry 会重入 catch） */
  inflight: boolean;
}
const tunnelRetries = new Map<string, TunnelRetryState>();
/** 实例 id → 最近一次自动拉起成功的时刻；只在成功时记账（失败由退避/上限管） */
const tunnelAutoStartedAt = new Map<string, number>();

/** agents 失败驱动的自动隧道复活。返回是否真正发起（被去重/退避/上限拦截返回 false）。 */
export function autoTunnelStart(
  id: string,
  start: (id: string) => Promise<unknown>,
  onSuccess: () => void,
  now: number = Date.now(),
): boolean {
  const s = tunnelRetries.get(id) ?? { fails: 0, nextAt: 0, inflight: false };
  const lastStart = tunnelAutoStartedAt.get(id) ?? 0;
  if (
    s.inflight ||
    s.fails >= TUNNEL_MAX_AUTO_STARTS ||
    now < s.nextAt ||
    now < lastStart + TUNNEL_AUTO_START_COOLDOWN_MS
  ) {
    return false;
  }
  s.inflight = true;
  tunnelRetries.set(id, s);
  void start(id)
    .then(() => {
      tunnelAutoStartedAt.set(id, Date.now()); // 冷却只在成功时起算
      onSuccess();
    })
    .catch(() => {
      // 拉起失败：留给离线降级（侧栏置灰），不报错不循环
    })
    .finally(() => {
      // 无论 start 成败都计数——计数锚定 agents 连续失败（恢复成功在 queryFn 成功路径复位）
      const cur = tunnelRetries.get(id);
      if (cur) {
        cur.fails += 1;
        cur.nextAt =
          Date.now() + Math.min(TUNNEL_BACKOFF_BASE_MS * 2 ** (cur.fails - 1), TUNNEL_BACKOFF_MAX_MS);
        cur.inflight = false;
      }
    });
  return true;
}

/** agents 拉取成功即复位该实例退避状态（远端恢复后自动拉起能力完整还原）。
 * 冷却期（tunnelAutoStartedAt）刻意不复位——复位正是打穿退避的那个动作。 */
export function resetTunnelBackoff(id: string): void {
  tunnelRetries.delete(id);
}

/** agents 失败后的隧道处置（H253）：先问隧道在不在跑，在跑就什么都不做——
 * 「慢而半通」不等于离线。原来的「失败即 start」会 invalidate(['instances']) →
 * WS 拆建 → agents 重拉，把慢隧道滚成请求风暴。状态查询本身失败才退回 r80
 * 的退避路径（保守当没跑）。返回是否真正发起了自动拉起。 */
export async function ensureTunnelRunning(
  id: string,
  status: () => Promise<{ running: boolean }>,
  start: (id: string) => Promise<unknown>,
  onSuccess: () => void,
  now?: number,
): Promise<boolean> {
  try {
    const st = await status();
    if (st.running) return false;
  } catch {
    // 状态未知：交给退避路径判断
  }
  return autoTunnelStart(id, start, onSuccess, now);
}

/** 曾失败标记（r80）：never-successful 的查询每次重探会 pending↔error 翻转，
 * isError 期间离线门打开 → 整栏 loading 抖动。一旦失败即保持降级（置灰），
 * 拉取成功才清除——与 resetTunnelBackoff 同生命周期。键与 offline 一致（null=''）。 */
const everErrored = new Set<string>();

// —— H253 按实例隔离：单通道堵死不许拖累本机与其他实例 ——
// 浏览器对同源只有 6 条并发连接，而本机与所有远端实例的请求都打同一个 origin
// （远端走 /api/instances/{id}/p 同源代理），连接池还是 FIFO——远端慢请求吃满
// 6 条槽位时本机请求只能排队，表现就是「一个渠道堵住所有 agent / load session 变慢」。
// 两层防线：① 每实例在途上限 1（singleFlight）② 远端单次拉取有 deadline，超时即弃。

/** 同实例在途复用：已有在途请求时直接复用它，绝不并发发出第二条。重复失效、
 * WS 事件、5s 重探、react-query 的 cancelRefetch 重发全落在同一个 promise 上。
 * 结果对所有订阅者等价（同一 queryKey 的同一份 agents 列表）。 */
const inflightAgents = new Map<string, Promise<AgentSummary[]>>();
export function singleFlight(
  key: string,
  run: () => Promise<AgentSummary[]>,
): Promise<AgentSummary[]> {
  const hit = inflightAgents.get(key);
  if (hit) return hit;
  const p = run().finally(() => {
    if (inflightAgents.get(key) === p) inflightAgents.delete(key);
  });
  inflightAgents.set(key, p);
  return p;
}

/** 远端单次拉取 deadline：慢实例不长期霸占那条浏览器连接，放弃本次、走既有降级态，
 * 等 5s 重探或下一条 WS 事件再来。10s 口径与 api 层 REMOTE_TIMEOUT_MS 常量共享
 * （直连实测单次 2.8s 内完成，10s 留 3 倍余量，超时即视为半通/堵死）。
 * 本机不设限（本机慢是 hub 自身问题，不该被判死）。 */
export function fetchAgentsWithDeadline(
  a: Api,
  instanceId: string | null,
): Promise<AgentSummary[]> {
  // 默认清单 all=false（2026-10-02 全驱动展示纪律：侧栏只列活跃会话，Done/历史
  // 不进默认视野；all=true 语义保留给历史面板 opt-in）
  if (instanceId == null) return a.listAgents(false);
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), REMOTE_TIMEOUT_MS);
  return a.listAgents(false, { signal: ac.signal }).finally(() => clearTimeout(timer));
}

// —— authFailed 自愈探测（凭据失效恢复修复）——
// 查询层对 AuthError 跳过重试（凭据失效非瞬态），标记解除靠「任何一次成功请求」；
// 查询停摆后成功请求永远等不到 → 标记挂死到刷新页面。标记态实例改为低频直连轻探
// （probeInstanceAuth），成功即解除标记 + invalidate 该实例 agents 恢复正常轮询；
// 401/其他错误保持标记。正常实例零额外请求（只在 authFailed 态排探）。
export const AUTH_PROBE_MS = 30_000;

export function useAuthFailedProbe(instances: InstanceCtx[]): void {
  const qc = useQueryClient();
  // 自订阅标记集合：解除即触发重渲染 → effect 清理停探（不依赖父组件透传）
  const authFailed = useSyncExternalStore(subscribeAuthFailed, authFailedSnapshot);
  // 经 ref 取最新实例表：清单 30s 重拉换引用不重排探测定时器
  const ctxRef = useRef(instances);
  ctxRef.current = instances;
  const key = [...authFailed].sort().join('|');
  useEffect(() => {
    if (key === '') return;
    const timers = new Set<ReturnType<typeof setTimeout>>();
    for (const id of authFailed) {
      const tick = (): void => {
        const ctx = ctxRef.current.find((c) => c.id === id);
        if (ctx == null) return; // 实例已从清单移除：不探，标记随刷新消失
        void probeInstanceAuth(ctx.baseUrl, id).then((ok) => {
          if (ok) void qc.invalidateQueries({ queryKey: ['agents', id] });
        });
      };
      const arm = (): void => {
        timers.add(
          setTimeout(() => {
            tick();
            arm();
          }, AUTH_PROBE_MS),
        );
      };
      arm(); // 首探也等一个周期：标记刚打上（401 刚发生），立刻重发是白打
    }
    return () => {
      timers.forEach((t) => clearTimeout(t));
      timers.clear();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- key = authFailed 稳定签名；instances 经 ref 读最新
  }, [key, qc]);
}

/// 跨实例清单聚合 + assistant-singleton 去重（纯函数，可测）：hub 助手每
/// gateway 唯一（用户规则 2026-10-11）——同机节点挂载后本机发现与节点发现会
/// 双源各出一行 pi 助手（pi 会话 id 恒为 hub-assistant），计数翻倍语义错误。
/// 防重做在聚合层（UI 过滤 reopen 再现）；指纹 = driver 'pi'（assistant 类
/// 身份字段）；本机直连优先（local 前置后首见即取，本机无助手时节点侧首见
/// 注册为唯一实例——不永久屏蔽节点发现）。一般 agent（dec/dj 等）多节点
/// 展示语义不动（只对 pi 去重）。
export function mergeAgentLists(
  perInst: { local: boolean; agents: AgentSummary[] }[],
): AgentSummary[] {
  // 本机恒前（稳定排序保持各实例内相对序与 registry 节点序）
  const ordered = [...perInst].sort((a, b) => Number(b.local) - Number(a.local));
  const merged: AgentSummary[] = [];
  let assistantTaken = false;
  for (const { agents } of ordered) {
    for (const a of agents) {
      if (a.driver === 'pi') {
        if (assistantTaken) continue;
        assistantTaken = true;
      }
      merged.push(a);
    }
  }
  return merged;
}

export function useAgents(): {
  data: AgentSummary[];
  grouped: GroupedAgents;
  isLoading: boolean;
  tree: GitTreeGroup[];
  registry: InstanceRegistry;
  instances: InstanceCtx[];
  /** 实例 id（null=本机）→ 该实例 agents 抓取是否失败（离线判定，任务6/10 置灰依据） */
  offline: Record<string, boolean>;
  /** 实例 id → 凭据失效（该实例 401，任务10：侧栏「凭据失效」态；本机走 TokenGate 不在此列） */
  authError: Record<string, boolean>;
} {
  const { registry, isLoading: instLoading, wsDown } = useRemoteInstances();
  const qc = useQueryClient();
  const byId = new Map<string | null, InstanceCtx>();
  for (const c of registry.instances) byId.set(c.id, c);

  // 凭据失效集合（任务10）：api 层 per-instance 401 打标、成功请求自动解除
  const authFailed = useSyncExternalStore(subscribeAuthFailed, authFailedSnapshot);
  // 标记态实例的低频自愈探测（凭据失效恢复修复）：查询停摆后等不来成功请求，
  // 由探测主动解除标记并恢复轮询
  useAuthFailedProbe(registry.instances);

  // 并行拉取每实例 agents，合并并打 instanceId 标。
  // 任务10（自动重连）：出错实例每 5s 重探一次，成功即自动复活（正常时 WS 事件驱动刷新）；
  // 401 不重试（凭据失效非瞬态，重试白打）；其余错误快速失败进离线态。
  const queries = useQueries({
    queries: registry.instances.map((ctx) => ({
      queryKey: ['agents', ctx.id ?? null] as const,
      queryFn: () => {
        const k = ctx.id ?? '';
        return singleFlight(k, () =>
          fetchAgentsWithDeadline(registry.apiFor(ctx.id), ctx.id)
            .then((list) => {
              // 恢复：退避与降级标记整条复位（r80——成功后自动拉起与正常展示完整还原）
              resetTunnelBackoff(k);
              everErrored.delete(k);
              return list;
            })
            .catch((e: unknown) => {
              // 任务10：ssh-tunnel 实例离线联动隧道复活——start 幂等（运行中返回现端口；
              // 服务端重连耗尽后可重新拉起），成功后重拉清单取新 localPort 再连。
              // r80：改走退避 + 上限 + 在途去重（原无条件 start 在远端持续不可达时自激风暴）
              // H253：加 running 判定——隧道在跑（只是慢/半通）就不 start、不 invalidate
              everErrored.add(k);
              const instId = ctx.id;
              if (ctx.mode === 'ssh-tunnel' && instId != null) {
                void ensureTunnelRunning(
                  instId,
                  () => api.tunnelStatus(instId),
                  (id) => api.tunnelStart(id),
                  () => qc.invalidateQueries({ queryKey: ['instances'] }),
                );
              }
              throw e;
            }),
        ).then((list) => list.map((agent) => ({ ...agent, instanceId: ctx.id })));
      },
      staleTime: 0,
      retry: (failureCount: number, error: unknown) =>
        !(error instanceof AuthError) && failureCount < 1,
      retryDelay: 2_000,
      // useQueries 泛型流不进回调参数，显式结构标注（QueryState 无 isError 属性，判 status）
      refetchInterval: (q: { state: { status: string } }) => (q.state.status === 'error' ? 5_000 : false),
    })),
  });

  const data = useMemo(() => {
    const perInst: { local: boolean; agents: AgentSummary[] }[] = [];
    queries.forEach((q, i) => {
      if (Array.isArray(q.data)) {
        perInst.push({ local: registry.instances[i]?.id == null, agents: q.data });
      }
    });
    return mergeAgentLists(perInst);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- registry.instances 与 queries 同序同源（useRemoteInstances useMemo）
  }, [queries]);

  // 工程族锚数据源（本机 projectTree）
  const tree = useQuery<GitTreeGroup[]>({
    queryKey: ['projectTree', null],
    queryFn: api.projectTree,
    refetchInterval: 30_000,
  });

  // 实例离线判定（任务6/10）：fetch 报错 **或 WS 断开** = 离线。
  // WS 是第一手信号——HTTP 查询有缓存数据时实例死亡不会触发重发，等不出错误态；
  // 本机（null，键 ''）也计入。
  const offline: Record<string, boolean> = useMemo(() => {
    const map: Record<string, boolean> = {};
    queries.forEach((q, i) => {
      const k = registry.instances[i]?.id ?? '';
      // r80：everErrored——pending 重探期间 isError 翻转会让离线门间歇打开（整栏 loading 抖动），
      // 失败过的实例保持降级直到拉取成功
      map[k] = q.isError || wsDown[k] || everErrored.has(k) || false;
    });
    return map;
  }, [queries, wsDown]);

  // 凭据失效 → 侧栏展示用记录（任务10）
  const authError: Record<string, boolean> = useMemo(() => {
    const map: Record<string, boolean> = {};
    authFailed.forEach((id) => {
      map[id] = true;
    });
    return map;
  }, [authFailed]);

  // 任一实例离线/凭据失效时不整栏 loading（任务10：离线实例的 5s 探测会让
  // 无数据查询在 pending 间翻转，门控会造成侧栏闪烁消失）
  const anyDegraded = Object.values(offline).some(Boolean) || Object.keys(authError).length > 0;

  // 启动风暴防御：全页 loading 只看本机查询——远程实例首次加载 pending 期间
  // 不劫持本机 agent 列表（远程未就绪走 offline/everErrored 置灰态，机制见上）
  const localPending = queries.some((q, i) => registry.instances[i]?.id == null && q.isPending);

  return { data, grouped: groupAgents(filterFinishedSubagents(data)), isLoading: instLoading || (localPending && !anyDegraded), tree: tree.data ?? [], registry, instances: registry.instances, offline, authError };
}