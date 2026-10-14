// @vitest-environment happy-dom
// docs 预览 type→renderer 工厂：内置命中、插件后注册先命中（.puml 截获 text）、
// 同 id 覆盖语义。
import type { ReactElement } from 'react';
import { describe, expect, it } from 'vitest';
import { registerDocRenderer, resolveDocRenderer } from './docRender';
// 插件模块副作用注册（.puml）——与 DocViewer 同一入口
import './docRenderPlugins';

describe('resolveDocRenderer', () => {
  it('内置六类命中', () => {
    expect(resolveDocRenderer('markdown', 'a.md')?.id).toBe('builtin-markdown');
    expect(resolveDocRenderer('html', 'a.html')?.id).toBe('builtin-html');
    expect(resolveDocRenderer('image', 'a.png')?.id).toBe('builtin-image');
    expect(resolveDocRenderer('pdf', 'a.pdf')?.id).toBe('builtin-pdf');
    expect(resolveDocRenderer('text', 'a.txt')?.id).toBe('builtin-text');
  });

  it('binary 兜底恒有渲染器（未知 kind 不白屏）', () => {
    expect(resolveDocRenderer('binary', 'a.bin')?.id).toBe('builtin-binary');
    expect(resolveDocRenderer('whatever', 'a.weird')?.id).toBe('builtin-binary');
  });

  it('插件后注册先命中：.puml 在 builtin-text 之前截获', () => {
    expect(resolveDocRenderer('text', 'flow.puml')?.id).toBe('plugin-plantuml-file');
    // 非 .puml 的 text 不受影响
    expect(resolveDocRenderer('text', 'a.txt')?.id).toBe('builtin-text');
  });

  it('同 id 再注册 = 覆盖（插件升级缝）', () => {
    const spy = (): ReactElement => <span data-testid="spy" />;
    registerDocRenderer({
      id: 'test-override',
      match: (k) => k === 'text',
      component: spy,
    });
    expect(resolveDocRenderer('text', 'a.txt')?.id).toBe('test-override');
    registerDocRenderer({
      id: 'test-override',
      match: (k) => k === 'never',
      component: spy,
    });
    // 覆盖后原 match 失效，回到 builtin-text
    expect(resolveDocRenderer('text', 'a.txt')?.id).toBe('builtin-text');
  });
});
