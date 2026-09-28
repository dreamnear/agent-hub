import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { useQuery, useQueryClient, useQueries } from '@tanstack/react-query';
import {
  api,
  openEventStream,
  AuthError,
  subscribeAuthFailed,
  authFailedSnapshot,
  type Api,
} from '../api';
import { makeApi } from '../api';
import { fetchInstances } from '../instances';
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
  mode: 'direct' | 'ssh-tunnel';
  baseUrl: string;
  config: InstanceConfig | null;
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
      list.push({
        id: inst.id,
        name: inst.name,
        mode: inst.mode,
        baseUrl: inst.mode === 'direct' ? (inst.url ?? '') : inst.localPort != null ? `http://127.0.0.1:${inst.localPort}` : '',
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
  const [wsDown, setWsDown] = useState<Record<string, boolean>>({});
  useEffect(() => {
    const closes = ctxs.map((ctx) => {
      const key = ctx.id ?? '';
      return openEventStream(
        () => {
          qc.invalidateQueries({ queryKey: ['agents', ctx.id ?? null] });
        },
        ctx.id,
        ctx.baseUrl,
        (up) =>
          setWsDown((m) => {
            const next = !up;
            return m[key] === next ? m : { ...m, [key]: next }; // 值不变不触发重渲染
          }),
      );
    });
    return () => closes.forEach((c) => c());
  }, [ctxs, qc]);

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

interface TunnelRetryState {
  /** 已发生的自动拉起次数（agents 恢复成功即整条复位） */
  fails: number;
  /** 下次允许自动拉起的时间戳（Date.now() 口径） */
  nextAt: number;
  /** 在途去重：拉起未落定不再发起（react-query retry 会重入 catch） */
  inflight: boolean;
}
const tunnelRetries = new Map<string, TunnelRetryState>();

/** agents 失败驱动的自动隧道复活。返回是否真正发起（被去重/退避/上限拦截返回 false）。 */
export function autoTunnelStart(
  id: string,
  start: (id: string) => Promise<unknown>,
  onSuccess: () => void,
  now: number = Date.now(),
): boolean {
  const s = tunnelRetries.get(id) ?? { fails: 0, nextAt: 0, inflight: false };
  if (s.inflight || s.fails >= TUNNEL_MAX_AUTO_STARTS || now < s.nextAt) return false;
  s.inflight = true;
  tunnelRetries.set(id, s);
  void start(id)
    .then(() => onSuccess())
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

/** agents 拉取成功即复位该实例退避状态（远端恢复后自动拉起能力完整还原） */
export function resetTunnelBackoff(id: string): void {
  tunnelRetries.delete(id);
}

/** 曾失败标记（r80）：never-successful 的查询每次重探会 pending↔error 翻转，
 * isError 期间离线门打开 → 整栏 loading 抖动。一旦失败即保持降级（置灰），
 * 拉取成功才清除——与 resetTunnelBackoff 同生命周期。键与 offline 一致（null=''）。 */
const everErrored = new Set<string>();

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

  // 并行拉取每实例 agents，合并并打 instanceId 标。
  // 任务10（自动重连）：出错实例每 5s 重探一次，成功即自动复活（正常时 WS 事件驱动刷新）；
  // 401 不重试（凭据失效非瞬态，重试白打）；其余错误快速失败进离线态。
  const queries = useQueries({
    queries: registry.instances.map((ctx) => ({
      queryKey: ['agents', ctx.id ?? null] as const,
      queryFn: () => {
        const k = ctx.id ?? '';
        const a = registry.apiFor(ctx.id);
        return a
          .listAgents(true)
          .then((list) => {
            // 恢复：退避与降级标记整条复位（r80——成功后自动拉起与正常展示完整还原）
            resetTunnelBackoff(k);
            everErrored.delete(k);
            return list.map((agent) => ({ ...agent, instanceId: ctx.id }));
          })
          .catch((e: unknown) => {
            // 任务10：ssh-tunnel 实例离线联动隧道复活——start 幂等（运行中返回现端口；
            // 服务端重连耗尽后可重新拉起），成功后重拉清单取新 localPort 再连。
            // r80：改走退避 + 上限 + 在途去重（原无条件 start 在远端持续不可达时自激风暴）
            everErrored.add(k);
            if (ctx.mode === 'ssh-tunnel' && ctx.id != null) {
              autoTunnelStart(ctx.id, (id) => api.tunnelStart(id), () =>
                qc.invalidateQueries({ queryKey: ['instances'] }),
              );
            }
            throw e;
          });
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
    const merged: AgentSummary[] = [];
    for (const q of queries) {
      if (Array.isArray(q.data)) merged.push(...q.data);
    }
    return merged;
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

  return { data, grouped: groupAgents(filterFinishedSubagents(data)), isLoading: instLoading || (queries.some((q) => q.isPending) && !anyDegraded), tree: tree.data ?? [], registry, instances: registry.instances, offline, authError };
}