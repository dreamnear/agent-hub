// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AgentSummary } from '../types';
import {
  autoTunnelStart,
  ensureTunnelRunning,
  fetchAgentsWithDeadline,
  filterAgents,
  filterFinishedSubagents,
  filterInteractive,
  groupAgents,
  instanceSetKey,
  mergeAgentLists,
  resetTunnelBackoff,
  singleFlight,
  TUNNEL_AUTO_START_COOLDOWN_MS,
  type InstanceCtx,
} from './useAgents';

function fake(id: string, group: AgentSummary['group']): AgentSummary {
  return {
    id,
    driver: 'claude',
    group,
    name: null,
    cwd: null,
    kind: null,
    rawState: null,
    detail: null,
    tokens: null,
  context_tokens: null,
    startedAt: null,
    sessionId: null,
  };
}

describe('groupAgents', () => {
  it('routes needs_input/working/completed/other correctly', () => {
    const agents = [
      fake('a', 'needs_input'),
      fake('b', 'working'),
      fake('c', 'completed'),
      fake('d', 'other' as unknown as AgentSummary['group']),
    ];
    const g = groupAgents(agents);
    expect(g.needsInput.map((a) => a.id)).toEqual(['a']);
    expect(g.working.map((a) => a.id)).toEqual(['b']);
    expect(g.completed.map((a) => a.id)).toEqual(['c']);
    expect(g.other.map((a) => a.id)).toEqual(['d']);
  });
});

describe('mergeAgentLists (assistant-singleton)', () => {
  const pi = (instanceId: string | null): AgentSummary => ({
    ...fake('hub-assistant', 'working'),
    driver: 'pi',
    name: 'hub-assistant',
    instanceId,
  });
  const claude = (id: string, instanceId: string | null): AgentSummary => ({
    ...fake(id, 'working'),
    instanceId,
  });
  const piIds = (m: AgentSummary[]): (string | null | undefined)[] =>
    m.filter((a) => a.driver === 'pi').map((a) => a.instanceId);

  it('A1 同机双源：本机+节点各一个 pi 助手只保留本机 1 行，一般 agent 不动', () => {
    const merged = mergeAgentLists([
      { local: true, agents: [claude('l1', null), pi(null)] },
      { local: false, agents: [claude('n1', 'instR'), pi('instR')] },
    ]);
    expect(piIds(merged)).toEqual([null]);
    expect(merged.filter((a) => a.driver !== 'pi').map((a) => a.id)).toEqual(['l1', 'n1']);
  });

  it('A2 本机无助手：节点侧 pi 注册为唯一实例（不永久屏蔽节点发现）', () => {
    const merged = mergeAgentLists([
      { local: true, agents: [claude('l1', null)] },
      { local: false, agents: [pi('instR')] },
    ]);
    expect(piIds(merged)).toEqual(['instR']);
  });

  it('多节点各带助手且本机不在场：registry 序首见胜出', () => {
    const merged = mergeAgentLists([
      { local: false, agents: [pi('instR1')] },
      { local: false, agents: [pi('instR2')] },
    ]);
    expect(piIds(merged)).toEqual(['instR1']);
  });

  it('local 标志权威：本机条目不在数组首位也前置取助手', () => {
    const merged = mergeAgentLists([
      { local: false, agents: [pi('instR')] },
      { local: true, agents: [pi(null)] },
    ]);
    expect(piIds(merged)).toEqual([null]);
  });
});

describe('filterInteractive', () => {
  const withKind = (id: string, kind: string | null): AgentSummary => ({ ...fake(id, 'working'), kind });
  const agents = [withKind('bg1', 'background'), withKind('tty', 'interactive'), withKind('null-kind', null)];

  it('hides interactive sessions by default (Hub = bg task console)', () => {
    expect(filterInteractive(agents, false).map((a) => a.id)).toEqual(['bg1', 'null-kind']);
  });

  it('shows everything when the toggle is on', () => {
    expect(filterInteractive(agents, true)).toHaveLength(3);
  });
});

