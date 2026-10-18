// 路由裁决四分支锚定（agent-hub-resume-inplace #21）：
// 运行中复用 / 死+idle 注入 / 死+忙回退 / 无活 agent 回退，外加驱动与同工程守卫。

import { describe, expect, it } from 'vitest';
import { resolveResumeRoute, type ResumeRoute } from './resumeRoute';
import type { AgentSummary } from '../types';

function agent(over: Partial<AgentSummary>): AgentSummary {
  return {
    driver: 'claude',
    id: 'agt-x',
    name: '测试行',
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

const entry = agent({ id: 'agt-entry', sessionId: null });
const idleSelected = agent({ id: 'agt-sel', cwd: '/repo/proj', group: 'other', sessionId: 'live-sess' });
const busySelected = agent({ id: 'agt-sel', cwd: '/repo/proj', group: 'working', sessionId: 'live-sess' });

function route(over: {
  sessionId?: string;
  entry?: AgentSummary;
  activeAgents?: AgentSummary[];
  selected?: AgentSummary | null;
}): ResumeRoute {
  return resolveResumeRoute({
    sessionId: over.sessionId ?? 'dead-sess',
    entry: over.entry ?? entry,
    activeAgents: over.activeAgents ?? [idleSelected],
    selected: over.selected === undefined ? idleSelected : over.selected,
  });
}

describe('resolveResumeRoute（agent-hub-resume-inplace #21）', () => {
  it('运行中：目标会话在活跃清单 → 复用现有行（路由 1）', () => {
    expect(route({ sessionId: 'live-sess' })).toEqual({ kind: 'reuse' });
  });

  it('运行中：入口行自己持有目标会话也复用（不区分谁持有）', () => {
    expect(route({ sessionId: 'live-sess', selected: null })).toEqual({ kind: 'reuse' });
  });

  it('死会话 + 选中行活着且 idle + 同工程 → 原地注入该选中行（路由 2）', () => {
    expect(route({})).toEqual({ kind: 'inplace', agent: idleSelected });
  });

  it('死会话 + 选中行忙（working）→ spawn 回退 busy（路由 3，busy 策略 b）', () => {
    expect(route({ selected: busySelected })).toEqual({ kind: 'spawn', reason: 'busy' });
  });

  it('死会话 + 无选中 → spawn 回退 no-agent（路由 4）', () => {
    expect(route({ selected: null })).toEqual({ kind: 'spawn', reason: 'no-agent' });
  });

  it('死会话 + 选中行非 claude 驱动（omp/acp 等）→ spawn 回退 driver', () => {
    const ompSelected = agent({ id: 'agt-omp', driver: 'omp', sessionId: null });
    expect(route({ selected: ompSelected })).toEqual({ kind: 'spawn', reason: 'driver' });
    const acpSelected = agent({ id: 'agt-acp', driver: 'acp', sessionId: null });
    expect(route({ selected: acpSelected })).toEqual({ kind: 'spawn', reason: 'driver' });
  });

  it('入口行非 claude 驱动（omp 清单弹窗）→ 不注入，spawn 回退 driver', () => {
    const ompEntry = agent({ id: 'agt-entry', driver: 'omp', sessionId: null });
    expect(route({ entry: ompEntry })).toEqual({ kind: 'spawn', reason: 'driver' });
  });

  it('选中行与入口行不同工程（cwd 不一致）→ spawn 回退 cwd（防污染原转录）', () => {
    const elsewhere = agent({ id: 'agt-sel', cwd: '/repo/other', sessionId: 'live-s2' });
    expect(route({ selected: elsewhere })).toEqual({ kind: 'spawn', reason: 'cwd' });
  });

  it('选中行 cwd 缺失 → 视为不可注入，spawn 回退 cwd', () => {
    const noCwd = agent({ id: 'agt-sel', cwd: null, sessionId: 'live-s3' });
    expect(route({ selected: noCwd })).toEqual({ kind: 'spawn', reason: 'cwd' });
  });

  it('needs_input（blocked 等输入）不算忙 → 可注入（TUI 停在提示符等输入）', () => {
    const waiting = agent({ id: 'agt-sel', group: 'needs_input', sessionId: 'live-s4' });
    expect(route({ selected: waiting })).toEqual({ kind: 'inplace', agent: waiting });
  });

  it('活 TUI 的 done（group other 兜底）不算忙 → 可注入', () => {
    // claude 驱动 done+alive 映射 Other（drivers/claude/mod.rs），即「停在提示符」
    expect(idleSelected.group).toBe('other');
    expect(route({})).toEqual({ kind: 'inplace', agent: idleSelected });
  });
});
