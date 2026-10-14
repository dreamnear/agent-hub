// @vitest-environment happy-dom
// 设置中心「定时任务」段：列表渲染/动作域切换/必填校验/启停/手动触发/删除二次确认/会话跳转。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';
import TasksSection from './TasksSection';
import type { TaskRun, TaskSpecWithAction } from '../../types';

const listTasksMock = vi.fn();
const createTaskMock = vi.fn();
const updateTaskMock = vi.fn();
const deleteTaskMock = vi.fn();
const runTaskMock = vi.fn();
const taskRunsMock = vi.fn();
const schedulerStatusMock = vi.fn();

vi.mock('../../api', () => ({
  api: {
    listTasks: () => listTasksMock(),
    createTask: (t: TaskSpecWithAction) => createTaskMock(t),
    updateTask: (id: string, t: TaskSpecWithAction) => updateTaskMock(id, t),
    deleteTask: (id: string) => deleteTaskMock(id),
    runTask: (id: string) => runTaskMock(id),
    taskRuns: (id: string) => taskRunsMock(id),
    schedulerStatus: () => schedulerStatusMock(),
  },
}));

const SPAWN_TASK: TaskSpecWithAction = {
  id: 'task-1',
  name: '每日日报',
  cron: '0 30 9 * * *',
  enabled: true,
  action: 'spawn',
  driver: 'claude',
  cwd: '/repo/main',
  name_template: '日报 {{date}}',
  prompt_template: '写日报',
  next_run_at: '2026-10-05T09:30:00+08:00',
};

const RUN: TaskRun = {
  fired_at: '2026-10-05T09:30:00+08:00',
  status: 'ok',
  session_id: 'agent-42',
  duration_ms: 120,
};

const Panel = ({ onOpenSession }: { onOpenSession?: (id: string) => void }): ReactElement => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return (
    <QueryClientProvider client={client}>
      <TasksSection onOpenSession={onOpenSession} />
    </QueryClientProvider>
  );
};

beforeEach(() => {
  listTasksMock.mockReset().mockResolvedValue([SPAWN_TASK]);
  createTaskMock.mockReset().mockResolvedValue(SPAWN_TASK);
  updateTaskMock.mockReset().mockResolvedValue(SPAWN_TASK);
  deleteTaskMock.mockReset().mockResolvedValue(undefined);
  runTaskMock.mockReset().mockResolvedValue(undefined);
  taskRunsMock.mockReset().mockResolvedValue([RUN]);
  schedulerStatusMock.mockReset().mockResolvedValue({ primary: true });
});

afterEach(cleanup);

const q = (container: HTMLElement, sel: string): HTMLElement => {
  const el = container.querySelector(sel);
  if (!el) throw new Error(`未渲染: ${sel}`);
  return el as HTMLElement;
};

