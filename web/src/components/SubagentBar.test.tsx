// @vitest-environment happy-dom
import { describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/react';
import SubagentBar from './SubagentBar';
import type { SubagentEntry } from '../types';

const list: SubagentEntry[] = [
  {
    agentId: 'dr-planner-f9dcf57d',
    name: 'dr-planner',
    agentType: 'dr-planner',
    description: 'P1 计划转 tasks.md',
    model: 'opus',
    status: 'completed',
    startedAt: null,
    lastActiveAt: null,
  },
  {
    agentId: 'ae857e95bdb0',
    name: 'ae857e95',
    agentType: 'subagent',
    description: null,
    model: null,
    status: 'active',
    startedAt: null,
    lastActiveAt: null,
  },
];

describe('SubagentBar', () => {
  it('renders collapsed by default with count summary (TaskListBar 同款形态)', () => {
    const { container } = render(
      <SubagentBar subagents={list} activeId={null} onSelect={() => {}} />,
    );
    const card = container.querySelector('details.subagent-card') as HTMLDetailsElement;
    expect(card).not.toBeNull();
    expect(card.open).toBe(false);
    expect(container.querySelector('.subagent-summary-text')?.textContent).toBe(
      '🤖 Subagent 2 · 1 active',
    );
    // 收起态不渲染不出 chip（details 原生折叠）
    expect(card.textContent).toContain('dr-planner');
  });

  it('omits the active suffix when nothing is running', () => {
    const done = list.map((s) => ({ ...s, status: 'completed' as const }));
    const { container } = render(
      <SubagentBar subagents={done} activeId={null} onSelect={() => {}} />,
    );
    expect(container.querySelector('.subagent-summary-text')?.textContent).toBe('🤖 Subagent 2');
  });

  it('renders nothing when there are no subagents', () => {
    const { container } = render(<SubagentBar subagents={[]} activeId={null} onSelect={() => {}} />);
    expect(container.querySelector('details.subagent-card')).toBeNull();
  });

  it('exposes chips on expand with status dots and reports selection', () => {
    const onSelect = vi.fn();
    const { container } = render(
      <SubagentBar subagents={list} activeId="ae857e95bdb0" onSelect={onSelect} />,
    );
    expect(container.querySelectorAll('.subagent-chip')).toHaveLength(2);
    expect(container.querySelector('.subagent-dot--active')).not.toBeNull();
    const active = container.querySelector('.subagent-chip--active') as HTMLButtonElement;
    expect(active.textContent).toContain('ae857e95');
    active.click();
    expect(onSelect).toHaveBeenCalledWith('ae857e95bdb0');
  });

  it('loads its stylesheet for summary interaction and chip shape (missing-import guard)', () => {
    // 防漏 import（同 TaskListBar 模式）：样式缺失即失败
    const { container } = render(
      <SubagentBar subagents={list} activeId={null} onSelect={() => {}} />,
    );
    const summary = container.querySelector('summary') as HTMLElement;
    expect(getComputedStyle(summary).cursor).toBe('pointer');
    const chip = container.querySelector('.subagent-chip') as HTMLElement;
    expect(getComputedStyle(chip).borderRadius).toBe('999px');
  });
});
