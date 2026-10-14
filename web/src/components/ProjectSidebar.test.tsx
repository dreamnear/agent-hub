// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';
import ProjectSidebar, { branchRedundant } from './ProjectSidebar';
import type { AgentSummary, GitTreeGroup } from '../types';
import type { ReactNode } from 'react';

const projectTreeMock = vi.fn();
const openDirMock = vi.fn();
vi.mock('../api', () => ({
  api: {
    projectTree: () => projectTreeMock(),
    openDir: (path: string) => openDirMock(path),
  },
}));

const agents: AgentSummary[] = [
  {
    driver: 'claude',
    id: 'a1',
    name: null,
    cwd: '/repo/main',
    kind: 'background',
    rawState: null,
    group: 'working',
    detail: null,
    tokens: null,
  context_tokens: null,
    startedAt: null,
    sessionId: null,
  },
];

const group = (mainPath: string, branch: string | null, wts: string[]): GitTreeGroup => ({
  main: { path: mainPath, name: mainPath.split('/').pop() ?? mainPath, isMain: true, isGit: true, branch, head: null },
  worktrees: wts.map((w) => ({
    path: w,
    name: w.split('/').pop() ?? w,
    isMain: false,
    isGit: true,
    branch: 'feat-x',
    head: null,
  })),
});

const TREE: GitTreeGroup[] = [
  group('/repo/main', 'main', ['/repo/.worktree/feat', '/repo/.worktree/plan']),
  group('/opt/plain', null, []),
];

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('ProjectSidebar git tree', () => {
  it('groups main repo with collapsible worktree children', async () => {
    // B1/B2：主仓为父节点、worktree 为子节点（details 折叠），分支名显示
    projectTreeMock.mockResolvedValue(TREE);
    const { container } = render(<ProjectSidebarTable />, { wrapper });
    await waitFor(() => expect(container.querySelector('details.tree-group')).not.toBeNull());
    const summary = container.querySelector('summary.tree-row');
    expect(summary?.textContent).toContain('main');
    // 子节点渲染（默认 open），分支名可见
    const subs = container.querySelectorAll('.tree-sub-row');
    expect(subs.length).toBe(2);
    expect(subs[0].textContent).toContain('feat');
    expect(container.textContent).toContain('plain'); // 无 worktree 平铺
  });

  it('calls openDir when the open button is clicked and shows error on 403', async () => {
    // B3：节点 📂 按钮 → openDir(path)；白名单外失败显示错误提示
    projectTreeMock.mockResolvedValue(TREE);
    openDirMock.mockRejectedValueOnce(new Error('403'));
    const { container } = render(<ProjectSidebarTable />, { wrapper });
    await waitFor(() => expect(container.querySelector('.tree-open')).not.toBeNull());
    const btns = container.querySelectorAll<HTMLButtonElement>('.tree-open');
    fireEvent.click(btns[0]);
    await waitFor(() => expect(openDirMock).toHaveBeenCalledWith('/repo/main'));
    await waitFor(() => expect(container.querySelector('.sidebar-open-err')?.textContent).toContain('403'));

    // 成功路径：不落错误提示
    openDirMock.mockResolvedValueOnce(undefined);
    fireEvent.click(btns[0]);
    await waitFor(() => expect(container.querySelector('.sidebar-open-err')).toBeNull());
  });

  it('falls back to the flat registered list when the tree is empty', () => {
    // 树为空（探测未就绪/全空）→ 回退注册列表平铺（旧行为）
    projectTreeMock.mockResolvedValue([]);
    const { container } = render(<ProjectSidebarTable />, { wrapper });
    expect(container.querySelector('details.tree-group')).toBeNull();
    expect(container.querySelectorAll('.tree-row').length).toBe(1);
    expect(container.querySelector('.tree-name')?.textContent).toBe('main');
  });

  it('hides the branch sub-row when redundant with the node name', async () => {
    // 两行式布局：组头 branch 'main' 与名称冗余 → 副行不渲染（名称行垂直居中）；
    // 子行分支非冗余 → 副行存在且文本正确
    projectTreeMock.mockResolvedValue(TREE);
    const { container } = render(<ProjectSidebarTable />, { wrapper });
    await waitFor(() => expect(container.querySelector('details.tree-group')).not.toBeNull());
    expect(container.querySelector('summary.tree-row .tree-branch')).toBeNull();
    const subBranch = container.querySelector('.tree-sub-row .tree-branch');
    expect(subBranch?.textContent).toBe('feat-x');
  });
});

describe('branchRedundant', () => {
  it('marks name/branch duplicates as redundant after normalization', () => {
    expect(branchRedundant('plan', 'plan')).toBe(true); // 完全相等
    expect(branchRedundant('Main', 'main')).toBe(true); // 大小写差异
    expect(branchRedundant('feat_x', 'feat-x')).toBe(true); // 分隔符差异
    expect(branchRedundant('feat.x', 'featx')).toBe(true); // 点分隔符差异
    expect(branchRedundant('feat', 'fix')).toBe(false); // 真实不同分支保留显示
    expect(branchRedundant('repo', null)).toBe(true); // 无分支 → 冗余（槽留空）
  });
});

/// 受控 props 的占位包裹（组件导出名不变，测试内简化 render 签名）
function ProjectSidebarTable(): ReactElement {
  return (
    <ProjectSidebar
      agents={agents}
      projects={['/repo/main']}
      checked={new Set()}
      pending=""
      onPendingChange={() => {}}
      onToggle={() => {}}
      onAdd={() => {}}
      selected={null}
      onSelect={() => {}}
    />
  );
}