describe('TasksSection（设置中心定时任务段）', () => {
  it('渲染任务列表：名称/cron/动作摘要/下次触发', async () => {
    const { container } = render(<Panel />);
    await waitFor(() => expect(container.textContent).toContain('每日日报'));
    expect(container.textContent).toContain('0 30 9 * * *');
    expect(container.textContent).toContain('/repo/main');
    expect(container.textContent).toContain('09:30');
  });

  it('assistant-cron C4：助手来源任务渲染「助手」徽标，手动任务无徽标', async () => {
    const assistantTask: TaskSpecWithAction = {
      ...SPAWN_TASK,
      id: 'task-asst1',
      name: '每日晨报',
      source: 'assistant',
    };
    listTasksMock.mockResolvedValue([SPAWN_TASK, assistantTask]);
    const { container } = render(<Panel />);
    await waitFor(() => expect(container.textContent).toContain('每日晨报'));
    // 助手来源 → 徽标；手动来源 → 无徽标
    expect(q(container, '[data-testid="task-source-task-asst1"]').textContent).toBe('助手');
    expect(container.querySelector('[data-testid="task-source-task-1"]')).toBeNull();
  });

  it('新建 spawn：动作域切换 + 必填校验 + 保存时模板字段透传', async () => {
    listTasksMock.mockResolvedValue([]);
    const { container } = render(<Panel />);
    fireEvent.click(q(container, '.task-new'));
    const form = q(container, '.task-form');
    // 默认 spawn 域：cwd/prompt 可见，inject 域隐藏
    expect(form.textContent).toContain('工作目录');
    expect(form.textContent).not.toContain('注入文本');
    // 切 inject：域互换
    fireEvent.change(q(container, '.task-form select'), {
      target: { value: 'inject' },
    });
    expect(form.textContent).not.toContain('工作目录');
    expect(form.textContent).toContain('注入文本');

    // 必填校验：全空保存 → 错误提示且不请求
    fireEvent.click([...container.querySelectorAll('button')].find((b) => b.textContent === '保存')!);
    expect(container.textContent).toContain('请填写');
    expect(createTaskMock).not.toHaveBeenCalled();

    // 填齐再保存 → createTask 收到渲染后的字段
    fireEvent.change(container.querySelector('.task-form input[placeholder*="每日"]') as HTMLElement, {
      target: { value: '注入任务' },
    });
    fireEvent.change(container.querySelector('.task-form input[placeholder="0 30 9 * * *"]') as HTMLElement, {
      target: { value: '0 0 12 * * *' },
    });
    const inputs = [...container.querySelectorAll('.task-form input')];
    fireEvent.change(inputs[inputs.length - 1], { target: { value: 'sess-9' } });
    fireEvent.change(container.querySelector('.task-form textarea') as HTMLElement, {
      target: { value: '你好 {{time}}' },
    });
    fireEvent.click([...container.querySelectorAll('button')].find((b) => b.textContent === '保存')!);
    await waitFor(() => expect(createTaskMock).toHaveBeenCalledOnce());
    const body = createTaskMock.mock.calls[0]?.[0] as TaskSpecWithAction;
    expect(body).toMatchObject({
      action: 'inject',
      driver: 'claude',
      session_id: 'sess-9',
      text: '你好 {{time}}',
      name: '注入任务',
      cron: '0 0 12 * * *',
    });
  });

  it('启停开关走 updateTask；手动触发走 runTask；删除需二次确认', async () => {
    const { container } = render(<Panel />);
    await waitFor(() => expect(container.querySelector('.task-item')).not.toBeNull());
    const checkbox = container.querySelector('.task-switch input') as HTMLInputElement;
    fireEvent.click(checkbox);
    await waitFor(() => expect(updateTaskMock).toHaveBeenCalledWith('task-1', expect.objectContaining({ enabled: false })));

    fireEvent.click([...container.querySelectorAll('button')].find((b) => b.textContent === '立即触发')!);
    await waitFor(() => expect(runTaskMock).toHaveBeenCalledWith('task-1'));

    // 删除：第一次只二次确认，第二次才删
    const delBtn = [...container.querySelectorAll('button')].find((b) => b.textContent === '删除')!;
    fireEvent.click(delBtn);
    expect(deleteTaskMock).not.toHaveBeenCalled();
    fireEvent.click([...container.querySelectorAll('button')].find((b) => b.textContent === '确认删除？')!);
    await waitFor(() => expect(deleteTaskMock).toHaveBeenCalledWith('task-1'));
  });

  it('展开最近触发：Run 记录渲染，sessionId 可点跳转该会话', async () => {
    const onOpenSession = vi.fn();
    const { container } = render(<Panel onOpenSession={onOpenSession} />);
    await waitFor(() => expect(container.querySelector('.task-item')).not.toBeNull());
    fireEvent.click(q(container, '.task-item-actions .task-btn'));
    await waitFor(() => expect(container.querySelector('.task-run')).not.toBeNull());
    expect(container.textContent).toContain('成功');
    expect(container.textContent).toContain('120ms');
    fireEvent.click(q(container, '.task-run-session'));
    expect(onOpenSession).toHaveBeenCalledWith('agent-42');
  });
});

describe('调度权徽标（agent-hub-schedule-lock S2）', () => {
  it('primary=true：渲染「调度中」on 徽标', async () => {
    schedulerStatusMock.mockResolvedValue({ primary: true });
    const { container } = render(<Panel />);
    await waitFor(() => {
      const badge = container.querySelector('.task-sched-badge');
      expect(badge).not.toBeNull();
      expect(badge?.className).toContain('task-sched-badge--on');
      expect(badge?.textContent).toContain('调度中');
    });
  });

  it('primary=false：渲染「调度禁用 · 另一实例持锁」off 徽标', async () => {
    schedulerStatusMock.mockResolvedValue({ primary: false });
    const { container } = render(<Panel />);
    await waitFor(() => {
      const badge = container.querySelector('.task-sched-badge');
      expect(badge).not.toBeNull();
      expect(badge?.className).toContain('task-sched-badge--off');
      expect(badge?.textContent).toContain('调度禁用');
    });
  });
});
