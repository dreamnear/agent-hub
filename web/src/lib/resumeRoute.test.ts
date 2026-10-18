// 路由裁决四分支锚定（agent-hub-resume-inplace #21）：
// 运行中复用 / 死+idle 注入 / 死+忙回退 / 无活 agent 回退，外加驱动与同工程守卫。

import { describe, expect, it } from 'vitest';
import { isInjectableSessionId, resolveResumeRoute, type ResumeRoute } from './resumeRoute';
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
    // 选中行须在活跃清单（活判先行），驱动守卫在活判之后
    const ompSelected = agent({ id: 'agt-omp', driver: 'omp', sessionId: null });
    expect(route({ activeAgents: [ompSelected], selected: ompSelected })).toEqual({
      kind: 'spawn',
      reason: 'driver',
    });
    const acpSelected = agent({ id: 'agt-acp', driver: 'acp', sessionId: null });
    expect(route({ activeAgents: [acpSelected], selected: acpSelected })).toEqual({
      kind: 'spawn',
      reason: 'driver',
    });
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

  it('needs_input（blocked = AskUserQuestion/权限确认挂起中）→ 不可注入，spawn 回退 busy', () => {
    // blocked 是「agent 提问/权限确认挂起等待用户」的态（agent-hub-p4 实证），
    // 此时注入 /resume 会经 /message 写进挂起框替用户应答——round1 审查改判
    const waiting = agent({ id: 'agt-sel', group: 'needs_input', sessionId: 'live-s4' });
    expect(route({ selected: waiting })).toEqual({ kind: 'spawn', reason: 'busy' });
  });

  it('选中行不在活跃清单（historySel 快照死行）→ spawn 回退 no-agent（不触发静默 respawn）', () => {
    const deadSnapshot = agent({ id: 'agt-dead', sessionId: 'live-s5' });
    expect(route({ selected: deadSnapshot })).toEqual({ kind: 'spawn', reason: 'no-agent' });
  });

  it('同 id 不同实例的选中行不算活（复合键实例维）→ spawn 回退 no-agent', () => {
    const remoteDead = agent({ id: 'agt-sel', instanceId: 'remote-1', sessionId: 'live-s6' });
    // 活跃清单里是本机 agt-sel，selected 声称 remote-1 → 维不匹配 = 死
    expect(route({ selected: remoteDead })).toEqual({ kind: 'spawn', reason: 'no-agent' });
  });

  it('活 TUI 的 done（group other 兜底）不算忙 → 可注入', () => {
    // claude 驱动 done+alive 映射 Other（drivers/claude/mod.rs），即「停在提示符」
    expect(idleSelected.group).toBe('other');
    expect(route({})).toEqual({ kind: 'inplace', agent: idleSelected });
  });
});

describe('isInjectableSessionId（注入白名单，round1 审查 MINOR 收紧）', () => {
  it('合法 UUID 放行（大小写不敏感）', () => {
    expect(isInjectableSessionId('a1b2c3d4-e5f6-7890-abcd-ef0123456789')).toBe(true);
    expect(isInjectableSessionId('A1B2C3D4-E5F6-7890-ABCD-EF0123456789')).toBe(true);
  });

  it('非 UUID / 命令分隔向量拒绝（换行、路径分量、空串）', () => {
    expect(isInjectableSessionId('abc')).toBe(false);
    expect(isInjectableSessionId('')).toBe(false);
    expect(isInjectableSessionId('/resume abc\nwhoami')).toBe(false);
    expect(isInjectableSessionId('../../etc/passwd')).toBe(false);
    expect(isInjectableSessionId('a1b2c3d4e5f67890abcdefgh0123456789')).toBe(false); // 缺连字符
  });
});
