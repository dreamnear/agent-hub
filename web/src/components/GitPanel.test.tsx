// @vitest-environment happy-dom
// Git 面板（agent-hub-git-timeline 分类化）：变更/Timeline 可折叠分区 + 多仓聚合
// （父仓与 submodule 各得分区）+ Timeline 提交历史（限量+加载更多+点击复制 hash）+
// 只读立场（worktree 创建入口已移除——T4）。既有纯函数（statusKind/buildFileTree/
// view 持久化）全保留。git-sections-polish：分区头组视觉/左对齐 + 变更头概要。
import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import GitPanel, {
  buildFileTree,
  loadGitView,
  markOf,
  saveGitView,
  statusKind,
  type FileEntry,
} from './GitPanel';
import type { Api } from '../api';
import type { GitCommitFiles, GitLog, GitStatus, SubmoduleInfo } from '../types';

const gitStatusMock = vi.fn();
const gitDiffMock = vi.fn();
const submodulesMock = vi.fn();
const gitLogMock = vi.fn();
const commitFilesMock = vi.fn();
const commitDiffMock = vi.fn();
const apiStub = {
  gitStatus: (path: string) => gitStatusMock(path),
  gitDiff: (path: string, file: string, cached: boolean) => gitDiffMock(path, file, cached),
  submodules: (path: string) => submodulesMock(path),
  gitLog: (path: string, limit?: number, skip?: number) => gitLogMock(path, limit, skip),
  commitFiles: (path: string, hash: string, limit?: number) => commitFilesMock(path, hash, limit),
  commitDiff: (path: string, hash: string, file: string) => commitDiffMock(path, hash, file),
} as unknown as Api;

const STATUS: GitStatus = {
  branch: 'main',
  upstream: 'origin/main',
  ahead: 1,
  behind: 2,
  files: [
    { x: 'A', y: ' ', path: 'staged.txt', origPath: null },
    { x: ' ', y: 'M', path: 'docs/modified.txt', origPath: null },
    { x: '?', y: '?', path: 'new.txt', origPath: null },
  ],
};

const SUBS: SubmoduleInfo[] = [
  { status: '+', sha: 'abc123de', path: 'libs/sub', changes: 2, untracked: 1 },
  { status: ' ', sha: 'def456ab', path: 'libs/clean', changes: 0, untracked: 0 },
];

const LOG: GitLog = {
  commits: [
    { hash: 'aaa1111', author: 't', relTime: '2 hours ago', subject: 'latest commit' },
    { hash: 'bbb2222', author: 't', relTime: '3 days ago', subject: 'older commit' },
  ],
  hasMore: true,
};

const CWD = '/repo/main';
const INST = 'inst-x';

/// git-timeline-files：提交文件维清单 fixture（M + 改名 R100 各一）
const CFILES: GitCommitFiles = {
  files: [
    { status: 'M', path: 'src/app.ts', origPath: null },
    { status: 'R100', path: 'src/new.ts', origPath: 'src/old.ts' },
  ],
  hasMore: false,
};

const renderPanel = (onOpenDiff = vi.fn()): { container: HTMLElement; onOpenDiff: ReturnType<typeof vi.fn> } => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const container = render(
    <QueryClientProvider client={client}>
      <GitPanel cwd={CWD} api={apiStub} instanceId={INST} onOpenDiff={onOpenDiff} />
    </QueryClientProvider>,
  ).container;
  return { container, onOpenDiff };
};

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.clearAllMocks();
});