describe('filterFinishedSubagents', () => {
  // r78：subagent 是临时工，跑完即释放——已结束（非 working/needs_input）的
  // kind=subagent 条目从侧栏隐藏；working/等待中的保留（看并行进度是刚需）；
  // 主会话（background 等）与 ACP 不受影响。
  const withKind = (id: string, kind: string | null, group: AgentSummary['group']): AgentSummary => ({
    ...fake(id, group),
    kind,
  });

  it('hides finished subagents (completed / other / needs-no-input buckets)', () => {
    const agents = [
      withKind('sub-done', 'subagent', 'completed'),
      withKind('sub-idle', 'subagent', 'other'),
    ];
    expect(filterFinishedSubagents(agents)).toEqual([]);
  });

  it('keeps working and waiting-for-input subagents', () => {
    const agents = [
      withKind('sub-working', 'subagent', 'working'),
      withKind('sub-ask', 'subagent', 'needs_input'),
    ];
    expect(filterFinishedSubagents(agents).map((a) => a.id)).toEqual(['sub-working', 'sub-ask']);
  });

  it('never hides main sessions even when completed', () => {
    const agents = [
      withKind('bg-done', 'background', 'completed'),
      withKind('bg-idle', 'background', 'other'),
      withKind('null-kind', null, 'completed'),
    ];
    expect(filterFinishedSubagents(agents).map((a) => a.id)).toEqual(['bg-done', 'bg-idle', 'null-kind']);
  });

  it('does not affect ACP sessions', () => {
    const agents = [withKind('acp-1', 'acp', 'completed')];
    expect(filterFinishedSubagents(agents).map((a) => a.id)).toEqual(['acp-1']);
  });
});

