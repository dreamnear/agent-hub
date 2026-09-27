// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import GitPanel from './GitPanel';
import type { GitStatus, GitTreeNode, SubmoduleInfo } from '../types';

const gitStatusMock = vi.fn();
const gitDiffMock = vi.fn();
const submodulesMock = vi.fn();
const createWorktreeMock = vi.fn();
vi.mock('../api', () => ({
  api: {
    gitStatus: (path: string) => gitStatusMock(path),
    gitDiff: (path: string, file: string, cached: boolean) => gitDiffMock(path, file, cached),
    submodules: (path: string) => submodulesMock(path),
    createWorktree: (base: string, name: string) => createWorktreeMock(base, name),
  },
}));

const node: GitTreeNode = {
  path: '/repo/main',
  name: 'main-repo',
  isMain: true,
  isGit: true,
  branch: 'main',
  head: null,
};

const STATUS: GitStatus = {
  branch: 'main',
  upstream: 'origin/main',
  ahead: 1,
  behind: 2,
  files: [
    { x: 'A', y: ' ', path: 'staged.txt', origPath: null },
    { x: ' ', y: 'M', path: 'modified.txt', origPath: null },
    { x: '?', y: '?', path: 'new.txt', origPath: null },
  ],
};

const SUBS: SubmoduleInfo[] = [{ status: '+', sha: 'abc123de', path: 'libs/sub' }];

const renderPanel = (): { container: HTMLElement } => ({
  container: render(
    <QueryClientProvider client={new QueryClient()}>
      <GitPanel node={node} />
    </QueryClientProvider>,
  ).container,
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('GitPanel', () => {
  it('shows branch, ahead/behind and grouped change files', async () => {
    // B5+B6：分支/upstream/ahead-behind + staged/unstaged/untracked 三组
    gitStatusMock.mockResolvedValue(STATUS);
    submodulesMock.mockResolvedValue([]);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-files')).not.toBeNull());
    expect(container.querySelector('.git-title')?.textContent).toContain('main');
    expect(container.querySelector('.git-ab')?.textContent).toContain('↑1');
    const groups = container.querySelectorAll('.git-group-title');
    expect(groups.length).toBe(3);
    expect(groups[0].textContent).toContain('已暂存 (1)');
    expect(groups[1].textContent).toContain('未暂存 (1)');
    expect(groups[2].textContent).toContain('未跟踪 (1)');
  });

  it('opens a read-only diff on file click and lists submodules', async () => {
    // B6 diff：点击文件加载 diff 内容；B7 submodule 清单渲染（漂移标 +）
    gitStatusMock.mockResolvedValue(STATUS);
    submodulesMock.mockResolvedValue(SUBS);
    gitDiffMock.mockResolvedValue({ diff: '-hello\n+world' });
    const { container } = render(<GitPanelWithWrap />, { wrapper: Panel });
    await waitFor(() => expect(container.querySelector('.git-files')).not.toBeNull());
    expect(container.textContent).toContain('libs/sub');
    const fileBtns = container.querySelectorAll<HTMLButtonElement>('.git-file');
    fireEvent.click(fileBtns[1]); // modified.txt（工作区）
    await waitFor(() => expect(container.querySelector('.git-diff')?.textContent).toContain('+world'));
    expect(gitDiffMock).toHaveBeenCalledWith('/repo/main', 'modified.txt', false);
  });

  it('creates a worktree from the selected node and surfaces conflicts', async () => {
    // B4：表单提交 → createWorktree(选中节点, 分支名)；冲突 409 文案透出
    gitStatusMock.mockResolvedValue(STATUS);
    submodulesMock.mockResolvedValue([]);
    createWorktreeMock.mockRejectedValueOnce(new Error('409: 分支 feat-demo 已存在'));
    const { container } = render(<GitPanelWithWrap />, { wrapper: Panel });
    await waitFor(() => expect(container.querySelector('.git-wt-form input')).not.toBeNull());
    const input = container.querySelector('.git-wt-form input') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'feat-demo' } });
    fireEvent.click(container.querySelector('.git-wt-form button') as HTMLButtonElement);
    await waitFor(() =>
      expect(container.querySelector('.git-wt-msg')?.textContent).toContain('已存在'),
    );
    expect(createWorktreeMock).toHaveBeenCalledWith('/repo/main', 'feat-demo');
  });
});

const Panel = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

function GitPanelWithWrap(): ReactElement {
  return (
    <QueryClientProvider client={new QueryClient()}>
      <GitPanel node={node} />
    </QueryClientProvider>
  );
}