describe('GitPanel（分类化：变更/Timeline 分区）', () => {
  it('按 cwd 查状态（queryKey 含实例维），渲染分支/ahead-behind/三组变更（变更区现状保留）', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => {
      expect(container.querySelector('.git-title')?.textContent).toContain('main');
    });
    // git-changes-meta-dedup M1：↑↓/计数唯一入口=「变更」分区头概要（meta 行已删）
    const changesHead = [...container.querySelectorAll('.git-section-head')].find((h) =>
      h.textContent?.includes('变更'),
    ) as HTMLElement;
    expect(changesHead.textContent).toContain('↑1');
    expect(changesHead.textContent).toContain('↓2');
    expect(container.textContent).toContain('staged.txt');
    expect(container.textContent).toContain('new.txt');
    expect(gitStatusMock).toHaveBeenCalledWith(CWD);
  });

  it('Timeline 分区渲染提交历史（hash/subject/作者/相对时间）', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => {
      expect(container.querySelector('.git-commit')).not.toBeNull();
    });
    expect(container.textContent).toContain('latest commit');
    expect(container.textContent).toContain('2 hours ago');
    expect(gitLogMock).toHaveBeenCalledWith(CWD, 50, 0);
  });

  it('加载更多：limit 递增 50 重新拉取', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-commit')).not.toBeNull());
    fireEvent.click(screen.getByText('加载更多'));
    await waitFor(() => {
      expect(gitLogMock).toHaveBeenCalledWith(CWD, 100, 0);
    });
  });

  it('分区折叠持久化：折叠 Timeline 后刷新（重挂载）保持折叠', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-commit')).not.toBeNull());
    // 点 Timeline 分区头折叠
    fireEvent.click(screen.getByText('Timeline'));
    await waitFor(() => expect(container.querySelector('.git-commit')).toBeNull());
    // 重挂载（模拟刷新）：折叠态从 localStorage 恢复
    cleanup();
    localStorage.setItem('hub_git_view', 'flat');
    gitLogMock.mockClear();
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const c2 = render(
      <QueryClientProvider client={client}>
        <GitPanel cwd={CWD} api={apiStub} instanceId={INST} onOpenDiff={vi.fn()} />
      </QueryClientProvider>,
    ).container;
    await waitFor(() => expect(gitStatusMock).toHaveBeenCalled());
    expect(c2.querySelector('.git-commit')).toBeNull();
    expect(c2.textContent).toContain('Timeline');
  });

  it('变更分区折叠不影响 Timeline（独立分区）', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-commit')).not.toBeNull());
    const changesHead = [...container.querySelectorAll('.git-section-head')].find((h) =>
      h.textContent?.includes('变更'),
    ) as HTMLElement;
    fireEvent.click(changesHead);
    await waitFor(() => expect(container.querySelector('.git-title')).toBeNull());
    expect(container.querySelector('.git-commit')).not.toBeNull();
  });

  it('非 git 目录（gitStatus 报错）→ 变更分区空态，不影响 Timeline', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockRejectedValue(new Error('not a repo'));
    const { container } = renderPanel();
    await waitFor(() => {
      expect(container.textContent).toContain('不在 git 仓库内');
    });
    // Timeline 独立可用
    await waitFor(() => expect(container.querySelector('.git-commit')).not.toBeNull());
  });
});

describe('GitPanel 多仓聚合（T3b）', () => {
  it('submodule 各得可折叠分区（懒加载：收起不拉子仓数据），头含变更徽标', async () => {
    submodulesMock.mockResolvedValue(SUBS);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockImplementation((p: string) =>
      p === `${CWD}/libs/sub`
        ? Promise.resolve({ ...STATUS, branch: 'sub-main', files: [] })
        : Promise.resolve(STATUS),
    );
    renderPanel();
    // 子仓分区头：标签为路径尾段（sub），title 承载全相对路径；徽标 ● 2 = changes
    const subHead = (await screen.findByTitle('libs/sub')) as HTMLElement;
    expect(subHead.textContent).toContain('sub');
    expect(subHead.textContent).toContain('● 2');
    // 懒加载：收起态不拉子仓 status
    expect(gitStatusMock).not.toHaveBeenCalledWith(`${CWD}/libs/sub`);
    // 展开子仓分区 → 拉子仓 status（独立 query，不串仓）+ Timeline
    fireEvent.click(subHead);
    await waitFor(() => {
      expect(gitStatusMock).toHaveBeenCalledWith(`${CWD}/libs/sub`);
    });
    // 子仓 Timeline 以子仓 path 拉取
    await waitFor(() => {
      expect(gitLogMock).toHaveBeenCalledWith(`${CWD}/libs/sub`, 50, 0);
    });
  });

  it('无子仓项目单仓分区不变（无子仓分区渲染）', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-title')).not.toBeNull());
    expect(container.textContent).not.toContain('libs/sub');
  });
});

describe('GitPanel 只读立场（T4/T5）', () => {
  it('worktree 创建入口已移除（无表单/按钮/api 方法调用）', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-title')).not.toBeNull());
    expect(container.querySelector('.git-wt-form')).toBeNull();
    expect(container.textContent).not.toContain('worktree');
    expect(container.textContent).not.toContain('git.wtPh');
  });
});