describe('filterAgents', () => {
  const withCwd = (id: string, cwd: string): AgentSummary => ({ ...fake(id, 'working'), cwd });
  // r51 实证形态：会话 cwd 可能是工程根、工程根下 worktree、或工程根下子目录
  const agents = [
    withCwd('root', '/proj/one'),
    withCwd('wt', '/proj/one/.worktree/plan'),
    withCwd('sub', '/proj/one/nested/dir'),
    withCwd('other', '/proj/two'),
    withCwd('nocwd', null as unknown as string),
  ];

  it('shows all when nothing is checked (filter off, r51 空集=全显)', () => {
    expect(filterAgents(agents, new Set())).toHaveLength(agents.length);
  });

  it('keeps sessions whose cwd equals a checked root', () => {
    const g = filterAgents([withCwd('root', '/proj/one'), withCwd('far', '/proj/two')], new Set(['/proj/one']), [
      '/proj/one',
      '/proj/two',
    ]);
    expect(g.map((a) => a.id)).toEqual(['root']);
  });

  it('keeps worktree sessions under a checked root (r51：等值匹配曾把它们全滤掉)', () => {
    const g = filterAgents([withCwd('wt', '/proj/one/.worktree/plan')], new Set(['/proj/one']), ['/proj/one']);
    expect(g.map((a) => a.id)).toEqual(['wt']);
  });

  it('keeps sessions in subdirectories of a checked root (r51 claude-view 场景)', () => {
    const g = filterAgents(
      [withCwd('sub', '/Users/kevin/Works/personal/projects/claude-view')],
      new Set(['/Users/kevin/Works/personal']),
      ['/Users/kevin/Works/personal'],
    );
    expect(g.map((a) => a.id)).toEqual(['sub']);
  });

  it('opencode sessions participate in family-anchor filtering (修正2 回归)：本机驱动 cwd 过滤同 claude', () => {
    // opencode 目录接既有工程白名单：非 busy 历史条目（driver='opencode'，group 完成
    // 态）走同一 cwd→族锚路径——勾选工程内的留、白名单外（如 /private/tmp、用户主目录
    // 散目录）的默认不显示；working 活跃桶由 buildInstanceGroups 单独放行（永不被过滤）
    const oc = (id: string, cwd: string, group: AgentSummary['group']): AgentSummary => ({
      ...fake(id, group),
      driver: 'opencode',
      kind: 'opencode',
      cwd,
    });
    const anchors = ['/Users/kevin/Works/personal/projects/claude-view'];
    const checked = new Set(anchors);
    const g = filterAgents(
      [
        oc('oc-in-proj', '/Users/kevin/Works/personal/projects/claude-view', 'completed'),
        oc('oc-stray', '/private/tmp/scratch', 'completed'),
        oc('oc-home', '/Users/kevin/dev/games/hyper3D_1', 'completed'),
        oc('oc-working-stray', '/private/tmp/scratch', 'working'),
      ],
      checked,
      anchors,
    );
    // 完成/空闲桶：仅勾选工程内的留；活跃桶不在 filterAgents 输入里（上层放行），
    // 这里只断言过滤语义本身
    expect(g.map((a) => a.id)).toEqual(['oc-in-proj']);
  });

  it('common-root family: checking any dec node pulls the whole dec family (r53 主判据)', () => {
    // 族锚 = git 树主仓根（最短前缀归属）；同 common-root（同主仓）即同族——
    // 勾 plan worktree 激活 dec 族，主仓根/兄弟 worktree 会话全部归入；跨族（dxq）不误扩
    const g = filterAgents(
      [
        withCwd('dec-plan-1', '/proj/dec/.worktree/plan'),
        withCwd('dec-plan-2', '/proj/dec/.worktree/plan'),
        withCwd('dec-main', '/proj/dec'),
        withCwd('dec-staging', '/proj/dec/.worktree/staging'),
        withCwd('dxq-wt', '/proj/dxq/.worktree/staging'),
      ],
      new Set(['/proj/dec/.worktree/plan']),
      ['/proj/dec', '/proj/dxq'],
    );
    expect(g.map((a) => a.id).sort()).toEqual(['dec-main', 'dec-plan-1', 'dec-plan-2', 'dec-staging']);
  });

  it('personal family: root/submodule/middle-layer cwd all fold into the personal anchor (r53 personal 族)', () => {
    // 族锚取最短前缀（最外层）：personal 主仓、其下 submodule（src/claude-view）、
    // 未注册中间层 cwd 三者同族——勾任一节点（含 submodule 节点）全族显示
    const anchors = [
      '/Users/kevin/Works/personal',
      '/Users/kevin/Works/personal/projects/claude-view/src/claude-view',
    ];
    const sessions = [
      withCwd('middle', '/Users/kevin/Works/personal/projects/claude-view'),
      withCwd('personal-root', '/Users/kevin/Works/personal'),
      withCwd('sub-inner', '/Users/kevin/Works/personal/projects/claude-view/src/claude-view/pkg'),
      withCwd('far', '/proj/other'),
    ];
    const byPersonalRoot = filterAgents(sessions, new Set([anchors[0]]), anchors);
    expect(byPersonalRoot.map((a) => a.id).sort()).toEqual(['middle', 'personal-root', 'sub-inner']);
    const bySub = filterAgents(sessions, new Set([anchors[1]]), anchors);
    expect(bySub.map((a) => a.id).sort()).toEqual(['middle', 'personal-root', 'sub-inner']);
  });

  it('does not leak across sibling projects (r23 族语义下防误扩)', () => {
    const g = filterAgents(
      [withCwd('dxq-wt', '/proj/dxq/.worktree/staging'), withCwd('one-two', '/proj/one-two/x')],
      new Set(['/proj/one', '/proj/dxq/.worktree/plan']),
      ['/proj/one', '/proj/dxq'],
    );
    // dxq-wt 与勾选的 dxq/plan 同族（dxq）→ 显示；one-two 与 /proj/one 仅同形前缀，
    // '/proj/one-two/x'.startsWith('/proj/one/') 为 false → 不误扩
    expect(g.map((a) => a.id)).toEqual(['dxq-wt']);
  });

  it('keeps every checked-root session when all roots are checked (原全勾语义)', () => {
    const all = new Set(['/proj/one', '/proj/two']);
    expect(filterAgents(agents, all, ['/proj/one', '/proj/two']).map((a) => a.id)).toEqual([
      'root',
      'wt',
      'sub',
      'other',
    ]);
  });
});

