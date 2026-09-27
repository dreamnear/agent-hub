// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import type { AgentSummary } from '../types';
import { filterAgents, filterFinishedSubagents, filterInteractive, groupAgents } from './useAgents';

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
      [withCwd('sub', '/Users/demo/Works/personal/projects/claude-view')],
      new Set(['/Users/demo/Works/personal']),
      ['/Users/demo/Works/personal'],
    );
    expect(g.map((a) => a.id)).toEqual(['sub']);
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
      '/Users/demo/Works/personal',
      '/Users/demo/Works/personal/projects/claude-view/src/claude-view',
    ];
    const sessions = [
      withCwd('middle', '/Users/demo/Works/personal/projects/claude-view'),
      withCwd('personal-root', '/Users/demo/Works/personal'),
      withCwd('sub-inner', '/Users/demo/Works/personal/projects/claude-view/src/claude-view/pkg'),
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