describe('GitPanel 分区视觉（git-sections-polish U1-U3/P1-P2）', () => {
  it('U3/P2 变更分区头携带概要（分支/↑↓/计数），折叠态概要不丢、展开内容零回归', async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    const head = (await waitFor(() => {
      const h = [...container.querySelectorAll('.git-section-head')].find((x) =>
        x.textContent?.includes('变更'),
      ) as HTMLElement | undefined;
      expect(h?.textContent).toContain('main'); // 概要随父仓 status 查询落位
      return h;
    })) as HTMLElement;
    expect(head.textContent).toContain('↑1');
    expect(head.textContent).toContain('↓2');
    expect(head.textContent).toContain('· 3');
    // 展开态：内容与现状零回归（分支头/变更清单均在）
    expect(container.querySelector('.git-title')).not.toBeNull();
    expect(container.textContent).toContain('staged.txt');
    // 折叠：分区内容卸载，概要仍可辨（P2 收起态信息不丢）
    fireEvent.click(head);
    await waitFor(() => expect(container.querySelector('.git-title')).toBeNull());
    expect(head.textContent).toContain('main');
    expect(head.textContent).toContain('↑1↓2');
    expect(head.textContent).toContain('· 3');
  });

  it('P1/U1/U2 分区头组视觉 + 组名左对齐（CSS 源级守卫）', () => {
    const css = readFileSync('src/components/GitPanel.css', 'utf-8');
    const block = css.slice(
      css.indexOf('.git-section-head {'),
      css.indexOf('.git-section-head:hover'),
    );
    // U1 组视觉：通栏底色 + 描边圆角（非透明裸文字）
    expect(block).toContain('background: var(--c-code');
    expect(block).toContain('border: 1px solid var(--c-border');
    expect(block).toContain('border-radius');
    // U2 组名左对齐：显式 flex-start，不随 UA 按钮居中
    expect(block).toContain('justify-content: flex-start');
    expect(block).toContain('text-align: left');
    // hover 反馈态存在
    expect(css).toContain('.git-section-head:hover');
  });
});

describe('GitPanel 子仓库不进变更列表（git-submodule-rows G1/G3）', () => {
  it('G1 submodule 行从变更列表排除，计数不含子仓；普通文件与聚合分区不受影响', async () => {
    submodulesMock.mockResolvedValue(SUBS);
    gitLogMock.mockResolvedValue(LOG);
    // 父仓 status：1 普通文件 + 2 个 submodule 条目（untracked/modified 形态）
    gitStatusMock.mockResolvedValue({
      ...STATUS,
      files: [
        { x: ' ', y: 'M', path: 'docs/modified.txt', origPath: null },
        { x: '?', y: '?', path: 'libs/sub', origPath: null },
        { x: 'M', y: ' ', path: 'libs/clean', origPath: null },
      ],
    });
    const { container, onOpenDiff } = renderPanel();
    await waitFor(() => expect(container.textContent).toContain('modified.txt'));
    // G1：submodule 条目不作为变更行渲染
    expect(container.textContent).not.toContain('libs/clean');
    const listText = container.querySelector('.git-files')?.textContent ?? '';
    expect(listText).toContain('modified.txt');
    expect(listText).not.toContain('libs/sub');
    // 计数不含子仓（变更 1：仅普通文件；概要同步 · 1——meta 行已删，概要唯一入口）
    const changesHead = [...container.querySelectorAll('.git-section-head')].find((h) =>
      h.textContent?.includes('变更'),
    ) as HTMLElement;
    expect(changesHead.textContent).toContain('· 1');
    // G3：子仓聚合分区仍在（各自分区展示子仓状态，折叠框架不变）
    expect(await screen.findByTitle('libs/sub')).toBeTruthy();
    // F2 零回归：普通文件进 diff 联动（onOpenDiff 带 cwd+file）
    fireEvent.click(screen.getByText('modified.txt'));
    expect(onOpenDiff).toHaveBeenCalledWith({ cwd: CWD, file: 'docs/modified.txt', cached: false });
  });
});

