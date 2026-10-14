// @vitest-environment happy-dom
// GitLab 式左右对照 diff：行对齐矩阵（context/纯增/纯删/混合 hunk/元信息）+ 渲染断言。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import GitDiffView, { parseSplitDiff } from './GitDiffView';
import type { Api } from '../api';

// vitest 默认不加载 CSS（布局断言不可得），CSS 消费端守卫走源级直读（runner 根 = web/）
const diffCss = readFileSync('src/components/GitDiffView.css', 'utf-8');

const gitDiffMock = vi.fn();
const commitDiffMock = vi.fn();
const apiStub = {
  gitDiff: (path: string, file: string, cached: boolean) => gitDiffMock(path, file, cached),
  commitDiff: (path: string, hash: string, file: string) => commitDiffMock(path, hash, file),
} as unknown as Api;

/** 标准 mixed hunk：context / 纯删 / 纯增 / context（起点 old=10 new=20） */
const MIXED = [
  'diff --git a/f b/f',
  'index 111..222 100644',
  '--- a/f',
  '+++ b/f',
  '@@ -10,3 +20,3 @@ fn main()',
  ' ctx line',
  '-old line',
  '+new line',
  ' tail line',
].join('\n');

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const Panel = ({ children }: { children: React.ReactNode }): React.ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

describe('parseSplitDiff（行对齐矩阵）', () => {
  it('context：左右同码同 ctx 类，行号按 hunk 头各自起点推进', () => {
    const rows = parseSplitDiff('@@ -10,1 +20,1 @@\n ctx');
    expect(rows[0]).toEqual({ kind: 'hunk', text: '@@ -10,1 +20,1 @@' });
    expect(rows[1]).toEqual({
      kind: 'pair',
      left: { no: 10, text: 'ctx', kind: 'ctx' },
      right: { no: 20, text: 'ctx', kind: 'ctx' },
    });
  });

  it('纯增：右格 add、左格空占位（no=null），newNo 推进 oldNo 不动', () => {
    const rows = parseSplitDiff('@@ -1,0 +2,1 @@\n+added\n+more');
    expect(rows[1]).toEqual({
      kind: 'pair',
      left: { no: null, text: '', kind: 'empty' },
      right: { no: 2, text: 'added', kind: 'add' },
    });
    expect(rows[2]?.kind === 'pair' && rows[2].right.no).toBe(3);
  });

  it('纯删：左格 del、右格空占位，oldNo 推进 newNo 不动', () => {
    const rows = parseSplitDiff('@@ -5,2 +1,0 @@\n-gone\n-also gone');
    expect(rows[1]).toEqual({
      kind: 'pair',
      left: { no: 5, text: 'gone', kind: 'del' },
      right: { no: null, text: '', kind: 'empty' },
    });
    expect(rows[2]?.kind === 'pair' && rows[2].left.no).toBe(6);
  });

  it('混合 hunk：矩阵序 = hunk → ctx → del → add → ctx，行号交叉推进正确', () => {
    const rows = parseSplitDiff(MIXED);
    expect(rows).toHaveLength(5);
    expect(rows[0]?.kind).toBe('hunk');
    // ctx：左 10 / 右 20
    expect(rows[1]?.kind === 'pair' && rows[1].left).toMatchObject({ no: 10, kind: 'ctx' });
    expect(rows[1]?.kind === 'pair' && rows[1].right).toMatchObject({ no: 20, kind: 'ctx' });
    // del：左 11，右空
    expect(rows[2]?.kind === 'pair' && rows[2].left).toMatchObject({ no: 11, text: 'old line', kind: 'del' });
    expect(rows[2]?.kind === 'pair' && rows[2].right).toMatchObject({ no: null, kind: 'empty' });
    // add：右 21，左空
    expect(rows[3]?.kind === 'pair' && rows[3].left).toMatchObject({ no: null, kind: 'empty' });
    expect(rows[3]?.kind === 'pair' && rows[3].right).toMatchObject({ no: 21, text: 'new line', kind: 'add' });
    // tail ctx：左 12 / 右 22
    expect(rows[4]?.kind === 'pair' && rows[4].left).toMatchObject({ no: 12, kind: 'ctx' });
    expect(rows[4]?.kind === 'pair' && rows[4].right).toMatchObject({ no: 22, kind: 'ctx' });
  });

  it('元信息：---/+++/index/diff --git 不渲染；\\ No newline 成 meta 行', () => {
    const rows = parseSplitDiff(MIXED);
    expect(rows.some((r) => r.kind !== 'pair' && r.text.startsWith('diff --git'))).toBe(false);
    expect(rows.some((r) => r.kind !== 'pair' && r.text.startsWith('---'))).toBe(false);
    const withNoNl = parseSplitDiff('@@ -1,1 +1,1 @@\n-a\\ No newline\n\\ No newline at end of file');
    expect(withNoNl.at(-1)).toEqual({ kind: 'meta', text: '\\ No newline at end of file' });
  });

  it('空 diff → 空矩阵；畸形 hunk 头行号未知 → no 落 null', () => {
    expect(parseSplitDiff('')).toEqual([]);
    const rows = parseSplitDiff('@@ broken @@\n x');
    expect(rows[1]?.kind === 'pair' && rows[1].left.no).toBeNull();
  });
});

