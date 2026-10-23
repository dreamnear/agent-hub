// @vitest-environment happy-dom
// 文件树（agent-hub-files-panel F1/F2）：懒加载渲染 / 点文件上抛 / 空态 / 行对齐 tripwire。
import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';
import FilesTree from './FilesTree';
import { DOC_REFRESH_MS } from '../lib/docRefresh';
import type { DocEntry } from '../types';

const docsListMock = vi.fn();

vi.mock('../api', () => ({
  api: {
    docsList: (path: string) => docsListMock(path),
  },
}));

const ROOT_ENTRIES: DocEntry[] = [
  { name: 'docs', isDir: true, kind: 'dir' },
  { name: 'README.md', isDir: false, kind: 'markdown' },
];

const Wrapper = ({ children }: { children: ReactElement }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const renderTree = (root: string | null, active?: string): { container: HTMLElement } => {
  const onOpenFile = vi.fn();
  const { container } = render(
    <Wrapper>
      <FilesTree root={root} activePath={active} onOpenFile={onOpenFile} />
    </Wrapper>,
  );
  return { container };
};

afterEach(() => {
  cleanup();
  docsListMock.mockReset();
});

describe('FilesTree（右面板文件树，agent-hub-files-panel）', () => {
  it('懒加载渲染根层；目录展开拉子层；点文件上抛 onOpenFile（F2）', async () => {
    docsListMock.mockImplementation((path: string) =>
      path === '/repo'
        ? Promise.resolve(ROOT_ENTRIES)
        : Promise.resolve([{ name: 'c.txt', isDir: false, kind: 'text' }]),
    );
    const onOpenFile = vi.fn();
    const { container } = render(
      <Wrapper>
        <FilesTree root="/repo" onOpenFile={onOpenFile} />
      </Wrapper>,
    );
    await waitFor(() => expect(container.textContent).toContain('README.md'));
    // 目录展开 → 懒加载子层
    fireEvent.click(screen.getByRole('button', { name: /docs/ }));
    await waitFor(() => expect(container.textContent).toContain('c.txt'));
    // 点文件上抛（绝对路径 + 名字）
    fireEvent.click(screen.getByRole('button', { name: /README\.md/ }));
    expect(onOpenFile).toHaveBeenCalledWith('/repo/README.md', 'README.md');
  });

  it('root=null：空态提示（与 git 面板无会话同款）', () => {
    const { container } = renderTree(null);
    expect(container.textContent).toContain('选中会话后可浏览其工作目录');
    expect(container.querySelector('.doc-row')).toBeNull();
  });

  it('activePath 命中行高亮（主区打开的文件与面板行同步）', async () => {
    docsListMock.mockResolvedValue(ROOT_ENTRIES);
    const { container } = renderTree('/repo', '/repo/README.md');
    await waitFor(() => expect(container.querySelector('.doc-row--active')).not.toBeNull());
    expect(container.querySelector('.doc-row--active')?.textContent).toContain('README.md');
  });

  it('行左对齐 CSS 契约（tripwire，迁移自 DocViewer 反馈轮 14）', () => {
    // r43 盲区修复随树组件迁入 FilesTree.css：全局 button { justify-content: center }
    // 漏进满宽行容器会让行内容水平居中漂移。happy-dom 不套组件样式表，源级钉死：
    // .doc-row 显式 flex-start 压制全局居中，且无行级居中残留。
    const css = readFileSync('src/components/FilesTree.css', 'utf8').replace(
      /\/\*[\s\S]*?\*\//g,
      '',
    );
    const rowBlock = css.match(/\.doc-row \{[^}]*\}/)?.[0] ?? '';
    expect(rowBlock).toContain('justify-content: flex-start');
    expect(rowBlock).not.toMatch(/justify-content:\s*center/);
    expect(rowBlock).not.toMatch(/margin:\s*0\s+auto/);
    expect(rowBlock).not.toMatch(/text-align:\s*center/);
    // kind 标签固定右侧列：margin-left: auto，不参与行居中
    const kindBlock = css.match(/\.doc-kind \{[^}]*\}/)?.[0] ?? '';
    expect(kindBlock).toContain('margin-left: auto');
  });
});

describe('FilesTree 自动刷新（agent-hub-docs-tree-refresh #36 主修）', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('折叠→再展开总是重取（手动刷新入口）：新写入文件在二次清单中入树', async () => {
    docsListMock.mockImplementation((path: string) =>
      path === '/repo'
        ? Promise.resolve(ROOT_ENTRIES)
        : Promise.resolve([{ name: 'c.txt', isDir: false, kind: 'text' }]),
    );
    const { container } = renderTree('/repo');
    await waitFor(() => expect(container.textContent).toContain('README.md'));
    // 首次展开 docs：拉子层
    const docsBtn = (): HTMLElement => screen.getByRole('button', { name: /docs/ });
    fireEvent.click(docsBtn());
    await waitFor(() => expect(container.textContent).toContain('c.txt'));
    const callsBefore = docsListMock.mock.calls.length;
    // agent 写入新文件 → 折叠再展开：docsList 再次拉取、新文件出现（无需刷新页面）
    docsListMock.mockImplementation((path: string) =>
      path === '/repo'
        ? Promise.resolve(ROOT_ENTRIES)
        : Promise.resolve([
            { name: 'c.txt', isDir: false, kind: 'text' },
            { name: 'new.md', isDir: false, kind: 'markdown' },
          ]),
    );
    fireEvent.click(docsBtn()); // 折叠
    fireEvent.click(docsBtn()); // 再展开 = 手动刷新
    await waitFor(() => expect(container.textContent).toContain('new.md'));
    expect(docsListMock.mock.calls.length).toBeGreaterThan(callsBefore);
  });

  it('面板挂载期对已展开目录周期重取（DOC_REFRESH_MS）；未展开子层不预拉（懒加载不回退）', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    const calls: string[] = [];
    docsListMock.mockImplementation((path: string) => {
      calls.push(path);
      return path === '/repo'
        ? Promise.resolve(ROOT_ENTRIES)
        : Promise.resolve([{ name: 'c.txt', isDir: false, kind: 'text' }]);
    });
    const { container } = renderTree('/repo');
    await waitFor(() => expect(container.textContent).toContain('README.md'));
    const baselineRoot = calls.length;
    fireEvent.click(screen.getByRole('button', { name: /docs/ }));
    await waitFor(() => expect(container.textContent).toContain('c.txt'));
    expect(calls.filter((p) => p.startsWith('/repo/docs/'))).toEqual([]); // 子层懒加载
    const baseline = calls.length;
    // 推进一个刷新周期：已展开的 /repo（根）与 /repo/docs 各重取一次
    await act(async () => {
      await vi.advanceTimersByTimeAsync(DOC_REFRESH_MS + 1);
    });
    expect(calls.length).toBe(baseline + 2);
    expect(calls.slice(baseline)).toContain('/repo');
    expect(calls.slice(baseline)).toContain('/repo/docs');
    expect(calls.filter((p) => p.startsWith('/repo/docs/'))).toEqual([]); // 仍未预拉子层
    expect(calls.length).toBeGreaterThan(baselineRoot);
  });
});