describe('GitPanel 行组件统一（git-panel-row-unify U1-U3）', () => {
  it('U3 硬口径收敛单一来源：.git-file-row 内建 nowrap/左锚/max-content，容器横滚三处，旧三套行类删除（源级守卫）', () => {
    const css = readFileSync('src/components/GitPanel.css', 'utf-8');
    // 统一文件行类（FileRow 单一来源，收编原 .git-file/.git-tree-file/.git-commit-file 三套）
    const rowBlock = css.slice(css.indexOf('.git-file-row {'), css.indexOf('.git-file-row:hover'));
    expect(rowBlock).toContain('white-space: nowrap');
    expect(rowBlock).toContain('width: max-content');
    expect(rowBlock).toContain('min-width: 100%');
    expect(rowBlock).toContain('justify-content: flex-start');
    // 容器横滚三处（平铺/清单/树）
    const filesBlock = css.slice(
      css.indexOf('.git-files {'),
      css.indexOf('}', css.indexOf('.git-files {')),
    );
    expect(filesBlock).toContain('overflow-x: auto');
    const listBlock = css.slice(
      css.indexOf('.git-commit-files {'),
      css.indexOf('.git-commit-files-title'),
    );
    expect(listBlock).toContain('overflow-x: auto');
    const treeContainer = css.slice(css.indexOf('.git-tree {'), css.indexOf('}', css.indexOf('.git-tree {')));
    expect(treeContainer).toContain('overflow-x: auto');
    // Timeline 行硬口径（TimelineRow 单一来源）
    const commitBlock = css.slice(css.indexOf('.git-commit {'), css.indexOf('.git-commit:hover'));
    expect(commitBlock).toContain('justify-content: flex-start');
    expect(commitBlock).toContain('white-space: nowrap');
    // 旧三套行类已删除（收敛后无消费方——防样式漂移复活）
    expect(css).not.toContain('.git-file {');
    expect(css).not.toContain('.git-tree-file {');
    expect(css).not.toContain('.git-commit-file {');
    // 目录行左锚（TreeRows 目录行保留独立类）
    const dirBlock = css.slice(css.indexOf('.git-tree-dir {'), css.indexOf('.git-tree-dir:hover'));
    expect(dirBlock).toContain('justify-content: flex-start');
  });

  it('U1 主/子仓 Timeline DOM 同源（同一 TimelineRow 组件渲染，同数据下 outerHTML 一致）', async () => {
    submodulesMock.mockResolvedValue(SUBS);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    const { container } = renderPanel();
    await waitFor(() => expect(container.querySelector('.git-timeline')).not.toBeNull());
    // 展开子仓分区（缺省收起）→ 子仓 Timeline 挂载
    fireEvent.click(await screen.findByTitle('libs/sub'));
    await waitFor(() => expect(container.querySelectorAll('.git-timeline')).toHaveLength(2));
    // 同一 TimelineRow 组件 + 同一 LOG 数据 → 首行 DOM 结构同源（视觉一致性根）
    const firstRows = [...container.querySelectorAll('.git-timeline')].map(
      (t) => t.querySelector('.git-commit')?.outerHTML ?? '',
    );
    expect(firstRows[0]).toBe(firstRows[1]);
  });
});

