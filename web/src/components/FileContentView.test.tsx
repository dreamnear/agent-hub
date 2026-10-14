// @vitest-environment happy-dom
// 文件内容渲染组件（agent-hub-files-panel F2）：docsFile 拉取 → 工厂分派全类型 +
// 加载/错误态。原 DocViewer.test 渲染分派用例的能力承接（文档 Tab 退役对账迁移）；
// 工厂本身的分派规则由 lib/docRender.test.tsx 单测，此处只锁「组件接线」。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, waitFor } from '@testing-library/react';
import FileContentView from './FileContentView';
import type { DocFile } from '../types';

const docsFileMock = vi.fn();

vi.mock('../api', () => ({
  api: {
    docsFile: (path: string) => docsFileMock(path),
    docsRawUrl: (path: string) => `/api/docs/raw?path=${encodeURIComponent(path)}`,
  },
}));

function renderView(path = '/repo/README.md'): ReturnType<typeof render> {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } }, // 错误态直达（默认 3 次退避重试拖垮断言）
  });
  return render(
    <QueryClientProvider client={qc}>
      <FileContentView path={path} />
    </QueryClientProvider>,
  );
}

const fileOf = (over: Partial<DocFile>): DocFile => ({
  kind: 'text',
  name: 'f.txt',
  content: 'x',
  size: 1,
  ...over,
});

afterEach(() => {
  cleanup();
  docsFileMock.mockReset();
});

describe('FileContentView（工厂分派接线，文档 Tab 退役迁移账）', () => {
  it('markdown/text/image/pdf 四类 kind 各自分派到对应渲染器', async () => {
    for (const [kind, cls] of [
      ['markdown', 'doc-md'],
      ['text', 'doc-text'],
      ['image', 'doc-image'],
      ['pdf', 'doc-pdf'],
    ] as const) {
      docsFileMock.mockResolvedValue(fileOf({ kind, name: `f.${kind}` }));
      const { container, unmount } = renderView();
      // kind=${kind} 应渲染 .${cls}
      await waitFor(() => expect(container.querySelector(`.${cls}`)).not.toBeNull());
      unmount();
    }
  });

  it('binary：空态提示含文件名（原「友好空态」用例迁移）', async () => {
    docsFileMock.mockResolvedValue(fileOf({ kind: 'binary', name: 'asset.bin', content: null }));
    const { container } = renderView('/repo/asset.bin');
    await waitFor(() => expect(container.querySelector('.doc-binary')).not.toBeNull());
    expect(container.textContent).toContain('asset.bin');
  });

  it('加载态与错误态：loading 提示 → 失败落 doc-hint--err 文案', async () => {
    docsFileMock.mockReturnValue(new Promise<DocFile>(() => {})); // 永不落定 = loading
    const { container } = renderView();
    expect(container.querySelector('.doc-hint')).not.toBeNull();
    // 错误：reject 后查询进 error 态
    docsFileMock.mockRejectedValue(new Error('boom-403'));
    const { container: c2, unmount } = renderView();
    await waitFor(() => expect(c2.querySelector('.doc-hint--err')).not.toBeNull());
    expect(c2.textContent).toContain('boom-403');
    unmount();
  });
});