// —— autoTunnelStart（r80 自激风暴修复）：退避序列 / 上限 / 手动绕过 / 恢复复位 / 在途去重 ——

function deferred<T>(): { p: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void } {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const p = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { p, resolve, reject };
}

describe('autoTunnelStart（r80 自激风暴修复）', () => {
  const ID = 'inst-backoff';
  beforeEach(() => {
    resetTunnelBackoff(ID); // 模块级 Map 状态按用例隔离
    vi.useFakeTimers();
  });
  afterEach(() => {
    resetTunnelBackoff(ID);
    vi.useRealTimers();
  });

  /** 拉起可控行为：记录调用、返回未落定 deferred，reject 后排空微任务 */
  function makeStart() {
    const starts: ReturnType<typeof deferred>[] = [];
    const start = vi.fn(() => {
      const d = deferred<unknown>();
      starts.push(d);
      return d.p;
    });
    const failLast = async () => {
      starts[starts.length - 1].reject(new Error('tunnel down'));
      await vi.advanceTimersByTimeAsync(0);
    };
    return { start, starts, failLast };
  }

  it('连续失败按 2s/4s/8s 指数退避', async () => {
    const { start, failLast } = makeStart();
    // 第 1 次立即发起；失败后 2s 内不放行
    expect(autoTunnelStart(ID, start, () => {})).toBe(true);
    await failLast();
    expect(start).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1_999);
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    vi.advanceTimersByTime(1);
    expect(autoTunnelStart(ID, start, () => {})).toBe(true);
    // 第 2 次失败后 4s 内不放行
    await failLast();
    vi.advanceTimersByTime(3_999);
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    vi.advanceTimersByTime(1);
    expect(autoTunnelStart(ID, start, () => {})).toBe(true);
    // 第 3 次失败后 8s 内不放行
    await failLast();
    vi.advanceTimersByTime(7_999);
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    vi.advanceTimersByTime(1);
    expect(autoTunnelStart(ID, start, () => {})).toBe(true);
  });

  it('连续 5 次失败后停止自动发起（上限），不再风暴', async () => {
    const { start, failLast } = makeStart();
    for (let i = 0; i < 5; i++) {
      expect(autoTunnelStart(ID, start, () => {})).toBe(true);
      await failLast();
      vi.advanceTimersByTime(30_000); // 直接跳过退避窗口，验证触发的是次数上限而非退避
    }
    vi.advanceTimersByTime(600_000);
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    expect(start).toHaveBeenCalledTimes(5);
  });

  it('手动启动不受退避/上限限制（InstancesSection 直调 api.tunnelStart，不经本路径）', async () => {
    const { start, failLast } = makeStart();
    for (let i = 0; i < 5; i++) {
      expect(autoTunnelStart(ID, start, () => {})).toBe(true);
      await failLast();
      vi.advanceTimersByTime(30_000);
    }
    // 自动路径被上限拦截；手动路径 = 直接调用 api.tunnelStart（此处以直调 start 模拟），照常执行
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    const manual = vi.fn(() => Promise.resolve({ localPort: 58111 }));
    await expect(manual()).resolves.toEqual({ localPort: 58111 });
    expect(manual).toHaveBeenCalledOnce();
  });

  it('恢复后正常：agents 成功路径复位退避，自动拉起能力完整还原', async () => {
    const { start, failLast } = makeStart();
    for (let i = 0; i < 5; i++) {
      expect(autoTunnelStart(ID, start, () => {})).toBe(true);
      await failLast();
      vi.advanceTimersByTime(30_000);
    }
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    // queryFn 成功路径调用 resetTunnelBackoff → 下次故障可重新自动拉起
    resetTunnelBackoff(ID);
    expect(autoTunnelStart(ID, start, () => {})).toBe(true);
  });

  it('在途去重：拉起未落定不重复发起（react-query retry 重入 catch 不放大）', () => {
    const start = vi.fn(() => deferred<unknown>().p); // 永不落定
    expect(autoTunnelStart(ID, start, () => {})).toBe(true);
    expect(autoTunnelStart(ID, start, () => {})).toBe(false);
    expect(start).toHaveBeenCalledTimes(1);
  });

  it('拉起成功触发 onSuccess（invalidate instances）并清在途标', async () => {
    const onSuccess = vi.fn();
    let d!: ReturnType<typeof deferred>;
    expect(autoTunnelStart(ID, () => ((d = deferred()), d.p), onSuccess)).toBe(true);
    d.resolve({ localPort: 58111 });
    await vi.advanceTimersByTimeAsync(0);
    expect(onSuccess).toHaveBeenCalledOnce();
  });
});