describe('GitPanel 提交文件联动（git-timeline-files F1-F4）', () => {
  const setupTimeline = async () => {
    submodulesMock.mockResolvedValue([]);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockResolvedValue(STATUS);
    commitFilesMock.mockResolvedValue(CFILES);
    const handle = renderPanel();
    await waitFor(() => expect(handle.container.querySelector('.git-commit')).not.toBeNull());
    return handle;
  };

  it('F1 点提交 → 展开文件维变更清单（名+状态字母+改名原路径），行高亮选中态', async () => {
    const { container } = await setupTimeline();
    fireEvent.click(screen.getByText('latest commit'));
    await waitFor(() => {
      expect(commitFilesMock).toHaveBeenCalledWith(CWD, 'aaa1111', 200);
    });
    await waitFor(() => {
      expect(container.querySelector('.git-commit-files')).not.toBeNull();
    });
    expect(container.textContent).toContain('提交文件 2');
    expect(container.textContent).toContain('app.ts');
    expect(container.textContent).toContain('new.ts');
    // 改名条目 title 承载 原路径 → 新路径
    expect(screen.getByTitle('src/old.ts → src/new.ts')).not.toBeNull();
    // 选中态高亮在提交行上
    const row = screen.getByText('latest commit').closest('button') as HTMLElement;
    expect(row.className).toContain('is-selected');
  });

  it('F2 清单选文件 → onOpenDiff 带 hash 上报（提交级 diff 归属提交仓）', async () => {
    const { container, onOpenDiff } = await setupTimeline();
    fireEvent.click(screen.getByText('latest commit'));
    await waitFor(() => expect(container.querySelector('.git-commit-files .git-file-row')).not.toBeNull());
    fireEvent.click(screen.getByTitle('src/app.ts'));
    expect(onOpenDiff).toHaveBeenCalledWith({ cwd: CWD, file: 'src/app.ts', cached: false, hash: 'aaa1111' });
  });

  it('再点同条提交 → 清单收起（选中态解除）', async () => {
    const { container } = await setupTimeline();
    fireEvent.click(screen.getByText('latest commit'));
    await waitFor(() => expect(container.querySelector('.git-commit-files')).not.toBeNull());
    fireEvent.click(screen.getByText('latest commit'));
    await waitFor(() => expect(container.querySelector('.git-commit-files')).toBeNull());
  });

  it('F4 hasMore → 清单「加载更多」limit 递增 200 重取', async () => {
    const { container } = await setupTimeline();
    commitFilesMock.mockResolvedValue({ ...CFILES, hasMore: true });
    fireEvent.click(screen.getByText('latest commit'));
    await waitFor(() => expect(container.querySelector('.git-commit-files .git-file-row')).not.toBeNull());
    const filesBlock = container.querySelector('.git-commit-files') as HTMLElement;
    fireEvent.click(filesBlock.querySelector('.git-load-more') as HTMLButtonElement);
    await waitFor(() => {
      expect(commitFilesMock).toHaveBeenCalledWith(CWD, 'aaa1111', 400);
    });
  });

  it('F3 子仓 Timeline 的提交 → commitFiles/onOpenDiff 均归属子仓路径（不串仓）', async () => {
    submodulesMock.mockResolvedValue(SUBS);
    gitLogMock.mockResolvedValue(LOG);
    gitStatusMock.mockImplementation(() => Promise.resolve(STATUS));
    commitFilesMock.mockResolvedValue(CFILES);
    const { onOpenDiff } = renderPanel();
    // 展开子仓分区（懒加载：收起不 mount Timeline）
    fireEvent.click(await screen.findByTitle('libs/sub'));
    const subSection = (await screen.findByTitle('libs/sub')).closest('.git-section') as HTMLElement;
    await waitFor(() => expect(subSection.querySelector('.git-commit')).not.toBeNull());
    // 点子仓提交行 → 以子仓路径取清单
    fireEvent.click(subSection.querySelector('.git-commit') as HTMLElement);
    await waitFor(() => {
      expect(commitFilesMock).toHaveBeenCalledWith(`${CWD}/libs/sub`, 'aaa1111', 200);
    });
    await waitFor(() => expect(subSection.querySelector('.git-commit-files .git-file-row')).not.toBeNull());
    // 选文件 → onOpenDiff cwd = 子仓路径
    fireEvent.click(subSection.querySelector('.git-commit-files .git-file-row') as HTMLElement);
    expect(onOpenDiff).toHaveBeenCalledWith({
      cwd: `${CWD}/libs/sub`,
      file: 'src/app.ts',
      cached: false,
      hash: 'aaa1111',
    });
  });
});

describe('statusKind（状态字母映射）', () => {
  const entry = (path: string, mark: string, cached = false, isSub = false): FileEntry => ({
    path,
    mark,
    cached,
    isSub,
    origPath: null,
  });

  it('?/A→U、D→D、子模块→S、其余→M', () => {
    expect(statusKind('?', false)).toBe('U');
    expect(statusKind('A', false)).toBe('U');
    expect(statusKind('D', false)).toBe('D');
    expect(statusKind('M', true)).toBe('S');
    expect(statusKind('M', false)).toBe('M');
  });

  it('markOf：cached 取 x；否则 y 优先、空回退 x', () => {
    expect(markOf({ x: 'A', y: ' ' }, true)).toBe('A');
    expect(markOf({ x: 'M', y: 'D' }, false)).toBe('D');
    expect(markOf({ x: 'M', y: ' ' }, false)).toBe('M');
  });

  it('buildFileTree：目录排文件前、聚合计数与状态字母集', () => {
    const tree = buildFileTree([
      entry('b.txt', 'M'),
      entry('a/c.txt', '?'),
      entry('a/d.txt', 'D'),
    ]);
    expect(tree[0].kind === 'dir' && tree[0].name).toBe('a');
    const dir = tree[0];
    if (dir.kind === 'dir') {
      expect(dir.count).toBe(2);
      expect(dir.kinds.sort()).toEqual(['D', 'U']);
    } else {
      throw new Error('expected dir node');
    }
    expect(tree[tree.length - 1].kind).toBe('file');
  });

  it('同路径去重（staged/unstaged 双列首见胜出）', () => {
    const tree = buildFileTree([entry('x.txt', 'A'), entry('x.txt', 'M')]);
    const files = tree.filter((n) => n.kind === 'file');
    expect(files.length).toBe(1);
  });

  it('缺省平铺；save 后 load 读回', () => {
    expect(loadGitView()).toBe('flat');
    saveGitView('tree');
    expect(loadGitView()).toBe('tree');
    localStorage.removeItem('hub_git_view');
  });
});
