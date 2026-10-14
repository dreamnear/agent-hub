// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import StartDialog from './StartDialog';

const startAgentMock = vi.fn();
const createAcpSessionMock = vi.fn();
vi.mock('../api', () => ({
  api: {
    listProjects: () => Promise.resolve(['/repo/a', '/repo/b']),
    listDrivers: () => Promise.resolve(['claude', 'acp', 'dsh']),
    listAcpAgents: () =>
      Promise.resolve({
        agents: [{ name: 'omp', command: 'omp', args: ['acp'], cwd: null, model: null }],
      }),
    // cwd 选择器（CwdField）事实源：主仓 + worktree
    projectTree: () =>
      Promise.resolve([
        {
          main: { path: '/repo/a', name: 'a', isMain: true, isGit: true, branch: 'main', head: null },
          worktrees: [
            { path: '/repo/b-wt', name: 'b-wt', isMain: false, isGit: true, branch: 'wt/1', head: null },
          ],
        },
      ]),
    startAgent: (body: unknown, driver?: string) => startAgentMock(body, driver),
    createAcpSession: (body: unknown) => createAcpSessionMock(body),
  },
}));

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const renderDialog = () => render(<StartDialog onClose={vi.fn()} />, { wrapper });

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('StartDialog driver 切换（批2 任务7）', () => {
  it('默认 Claude Code 表单：prompt 必填，启动走 startAgent', async () => {
    startAgentMock.mockResolvedValue({ id: 'x' });
    renderDialog();
    // acp 专属字段不出现
    expect(screen.queryByLabelText(/ACP Agent/)).toBeNull();
    // cwd 由 listProjects 异步回填，回填后才可提交
    await waitFor(() => expect((screen.getByLabelText(/工程 \(cwd\)/) as HTMLSelectElement).value).toBe('/repo/a'));
    fireEvent.change(screen.getByLabelText(/指令 Prompt/), {
      target: { value: 'do something' },
    });
    fireEvent.click(screen.getByRole('button', { name: '启动' }));
    await waitFor(() => expect(startAgentMock).toHaveBeenCalledTimes(1));
    expect(createAcpSessionMock).not.toHaveBeenCalled();
  });

  it('切 ACP：agent/model 字段出现、prompt 消失，启动走 createAcpSession 并关窗', async () => {
    createAcpSessionMock.mockResolvedValue({ sessionId: 's1' });
    const onClose = vi.fn();
    render(<StartDialog onClose={onClose} />, { wrapper });
    // tabs 由 /api/drivers 异步回填后才渲染
    await waitFor(() => expect(screen.getByRole('tab', { name: 'ACP（omp）' })).toBeTruthy());
    fireEvent.click(screen.getByRole('tab', { name: 'ACP（omp）' }));
    // agent 下拉来自 /api/acp/agents，默认选首项
    await waitFor(() =>
      expect((screen.getByLabelText(/ACP Agent/) as HTMLSelectElement).value).toBe('omp'),
    );
    expect(screen.queryByLabelText(/指令 Prompt/)).toBeNull();
    fireEvent.change(screen.getByLabelText('Model (optional, blank = agent default)'), {
      target: { value: 'claude-sonnet' },
    });
    fireEvent.click(screen.getByRole('button', { name: '启动' }));
    await waitFor(() =>
      expect(createAcpSessionMock).toHaveBeenCalledWith({
        agent: 'omp',
        cwd: '/repo/a',
        model: 'claude-sonnet',
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(startAgentMock).not.toHaveBeenCalled();
  });

  it('ACP 提交失败：错误上屏、弹窗不关', async () => {
    createAcpSessionMock.mockRejectedValue(new Error('boom'));
    const onClose = vi.fn();
    render(<StartDialog onClose={onClose} />, { wrapper });
    await waitFor(() => expect(screen.getByRole('tab', { name: 'ACP（omp）' })).toBeTruthy());
    fireEvent.click(screen.getByRole('tab', { name: 'ACP（omp）' }));
    await waitFor(() => expect(screen.getByLabelText(/ACP Agent/)).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: '启动' }));
    await waitFor(() => expect(screen.getByText(/boom/)).toBeTruthy());
    expect(onClose).not.toHaveBeenCalled();
  });

  it('ACP 提交中：按钮禁用并显示创建中（首次建会话 30–60s 提示在副标题）', async () => {
    let resolveCreate: (v: unknown) => void = () => {};
    createAcpSessionMock.mockReturnValue(
      new Promise((res) => {
        resolveCreate = res;
      }),
    );
    const onClose = vi.fn();
    render(<StartDialog onClose={onClose} />, { wrapper });
    await waitFor(() => expect(screen.getByRole('tab', { name: 'ACP（omp）' })).toBeTruthy());
    fireEvent.click(screen.getByRole('tab', { name: 'ACP（omp）' }));
    await waitFor(() => expect(screen.getByLabelText(/ACP Agent/)).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: '启动' }));
    expect(screen.getByRole('button', { name: '创建中…' })).toBeTruthy();
    resolveCreate({ sessionId: 's1' });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it('切 DSH：prompt 表单复用，启动带 driver=dsh 走 /api/agents/dsh 并关窗', async () => {
    localStorage.removeItem('hub_last_model'); // 前序用例写入的「记住上次值」隔离
    localStorage.removeItem('hub_last_effort');
    startAgentMock.mockResolvedValue({ id: 'session-new' });
    const onClose = vi.fn();
    render(<StartDialog onClose={onClose} />, { wrapper });
    // 选项来自 /api/drivers（注册表事实源，去硬编码）
    await waitFor(() => expect(screen.getByRole('tab', { name: 'DSH' })).toBeTruthy());
    fireEvent.click(screen.getByRole('tab', { name: 'DSH' }));
    // cwd 由工程树回填（CwdField）：主仓 /repo/a
    await waitFor(() =>
      expect((screen.getByLabelText(/工程 \(cwd\)/) as HTMLInputElement).value).toBe('/repo/a'),
    );
    fireEvent.change(screen.getByLabelText(/指令 Prompt/), {
      target: { value: 'hello dsh' },
    });
    fireEvent.click(screen.getByRole('button', { name: '启动' }));
    await waitFor(() => expect(startAgentMock).toHaveBeenCalledTimes(1));
    expect(startAgentMock).toHaveBeenCalledWith(
      { cwd: '/repo/a', prompt: 'hello dsh' },
      'dsh',
    );
    expect(createAcpSessionMock).not.toHaveBeenCalled();
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it('cwd 手输新路径：不受工程树选项限制，提交带手输值', async () => {
    localStorage.removeItem('hub_last_model');
    localStorage.removeItem('hub_last_effort');
    startAgentMock.mockResolvedValue({ id: 'x2' });
    renderDialog();
    await waitFor(() =>
      expect((screen.getByLabelText(/工程 \(cwd\)/) as HTMLInputElement).value).toBe('/repo/a'),
    );
    // 手输树外目录（服务机新路径）
    fireEvent.change(screen.getByLabelText(/工程 \(cwd\)/), {
      target: { value: '/brand/new/dir' },
    });
    fireEvent.change(screen.getByLabelText(/指令 Prompt/), { target: { value: 'p' } });
    fireEvent.click(screen.getByRole('button', { name: '启动' }));
    await waitFor(() => expect(startAgentMock).toHaveBeenCalledTimes(1));
    expect(startAgentMock).toHaveBeenCalledWith(
      expect.objectContaining({ cwd: '/brand/new/dir' }),
      'claude',
    );
  });
});
