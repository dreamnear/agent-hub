// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import TaskListBar from './TaskListBar';
import type { TaskItem } from '../hooks/combineToolCalls';

const tasks: TaskItem[] = [
  { taskId: '1', subject: '资料收集', status: 'completed' },
  { taskId: '2', subject: '写报告', status: 'in_progress' },
  { taskId: '3', subject: 'review', status: 'pending' },
];

describe('TaskListBar', () => {
  it('loads its stylesheet to constrain the bar width and make the summary clickable', () => {
    const { container } = render(<TaskListBar tasks={tasks} />);
    const bar = container.querySelector('.task-bar') as HTMLElement;
    expect(bar).not.toBeNull();
    expect(getComputedStyle(bar).maxWidth).toBe('748px');
    expect(getComputedStyle(bar.querySelector('summary')!).cursor).toBe('pointer');
  });

  it('renders collapsed by default with first-subject summary line and total count', () => {
    const { container } = render(<TaskListBar tasks={tasks} />);
    const bar = container.querySelector('details.task-list-card.task-bar') as HTMLDetailsElement;
    expect(bar).not.toBeNull();
    expect(bar.open).toBe(false);
    // 摘要行：图标 + title + 排序后第一项（in_progress 优先）标题 + 总数括号
    expect(container.querySelector('.task-bar-summary-text')?.textContent).toBe(
      '📋 任务清单：写报告...(3)',
    );
  });

  it('sorts in_progress → pending → completed in the expanded list', () => {
    const { container } = render(<TaskListBar tasks={tasks} />);
    (container.querySelector('details.task-bar') as HTMLDetailsElement).open = true;
    // 批3：清单行换 BUI TaskRows——行 = aria-expanded 的 toggle button，label 在行内
    const rowLabels = [...container.querySelectorAll('[data-bui] button[aria-expanded]')]
      .filter((b) => b.getAttribute('aria-expanded') !== null && b.closest('[data-bui]'))
      .map((b) => b.textContent ?? '');
    expect(rowLabels[0]).toContain('写报告');
    expect(rowLabels[1]).toContain('review');
    expect(rowLabels[2]).toContain('资料收集');
  });

  it('keeps stable order within the same status (appearance order)', () => {
    const many: TaskItem[] = [
      { taskId: '1', subject: '先建', status: 'pending' },
      { taskId: '2', subject: '后建', status: 'pending' },
      { taskId: '3', subject: '进行', status: 'in_progress' },
    ];
    const { container } = render(<TaskListBar tasks={many} />);
    (container.querySelector('details.task-bar') as HTMLDetailsElement).open = true;
    const rowLabels = [...container.querySelectorAll('[data-bui] button[aria-expanded]')].map(
      (b) => b.textContent ?? '',
    );
    expect(rowLabels[0]).toContain('进行');
    expect(rowLabels[1]).toContain('先建');
    expect(rowLabels[2]).toContain('后建');
  });
});