// —— H253 请求风暴：慢而半通的隧道不再被「失败即 start + invalidate」滚成风暴 ——

describe('instanceSetKey（WS 不随引用抖动重建）', () => {
  const ctx = (id: string | null, baseUrl: string): InstanceCtx => ({
    id,
    name: id ?? '本机',
    mode: 'direct',
    baseUrl,
    config: null,
  });
  const list = [ctx(null, ''), ctx('h253', '/api/instances/h253/p')];

  it('同内容不同引用 → 同签名（清单重拉不触发 WS 拆建）', () => {
    expect(instanceSetKey(list)).toBe(instanceSetKey(list.map((c) => ({ ...c }))));
  });

  it('实例集真变了 → 签名变（WS 仍会正常拆建）', () => {
    const grown = [...list, ctx('h254', '/api/instances/h254/p')];
    expect(instanceSetKey(grown)).not.toBe(instanceSetKey(list));
    expect(instanceSetKey([ctx(null, ''), ctx('h253', '/api/instances/h253/p2')])).not.toBe(
      instanceSetKey(list),
    );
  });
});

describe('ensureTunnelRunning（慢 ≠ 离线）', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('隧道在跑就不自动 start（不 invalidate → 不重拉清单 → 不拆 WS）', async () => {
    const start = vi.fn(() => Promise.resolve({ localPort: 1 }));
    const ok = vi.fn();
    const fired = await ensureTunnelRunning(
      'inst-ensure-run',
      () => Promise.resolve({ running: true }),
      start,
      ok,
    );
    expect(fired).toBe(false);
    expect(start).not.toHaveBeenCalled();
    expect(ok).not.toHaveBeenCalled();
  });

  it('隧道没跑才自动拉起（H253 事故形态：隧道真挂了仍能复活）', async () => {
    const id = 'inst-ensure-down';
    const start = vi.fn(() => Promise.resolve({ localPort: 1 }));
    const ok = vi.fn();
    const fired = await ensureTunnelRunning(id, () => Promise.resolve({ running: false }), start, ok);
    await vi.advanceTimersByTimeAsync(0);
    expect(fired).toBe(true);
    expect(start).toHaveBeenCalledWith(id);
    expect(ok).toHaveBeenCalledOnce();
  });

  it('状态查询本身失败 → 保守当没跑，退回退避路径', async () => {
    const id = 'inst-ensure-unknown';
    const start = vi.fn(() => Promise.resolve({ localPort: 1 }));
    const fired = await ensureTunnelRunning(
      id,
      () => Promise.reject(new Error('hub down')),
      start,
      () => {},
    );
    expect(fired).toBe(true);
    expect(start).toHaveBeenCalledWith(id);
  });
});

