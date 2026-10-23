import { describe, expect, it, vi, beforeEach, beforeAll } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/react';
import { DeclarativeIframeRenderer } from './iframeRenderer';
import { plugins } from '../plugins';

// plugins 客户端 mock：票据失败路径（回退语义）与成功路径（iframe 呈送）
vi.mock('../plugins', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../plugins')>();
  return {
    ...orig,
    plugins: {
      ...orig.plugins,
      viewTicket: vi.fn(),
      list: vi.fn().mockResolvedValue([]),
    },
  };
});

beforeAll(() => {
  // happy-dom 的 IntersectionObserver.observe 不触发回调——stub 立即 isIntersecting
  vi.stubGlobal(
    'IntersectionObserver',
    class {
      constructor(cb: IntersectionObserverCallback) {
        setTimeout(() => cb([{ isIntersecting: true } as IntersectionObserverEntry], this as unknown as IntersectionObserver), 0);
      }
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
});

describe('渲染器 iframe 档（ADR-0012 §12.2）', () => {
  beforeEach(() => {
    vi.mocked(plugins.viewTicket).mockReset();
  });

  it('票据失败 → 回退 fallback（不假渲染）', async () => {
    vi.mocked(plugins.viewTicket).mockRejectedValue(new Error('no ticket'));
    const { container } = render(
      <DeclarativeIframeRenderer
        pluginId="p1"
        entry="render/index.html"
        source="graph TD; A-->B"
        lang="gantt"
        fallback={<div data-test="fallback">源码块</div>}
      />,
    );
    await waitFor(() => {
      expect(container.querySelector('[data-test="fallback"]')).not.toBeNull();
      expect(container.querySelector('iframe')).toBeNull();
    });
  });

  it('票据成功 → 沙箱 iframe 呈送（无 allow-same-origin；脚本不进宿主上下文）', async () => {
    vi.mocked(plugins.viewTicket).mockResolvedValue({ ticket: 'tk-1', expiresInSecs: 120 });
    const { container } = render(
      <DeclarativeIframeRenderer
        pluginId="p1"
        entry="render/index.html"
        source="graph TD; A-->B"
        lang="gantt"
        fallback={<div data-test="fallback">源码块</div>}
      />,
    );
    await waitFor(() => {
      const f = container.querySelector('iframe');
      expect(f).not.toBeNull();
      expect(f?.getAttribute('sandbox')).toBe('allow-scripts'); // 不含 allow-same-origin
      expect(f?.getAttribute('src')).toContain('/plugin-view/p1/~tk-1/render/index.html');
    });
  });

  it('rendered ok:false → 回退源码 + 失败原因', async () => {
    vi.mocked(plugins.viewTicket).mockResolvedValue({ ticket: 'tk-2', expiresInSecs: 120 });
    const { container } = render(
      <DeclarativeIframeRenderer
        pluginId="p1"
        entry="render/index.html"
        source="bad syntax"
        lang="gantt"
        fallback={<div data-test="fallback">源码块</div>}
      />,
    );
    await waitFor(() => expect(container.querySelector('iframe')).not.toBeNull());
    // 模拟渲染页 rendered 失败回执（仅接受 frame 自身句柄）
    const frame = container.querySelector('iframe') as HTMLIFrameElement;
    const ev = new MessageEvent('message', {
      source: frame.contentWindow,
      data: { type: 'rendered', payload: { ok: false, reason: 'syntax error' } },
    });
    window.dispatchEvent(ev);
    await waitFor(() => {
      expect(container.querySelector('[data-test="fallback"]')).not.toBeNull();
      expect(container.textContent).toContain('syntax error');
    });
    cleanup();
  });
});
