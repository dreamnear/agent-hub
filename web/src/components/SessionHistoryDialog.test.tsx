// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AgentSummary, CliSessionEntry } from '../types';
import type { Api } from '../api';
import SessionHistoryDialog from './SessionHistoryDialog';

function mkAgent(overrides: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: 'agent-1',
    driver: 'claude',
    group: 'other',
    name: '测试会话',
    cwd: '/repo/proj',
    kind: 'background',
    rawState: 'done',
    detail: null,
    tokens: null,
    context_tokens: null,
    startedAt: 1790831543,
    sessionId: 'sess-current',
    instanceId: null,
    ...overrides,
  };
}

function entry(overrides: Partial<CliSessionEntry> = {}): CliSessionEntry {
  return {
    sessionId: 'aaaa-old',
    mtimeSecs: 1_000_000,
    sizeBytes: 2048,
    branch: 'main',
    cwd: '/repo/proj',
    summary: '旧会话摘要',
    ...overrides,
  };
}

function mkApi(
  cliSessions: Api['cliSessions'],
  resumeSession?: Api['resumeSession'],
): Api {
  return {
    cliSessions,
    resumeSession:
      resumeSession ?? vi.fn().mockResolvedValue({ status: 'resumed', id: 'bg-new' }),
  } as unknown as Api;
}

function renderDialog(
  api: Api,
  onSelect: (a: AgentSummary) => void = vi.fn(),
  onClose: () => void = vi.fn(),
  agents: AgentSummary[] = [],
  agent: AgentSummary = mkAgent(),
): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <SessionHistoryDialog
        agent={agent}
        agents={agents}
        api={api}
        onSelect={onSelect}
        onClose={onClose}
      />
    </QueryClientProvider>,
  );
}

function itemButtons(): HTMLElement[] {
  return screen
    .queryAllByRole('button')
    .filter((b) => b.className === 'sesshist-item');
}

afterEach(cleanup);

describe('SessionHistoryDialog（agent-hub-session-history）', () => {
  it('列表渲染：条目数与数据一致，行内字段（摘要/分支/体积/相对时间）齐全', async () => {
    const api = mkApi(
      vi.fn().mockResolvedValue([
        entry(),
        entry({
          sessionId: 'bbbb-new',
          summary: '修复登录页的会话',
          branch: 'feat/login',
          sizeBytes: 3 * 1024 * 1024,
        }),
      ]),
    );
    renderDialog(api);
    await waitFor(() => expect(itemButtons().length).toBe(2));
    expect(screen.getByText('旧会话摘要')).toBeTruthy();
    expect(screen.getByText('修复登录页的会话')).toBeTruthy();
    expect(screen.getByText('main')).toBeTruthy();
    expect(screen.getByText('feat/login')).toBeTruthy();
    expect(screen.getByText('2KB')).toBeTruthy();
    expect(screen.getByText('3.0MB')).toBeTruthy();
  });

  it('worktree 会话显示 cwd 尾段（与 agent cwd 不同才显示）', async () => {
    const api = mkApi(
      vi.fn().mockResolvedValue([entry({ cwd: '/repo/.worktree/dec-staging' })]),
    );
    renderDialog(api);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    expect(screen.getByText('dec-staging')).toBeTruthy();
  });

  it('搜索过滤：输入即筛（摘要命中），无命中显示过滤空态', async () => {
    const api = mkApi(
      vi.fn().mockResolvedValue([entry(), entry({ sessionId: 'bbbb-new', summary: '修复登录页' })]),
    );
    renderDialog(api);
    await waitFor(() => expect(itemButtons().length).toBe(2));
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '登录页' } });
    expect(itemButtons().length).toBe(1);
    expect(screen.getByText('修复登录页')).toBeTruthy();
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '不存在的词' } });
    expect(itemButtons().length).toBe(0);
    expect(screen.getByText('没有匹配的会话')).toBeTruthy();
  });

  it('空态：工程无历史会话不报错', async () => {
    const api = mkApi(vi.fn().mockResolvedValue([]));
    renderDialog(api);
    await waitFor(() => expect(screen.getByText('该工程暂无历史会话')).toBeTruthy());
  });

  it('错误态：取数失败显示错误 + 重试可用', async () => {
    const cliSessions = vi
      .fn()
      .mockRejectedValueOnce(new Error('目录不可读'))
      .mockResolvedValueOnce([entry()]);
    const api = mkApi(cliSessions);
    renderDialog(api);
    await waitFor(() => expect(screen.getByText(/目录不可读/)).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    await waitFor(() => expect(itemButtons().length).toBe(1));
  });

  it('恢复（resumed）：onSelect 回抛快照（id 切新 bg 条目、sessionId 原样），随后关闭', async () => {
    const resumeSession = vi.fn().mockResolvedValue({ status: 'resumed', id: 'bg-new' });
    const api = mkApi(vi.fn().mockResolvedValue([entry()]), resumeSession);
    const onSelect = vi.fn();
    const onClose = vi.fn();
    renderDialog(api, onSelect, onClose);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    fireEvent.click(itemButtons()[0]);
    await waitFor(() => expect(onSelect).toHaveBeenCalledTimes(1));
    expect(onSelect).toHaveBeenCalledWith(
      expect.objectContaining({ id: 'bg-new', sessionId: 'aaaa-old', driver: 'claude' }),
    );
    expect(onClose).toHaveBeenCalledTimes(1);
    // 端点寻址：入口 agent 的 id + 目标 sessionId
    expect(resumeSession).toHaveBeenCalledWith('agent-1', 'aaaa-old', 'claude');
  });

  it('恢复（active）：复用既有实例条目，不产生重复实例', async () => {
    const existing = mkAgent({ id: 'bg-live', sessionId: 'aaaa-old', group: 'working' });
    const resumeSession = vi.fn().mockResolvedValue({ status: 'active', agent: existing });
    const api = mkApi(vi.fn().mockResolvedValue([entry()]), resumeSession);
    const onSelect = vi.fn();
    renderDialog(api, onSelect);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    fireEvent.click(itemButtons()[0]);
    await waitFor(() => expect(onSelect).toHaveBeenCalledTimes(1));
    expect(onSelect).toHaveBeenCalledWith(
      expect.objectContaining({ id: 'bg-live', sessionId: 'aaaa-old' }),
    );
  });

  it('运行中徽标：活跃清单 sessionId 命中即显示（恢复动作仍可用，服务端复用兜底）', async () => {
    const api = mkApi(vi.fn().mockResolvedValue([entry()]));
    const live = mkAgent({ id: 'bg-live', sessionId: 'aaaa-old' });
    renderDialog(api, vi.fn(), vi.fn(), [live]);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    expect(screen.getByText('运行中')).toBeTruthy();
  });

  it('当前会话徽标：agent.sessionId 命中显示「当前会话」而非「运行中」', async () => {
    const api = mkApi(vi.fn().mockResolvedValue([entry({ sessionId: 'sess-current' })]));
    renderDialog(api);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    expect(screen.getByText('当前会话')).toBeTruthy();
    expect(screen.queryByText('运行中')).toBeNull();
  });

  it('恢复失败：错误内联显示、弹窗不关', async () => {
    const resumeSession = vi.fn().mockRejectedValue(new Error('claude resume 失败'));
    const api = mkApi(vi.fn().mockResolvedValue([entry()]), resumeSession);
    const onSelect = vi.fn();
    const onClose = vi.fn();
    renderDialog(api, onSelect, onClose);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    fireEvent.click(itemButtons()[0]);
    await waitFor(() => expect(screen.getByText(/claude resume 失败/)).toBeTruthy());
    expect(onSelect).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });
});