describe('自动拉起冷却期（H253：退避不再被成功复位无限 re-arm）', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('拉起成功后进冷却：resetTunnelBackoff 也打不穿，满期才恢复', async () => {
    const id = 'inst-cooldown-ok';
    const start = vi.fn(() => Promise.resolve({ localPort: 1 }));
    const ok = vi.fn();
    expect(autoTunnelStart(id, start, ok)).toBe(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(ok).toHaveBeenCalledOnce();
    expect(start).toHaveBeenCalledTimes(1);

    // 成功路径复位退避计数 + 上限（H253 的自激入口），冷却必须仍在
    resetTunnelBackoff(id);
    vi.advanceTimersByTime(1);
    expect(autoTunnelStart(id, start, ok)).toBe(false);
    expect(start).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(TUNNEL_AUTO_START_COOLDOWN_MS);
    expect(autoTunnelStart(id, start, ok)).toBe(true);
    expect(start).toHaveBeenCalledTimes(2);
  });

  it('拉起失败不记冷却（连续失败仍按 r80 的退避序列推进，不被冷却拦死）', async () => {
    const id = 'inst-cooldown-fail';
    const start = vi.fn(() => Promise.reject(new Error('tunnel down')));
    expect(autoTunnelStart(id, start, () => {})).toBe(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(start).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1_999);
    expect(autoTunnelStart(id, start, () => {})).toBe(false); // 退避窗内
    vi.advanceTimersByTime(1);
    expect(autoTunnelStart(id, start, () => {})).toBe(true); // 退避到期放行（冷却未生效）
    expect(start).toHaveBeenCalledTimes(2);
  });
});

describe('每实例在途上限（H253 隔离：远端慢不许吃掉本机的连接槽）', () => {
  const list = [fake('a', 'working')];
  /** 镜像真实 listAgents：认 signal，abort 即 reject（fetch 的行为） */
  const stubApi = (impl: (signal?: AbortSignal) => Promise<AgentSummary[]>) =>
    ({
      listAgents: (_all: boolean, init?: RequestInit) => impl(init?.signal ?? undefined),
    }) as unknown as Parameters<typeof fetchAgentsWithDeadline>[0];

  it('同一实例并发失效只发一个在途请求（复用同一 promise）', async () => {
    const calls: string[] = [];
    let resolve!: (v: AgentSummary[]) => void;
    const run = (): Promise<AgentSummary[]> => {
      calls.push('h253');
      return new Promise((r) => (resolve = r));
    };
    const a = singleFlight('sf-1', run);
    const b = singleFlight('sf-1', run);
    const c = singleFlight('sf-1', run);
    expect(calls).toEqual(['h253']); // 并发三次只发出一条
    expect(a).toBe(b);
    expect(b).toBe(c);
    resolve(list);
    await expect(Promise.all([a, b, c])).resolves.toEqual([list, list, list]);

    // 落地后再失效 → 才发第二条（槽位已归还）
    void singleFlight('sf-1', run);
    expect(calls).toEqual(['h253', 'h253']);
  });

  it('不同实例互不复用（各自只占 1 条槽位）', () => {
    const calls: string[] = [];
    const run = (id: string) => () => {
      calls.push(id);
      return new Promise<AgentSummary[]>(() => {});
    };
    singleFlight('sf-h253', run('h253'));
    singleFlight('sf-h254', run('h254'));
    expect(calls).toEqual(['h253', 'h254']);
  });

  it('远端请求超 deadline 即中止（不长期霸占连接），本机不设限', async () => {
    vi.useFakeTimers();
    try {
      const hangUntilAbort = (signal?: AbortSignal) =>
        new Promise<AgentSummary[]>((_, rej) => {
          signal?.addEventListener('abort', () => rej(new Error('aborted')));
        });
      const remoteP = fetchAgentsWithDeadline(stubApi(hangUntilAbort), 'h253').catch(
        (e: unknown) => e,
      );
      await vi.advanceTimersByTimeAsync(10_000);
      await expect(remoteP).resolves.toBeInstanceOf(Error); // 10s deadline 到了就中止

      const local = fetchAgentsWithDeadline(stubApi(hangUntilAbort), null).catch(() => 'settled');
      let settled = false;
      void local.then(() => (settled = true));
      await vi.advanceTimersByTimeAsync(60_000);
      // 本机仍在飞（没有 deadline：慢是 hub 自身问题，不该被判死）
      expect(settled).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });
});