describe('GitDiffView（GitLab split 渲染）', () => {
  it('双栏网格：每对 4 格（双列号+双码格），hunk 带横跨，着色类随矩阵', async () => {
    gitDiffMock.mockResolvedValue({ diff: MIXED });
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={apiStub} file={{ file: 'a.txt', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.querySelector('.git-diff-hunk')).not.toBeNull());
    const cells = [...container.querySelectorAll('.git-diff-cell')];
    // 4 对 × 2 码格
    expect(cells).toHaveLength(8);
    expect(cells[0].className).toContain('is-ctx');
    expect(cells[1].className).toContain('is-ctx');
    expect(cells[2].className).toContain('is-del'); // 左 old line
    expect(cells[3].className).toContain('is-empty'); // 右空占位
    expect(cells[4].className).toContain('is-empty');
    expect(cells[5].className).toContain('is-add'); // 右 new line
    // hunk 带内容与横跨类
    expect(container.querySelector('.git-diff-hunk')?.textContent).toContain('@@ -10,3 +20,3 @@');
    // 列号灰列存在
    expect(container.querySelectorAll('.git-diff-no').length).toBe(8);
  });

  it('标题/关闭钮/ESC/空 diff 占位照旧', async () => {
    gitDiffMock.mockResolvedValue({ diff: '' });
    const onClose = vi.fn();
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={apiStub} file={{ file: 'a.txt', cached: true }} onClose={onClose} />
      </Panel>,
    );
    await waitFor(() => expect(container.textContent).toContain('（无差异）'));
    expect(container.querySelector('.git-diff-overlay-title')?.textContent).toContain('a.txt');
    fireEvent.click(container.querySelector('.git-diff-overlay-head button') as HTMLButtonElement);
    expect(onClose).toHaveBeenCalledOnce();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it('查询按 cwd+file+cached 走传入 api', async () => {
    gitDiffMock.mockResolvedValue({ diff: MIXED });
    render(
      <Panel>
        <GitDiffView cwd="/repo" api={apiStub} file={{ file: 'a.txt', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(gitDiffMock).toHaveBeenCalledWith('/repo', 'a.txt', false));
  });

  it('git-timeline-files：hash 存在 → commitDiff 取提交内 diff（gitDiff 不触发），标题带提交前缀', async () => {
    commitDiffMock.mockResolvedValue({ diff: MIXED, untracked: false, binary: false });
    const { container } = render(
      <Panel>
        <GitDiffView
          cwd="/repo"
          api={apiStub}
          file={{ file: 'a.txt', cached: false, hash: 'abc1234' }}
          onClose={() => {}}
        />
      </Panel>,
    );
    await waitFor(() => expect(commitDiffMock).toHaveBeenCalledWith('/repo', 'abc1234', 'a.txt'));
    expect(gitDiffMock).not.toHaveBeenCalled();
    expect(container.querySelector('.git-diff-overlay-title')?.textContent).toContain('提交 abc1234');
    await waitFor(() => expect(container.querySelector('.git-diff-hunk')).not.toBeNull());
  });

  it('双栏各占一半视宽 + 码格折行（CSS 消费端守卫：vitest 无布局引擎，锁源级模板防回退）', () => {
    // 列模板必须 minmax(0,1fr) 双码列——曾用 max-content 致左列长行把右列挤出视口
    expect(diffCss).toContain('grid-template-columns: 3.2em minmax(0, 1fr) 3.2em minmax(0, 1fr)');
    expect(diffCss).not.toContain('max-content');
    // 码格域内折行（保持等宽字体）
    const cellBlock = diffCss.slice(diffCss.indexOf('.git-diff-cell {'), diffCss.indexOf('.git-diff-cell.is-add'));
    expect(cellBlock).toContain('white-space: pre-wrap');
    expect(cellBlock).toContain('word-break: break-all');
  });
});

// —— 热修：错误态占位（不再无限加载）/ binary 占位 / untracked 全添加渲染 ——

describe('GitDiffView 热修（加载卡死根因）', () => {
  it('请求 400/错误 → 错误占位渲染（retry 已禁，不再无限转圈）', async () => {
    const rejectMock = vi.fn(() => Promise.reject(new Error('git diff 失败')));
    const errApi = { gitDiff: () => rejectMock() } as unknown as Api;
    const onClose = vi.fn();
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={errApi} file={{ file: 'ghost.txt', cached: false }} onClose={onClose} />
      </Panel>,
    );
    await waitFor(() => expect(container.textContent).toContain('该文件无法生成 diff'));
  });

  it('binary: true 非图片 → 二进制占位（不渲染垃圾文本行）', async () => {
    const binApi = { gitDiff: () => Promise.resolve({ diff: '', untracked: false, binary: true }) } as unknown as Api;
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={binApi} file={{ file: 'data.bin', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.textContent).toContain('二进制文件，无文本 diff'));
    expect(container.querySelector('.git-diff-line')).toBeNull();
  });

  it('binary: true 图片扩展 → 左右对照双窗格（rev=head/work 两 img）', async () => {
    const binApi = { gitDiff: () => Promise.resolve({ diff: '', untracked: false, binary: true }) } as unknown as Api;
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={binApi} file={{ file: 'pic.png', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.querySelectorAll('.git-img-pane')).toHaveLength(2));
    const imgs = [...container.querySelectorAll('img')];
    expect(imgs).toHaveLength(2);
    expect(imgs[0].getAttribute('src')).toContain('rev=head');
    expect(imgs[1].getAttribute('src')).toContain('rev=work');
  });

  it('untracked 图片（无旧版）：左占位「无旧版」，右窗 rev=work 有图', async () => {
    const unApi = { gitDiff: () => Promise.resolve({ diff: '', untracked: true, binary: true }) } as unknown as Api;
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={unApi} file={{ file: 'new.png', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.querySelectorAll('.git-img-pane')).toHaveLength(2));
    // untracked → 左窗直接占位（不请求 HEAD），右窗有图
    expect(container.textContent).toContain('无旧版');
    const imgs = [...container.querySelectorAll('img')];
    expect(imgs).toHaveLength(1);
    expect(imgs[0].getAttribute('src')).toContain('rev=work');
  });

  it('untracked: true → 全添加 diff 正常渲染（右列绿、左列空）', async () => {
    const unApi = {
      gitDiff: () => Promise.resolve({ diff: 'diff --git a/f b/f\nnew file mode 100644\n@@ -0,0 +1,2 @@\n+line1\n+line2', untracked: true, binary: false }),
    } as unknown as Api;
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={unApi} file={{ file: 'new.md', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.querySelectorAll('.git-diff-cell').length).toBe(4));
    // 每对 = 左空占位格 + 右 add 格；左列号全空（新增无旧行号）
    const lefts = [...container.querySelectorAll('.git-diff-cell')].filter((_, i) => i % 2 === 0);
    const rights = [...container.querySelectorAll('.git-diff-cell')].filter((_, i) => i % 2 === 1);
    expect(lefts.every((c) => c.className.includes('is-empty'))).toBe(true);
    expect(rights.every((c) => c.className.includes('is-add'))).toBe(true);
    const nos = [...container.querySelectorAll('.git-diff-no')].map((n) => n.textContent);
    expect(nos).toEqual(['', '1', '', '2']); // 左列号空（无旧行），右列号新行序
  });

  it('deleted（D 全删 diff）→ 左 del 全文 / 右空占位（与 A/U 镜像）', async () => {
    // 后端 git diff HEAD -- 对已删除文件回全删内容；前端 split 矩阵天然左删右空
    const delApi = {
      gitDiff: () => Promise.resolve({ diff: 'diff --git a/f b/f\ndeleted file mode 100644\n@@ -1,2 +0,0 @@\n-line one\n-line two', untracked: false, binary: false }),
    } as unknown as Api;
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={delApi} file={{ file: 'gone.txt', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.querySelectorAll('.git-diff-cell').length).toBe(4));
    const lefts = [...container.querySelectorAll('.git-diff-cell')].filter((_, i) => i % 2 === 0);
    const rights = [...container.querySelectorAll('.git-diff-cell')].filter((_, i) => i % 2 === 1);
    expect(lefts.every((c) => c.className.includes('is-del'))).toBe(true);
    expect(lefts.map((c) => c.textContent)).toEqual(['line one', 'line two']);
    expect(rights.every((c) => c.className.includes('is-empty'))).toBe(true);
  });

  it('错误占位居中：body 切 is-empty-state（flex 居中），CSS 源级守卫', async () => {
    const rejectMock = vi.fn(() => Promise.reject(new Error('git diff 失败')));
    const errApi = { gitDiff: () => rejectMock() } as unknown as Api;
    const { container } = render(
      <Panel>
        <GitDiffView cwd="/repo" api={errApi} file={{ file: 'ghost.txt', cached: false }} onClose={() => {}} />
      </Panel>,
    );
    await waitFor(() => expect(container.textContent).toContain('该文件无法生成 diff'));
    const body = container.querySelector('.git-diff-overlay-body');
    expect(body?.className).toContain('is-empty-state');
    // 源级守卫（vitest 无布局引擎）：居中规则存在且作用于占位态
    expect(diffCss).toContain('.git-diff-overlay-body.is-empty-state');
    const block = diffCss.split('.git-diff-overlay-body.is-empty-state')[1] ?? '';
    expect(block).toContain('align-items: center');
    expect(block).toContain('justify-content: center');
  });
});
