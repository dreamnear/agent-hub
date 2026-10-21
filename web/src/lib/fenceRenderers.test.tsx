import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import {
  registerFenceRenderer,
  unregisterPluginRenderers,
  resolveFenceRenderer,
  PlantumlServerBlock,
  HtmlSanitizedBlock,
  type FenceRendererDef,
} from './fenceRenderers';


describe('fenceRenderers 注册表语义', () => {
  it('内置围栏可解析（plantuml/mermaid/html/image）', () => {
    expect(resolveFenceRenderer('plantuml')?.id).toBe('builtin:plantuml');
    expect(resolveFenceRenderer('Mermaid')?.id).toBe('builtin:mermaid');
    expect(resolveFenceRenderer('html')?.id).toBe('builtin:html-sanitized');
    expect(resolveFenceRenderer('image')?.id).toBe('builtin:image');
    expect(resolveFenceRenderer('plaincode')).toBeNull();
  });

  it('插件声明注册：后注册先命中；unregister 撤销全部 plug_（BR-03 同现同隐）', () => {
    const def: FenceRendererDef = {
      id: 'plug_demo_gantt',
      fences: ['gantt'],
      component: ({ code }) => <div data-test="gantt">{code}</div>,
    };
    registerFenceRenderer(def);
    expect(resolveFenceRenderer('gantt')?.id).toBe('plug_demo_gantt');
    unregisterPluginRenderers();
    expect(resolveFenceRenderer('gantt')).toBeNull();
    // 内置不受注销影响
    expect(resolveFenceRenderer('mermaid')?.id).toBe('builtin:mermaid');
  });
});

describe('内置引擎回退语义（不假渲染）', () => {
  it('plantuml 未配置服务 → 回退源码 + 引导提示', () => {
    const { container } = render(
      <PlantumlServerBlock code={'@startuml\nA->B\n@enduml'} lang="plantuml" />,
    );
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('.md-fence-hint')).not.toBeNull();
  });

  it('html-sanitized：sandbox iframe 静态渲染（脚本不执行面）', () => {
    const { container } = render(
      <HtmlSanitizedBlock code="<b>x</b><script>alert(1)</script>" lang="html" />,
    );
    const iframe = container.querySelector('iframe');
    expect(iframe?.getAttribute('sandbox')).toBe('');
  });
});
