// @vitest-environment happy-dom
import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import DocViewer from './DocViewer';
import type { DocEntry } from '../types';

const docsListMock = vi.fn();
const docsFileMock = vi.fn();
vi.mock('../api', () => ({
  api: {
    docsList: (path: string) => docsListMock(path),
    docsFile: (path: string) => docsFileMock(path),
  },
}));
vi.mock('./MarkdownView', () => ({ default: ({ text }: { text: string }) => <div>MD:{text}</div> }));

const ROOT = '/repo/main';
const DOCS = `${ROOT}/docs`;

const ROOT_ENTRIES: DocEntry[] = [
  { name: 'docs', isDir: true, kind: 'dir' },
  { name: 'README.md', isDir: false, kind: 'markdown' },
  { name: 'app.bin', isDir: false, kind: 'binary' },
];
const DOCS_ENTRIES: DocEntry[] = [{ name: 'deep.txt', isDir: false, kind: 'text' }];

/**
 * 真实数据 fixture（反馈轮 13）：personal 仓根的 /api/docs/list 实际响应原样快照
 * （curl 2026-09-19）——含点开头目录/文件、大小写混合、无扩展名二进制等不规则形态。
 * 树 depth 由渲染递归层级决定（响应无 path 字段），本 case 防回归：
 * 真实不规则数据下根级对齐不被打破。
 */
const PERSONAL_REAL: DocEntry[] = [
  { name: '.claude', isDir: true, kind: 'dir' },
  { name: '.git', isDir: true, kind: 'dir' },
  { name: '.playwright-cli', isDir: true, kind: 'dir' },
  { name: '.playwright-mcp', isDir: true, kind: 'dir' },
  { name: 'claude-obsidian', isDir: true, kind: 'dir' },
  { name: 'docs', isDir: true, kind: 'dir' },
  { name: 'md-preview', isDir: true, kind: 'dir' },
  { name: 'projects', isDir: true, kind: 'dir' },
  { name: 'src', isDir: true, kind: 'dir' },
  { name: 'vault', isDir: true, kind: 'dir' },
  { name: 'xianyu-listing', isDir: true, kind: 'dir' },
  { name: '.DS_Store', isDir: false, kind: 'binary' },
  { name: '.gitignore', isDir: false, kind: 'text' },
  { name: '.gitmodules', isDir: false, kind: 'binary' },
  { name: '.preview-slot', isDir: false, kind: 'binary' },
  { name: '.preview.pid', isDir: false, kind: 'binary' },
  { name: 'CLAUDE.md', isDir: false, kind: 'markdown' },
  { name: 'TODO.md', isDir: false, kind: 'markdown' },
  { name: 'listing.png', isDir: false, kind: 'binary' },
  { name: 'ops.md', isDir: false, kind: 'markdown' },
  { name: 'pricing.md', isDir: false, kind: 'markdown' },
];
const PROJECTS_REAL: DocEntry[] = [
  { name: 'claude-view', isDir: true, kind: 'dir' },
  { name: 'ecom-ai-agent', isDir: true, kind: 'dir' },
];

// 按路径分发：root / docs 子目录 / personal 真实形态
docsListMock.mockImplementation((path: string): Promise<DocEntry[]> => {
  if (path === ROOT) return Promise.resolve(ROOT_ENTRIES);
  if (path === DOCS) return Promise.resolve(DOCS_ENTRIES);
  if (path === '/Users/alice/Works/personal') return Promise.resolve(PERSONAL_REAL);
  if (path === '/Users/alice/Works/personal/projects') return Promise.resolve(PROJECTS_REAL);
  return Promise.resolve([]);
});

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider
    client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}
  >
    {children}
  </QueryClientProvider>
);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('DocViewer tree (反馈轮 12)', () => {
  it('renders root entries with same-level left alignment', async () => {
    // 同级条目左缘严格对齐：root 三个条目 paddingLeft 一致（depth 0）
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    const rows = [...container.querySelectorAll<HTMLButtonElement>('.doc-row')];
    const paddings = new Set(rows.map((r) => r.style.paddingLeft));
    expect(paddings.size).toBe(1);
    // 目录折叠箭头 + 文件图标区分
    expect(rows[0].textContent).toContain('▸');
    expect(rows[0].textContent).toContain('docs');
    expect(rows[1].textContent).toContain('📄');
    expect(rows[1].textContent).toContain('README.md');
  });

  it('expands and collapses a directory with lazy-loaded children', async () => {
    // B8 懒加载：首次展开拉子目录；折叠隐藏子条目；再展开不重复拉取（缓存命中）
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    expect(container.textContent).not.toContain('deep.txt');

    const dirRow = container.querySelectorAll<HTMLButtonElement>('.doc-row')[0];
    fireEvent.click(dirRow);
    await waitFor(() => expect(container.textContent).toContain('deep.txt'));
    // 子级缩进比父级深一层
    const child = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('deep.txt'),
    ) as HTMLButtonElement;
    const parentPad = parseFloat(dirRow.style.paddingLeft);
    const childPad = parseFloat(child.style.paddingLeft);
    expect(childPad).toBe(parentPad + 14);

    // 折叠 → 子条目消失；再展开（缓存命中不重复请求）
    fireEvent.click(dirRow);
    await waitFor(() => expect(container.textContent).not.toContain('deep.txt'));
    const callsAfterCollapse = docsListMock.mock.calls.length;
    fireEvent.click(dirRow);
    await waitFor(() => expect(container.textContent).toContain('deep.txt'));
    expect(docsListMock.mock.calls.length).toBe(callsAfterCollapse);
  });

  it('previews a file and highlights the active row', async () => {
    // 点击文件 → 预览 + 行高亮；binary 无预览类型标
    docsFileMock.mockResolvedValue({ kind: 'markdown', name: 'README.md', content: '# T' });
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    const readme = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('README.md'),
    ) as HTMLButtonElement;
    fireEvent.click(readme);
    await waitFor(() => expect(container.textContent).toContain('MD:# T'));
    expect(readme.className).toContain('doc-row--active');
    // binary 行无 kind 标
    const bin = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('app.bin'),
    ) as HTMLElement;
    expect(bin.querySelector('.doc-kind')).toBeNull();
  });

  it('keeps same-level alignment with real-world irregular entries (personal root)', async () => {
    // 反馈轮 13 真实数据驱动：personal 根 21 条真实响应（点开头目录/文件、大小写混合）
    // —— 根级全部同 padding；展开 projects 子目录后子级 = 父 + 14，同级再对齐
    const personalRoot = '/Users/alice/Works/personal';
    const { container } = render(<DocViewer root={personalRoot} />, { wrapper });
    await waitFor(() =>
      expect(container.querySelectorAll('.doc-row').length).toBe(PERSONAL_REAL.length),
    );
    const rootRows = [...container.querySelectorAll('.doc-row')];
    const pads = new Set(rootRows.map((r) => (r as HTMLButtonElement).style.paddingLeft));
    expect(pads.size).toBe(1); // 全部 depth 0，根级严格对齐
    // 点开头的目录/文件渲染无异常
    expect(rootRows.find((r) => r.textContent?.includes('.claude'))).not.toBeNull();
    expect(rootRows.find((r) => r.textContent?.includes('.gitmodules'))).not.toBeNull();

    // 展开 projects → 子级 = 父 + 14，同级两个子目录再对齐
    const projRow = rootRows.find((r) => r.textContent?.includes('projects')) as HTMLButtonElement;
    fireEvent.click(projRow);
    await waitFor(() => expect(container.textContent).toContain('claude-view'));
    const childRows = [...container.querySelectorAll('.doc-row')].filter((r) =>
      ['claude-view', 'ecom-ai-agent'].includes(r.querySelector('.doc-row-name')?.textContent ?? ''),
    );
    expect(childRows.length).toBe(2);
    const childPads = new Set(childRows.map((r) => (r as HTMLButtonElement).style.paddingLeft));
    expect(childPads.size).toBe(1);
    const parentPad = parseFloat(projRow.style.paddingLeft);
    expect(parseFloat([...childPads][0])).toBe(parentPad + 14);
  });

  it('keeps tree rows left-aligned against global button centering (反馈轮 14)', () => {
    // r43 盲区修复：全局 button { justify-content: center } 漏进满宽行容器导致内容
    // 水平居中（目录行各自漂移）。happy-dom 不套组件样式表，水平定位在源级钉死：
    // .doc-row 必须显式 flex-start 压制全局居中，且无任何行级居中残留。
    const css = readFileSync('src/components/DocViewer.css', 'utf8');
    const rowBlock = css.match(/\.doc-row \{[^}]*\}/)?.[0] ?? '';
    expect(rowBlock).toContain('justify-content: flex-start');
    expect(rowBlock).not.toMatch(/justify-content:\s*center/);
    expect(rowBlock).not.toMatch(/margin:\s*0\s+auto/);
    expect(rowBlock).not.toMatch(/text-align:\s*center/);
    // kind 标签固定右侧列：margin-left: auto，不参与行居中
    const kindBlock = css.match(/\.doc-kind \{[^}]*\}/)?.[0] ?? '';
    expect(kindBlock).toContain('margin-left: auto');
  });

  it('renders dir rows and file rows with the same layout class', async () => {
    // 目录行/文件行同一布局容器：基类一致（禁止两种对齐路径）
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    const dirRow = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('docs'),
    ) as HTMLButtonElement;
    const fileRow = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('README.md'),
    ) as HTMLButtonElement;
    expect(dirRow.className.replace(' doc-row--active', '')).toBe(fileRow.className);
  });

  it('splits tree and preview into left/right panes (反馈轮 15)', async () => {
    // 左树右内容：.doc-tree 必须在 .doc-tree-pane 内，预览卡片在 .doc-preview-pane 内；
    // 未选中文件时右栏有空态提示
    docsFileMock.mockResolvedValue({ kind: 'markdown', name: 'README.md', content: '# T' });
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    expect(container.querySelector('.doc-tree-pane')?.querySelector('.doc-tree')).not.toBeNull();
    const pane = container.querySelector('.doc-preview-pane');
    expect(pane?.querySelector('.doc-empty-hint')?.textContent).toContain('在左侧选择文件预览');

    const readme = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('README.md'),
    ) as HTMLButtonElement;
    fireEvent.click(readme);
    await waitFor(() => expect(pane?.querySelector('.doc-preview')).not.toBeNull());
    expect(pane?.querySelector('.doc-empty-hint')).toBeNull();
  });

  it('pins split-pane layout and tree scrolling in CSS (反馈轮 15)', () => {
    // happy-dom 不套样式表，布局/滚动契约在源级钉死：树窗格固定宽 + overflow-y，
    // 内容窗格 flex:1 占满余宽；旧单列居中残留（min(748px)/margin auto）必须消失
    const css = readFileSync('src/components/DocViewer.css', 'utf8');
    const treeBlock = css.match(/\.doc-tree-pane \{[^}]*\}/)?.[0] ?? '';
    expect(treeBlock).toContain('overflow-y: auto');
    expect(treeBlock).toMatch(/width:\s*280px/);
    const paneBlock = css.match(/\.doc-preview-pane \{[^}]*\}/)?.[0] ?? '';
    expect(paneBlock).toContain('flex: 1');
    const viewerBlock = css.match(/\.doc-viewer \{[^}]*\}/)?.[0] ?? '';
    expect(viewerBlock).not.toMatch(/margin:\s*0\s+auto/);
    expect(css).not.toMatch(/width:\s*min\(748px/);
    // 内容视图不再 50vh 截断（右区自滚）
    expect(css).not.toMatch(/max-height:\s*50vh/);
  });

  it('toggles mobile tree/preview states with kept selection (反馈轮 16)', async () => {
    // 两态由 --m-preview 类驱动（CSS 媒体查询消费）：点文件进预览态（返回栏含相对
    // 路径），点返回栏回树态但保留已选文件，再点同一文件直接恢复预览态
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    const viewer = () => container.querySelector('.doc-viewer') as HTMLDivElement;
    expect(viewer().className).not.toContain('doc-viewer--m-preview');

    const readme = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('README.md'),
    ) as HTMLButtonElement;
    fireEvent.click(readme);
    expect(viewer().className).toContain('doc-viewer--m-preview');
    const back = container.querySelector('.doc-mobile-back') as HTMLButtonElement;
    expect(back.textContent).toContain('README.md');
    expect(back.querySelector('.doc-mobile-back-arrow')?.textContent).toBe('‹');

    // 回树态：类移除，但预览文件状态保留（doc-preview 仍在组件树中）
    fireEvent.click(back);
    expect(viewer().className).not.toContain('doc-viewer--m-preview');
    expect(container.querySelector('.doc-preview')).not.toBeNull();

    // 再点同一文件 → 直接恢复预览态
    fireEvent.click(readme);
    expect(viewer().className).toContain('doc-viewer--m-preview');
  });

  it('pins mobile two-state chrome to the ≤768px media query (反馈轮 16)', () => {
    // 桌面零变化：两态专用元素桌面 display:none；44px 返回栏/树态全高只在媒体查询内
    // （剥注释后断言，防说明文字误中）
    const css = readFileSync('src/components/DocViewer.css', 'utf8').replace(
      /\/\*[\s\S]*?\*\//g,
      '',
    );
    const mq = css.slice(css.lastIndexOf('@media (max-width: 768px)'));
    expect(mq).toContain('.doc-viewer--m-preview');
    expect(mq).toMatch(/\.doc-viewer--m-preview \.doc-tree-pane \{[^}]*flex:\s*0 0 44px/);
    expect(mq).toMatch(/\.doc-viewer--m-preview \.doc-preview-pane \{[^}]*flex:\s*1/);
    expect(mq).toContain('.doc-mobile-back-arrow');
    // 强调色箭头（设计稿 #10a37f → var(--c-accent)）
    expect(mq).toMatch(/\.doc-mobile-back-arrow \{[^}]*var\(--c-accent/);
    // 桌面区（媒体查询外）不含两态选择器
    const desktop = css.slice(0, css.lastIndexOf('@media (max-width: 768px)'));
    expect(desktop).not.toContain('doc-viewer--m-preview');
    expect(desktop).toMatch(/\.doc-mobile-head,\n\.doc-mobile-back \{\n  display: none;/);
  });

  it('surfaces the server size-limit message for oversized files', async () => {
    // B9：>512KB 的 413 服务端文案透出（修复串台图片上传文案）
    docsFileMock.mockRejectedValue(new Error('文件超过预览大小上限（512KB），仅列出不可预览'));
    const { container } = render(<DocViewer root={ROOT} />, { wrapper });
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(3));
    const readme = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('README.md'),
    ) as HTMLButtonElement;
    fireEvent.click(readme);
    await waitFor(() =>
      expect(container.querySelector('.doc-hint--err')?.textContent).toContain('512KB'),
    );
  });
});
