// 插件前端纯逻辑（agent-hub-plugin-system B1）：导航派生（BR-03/06/07 的 UI 侧
// 同现同隐）+ 视图资产 URL（票据门禁形态）。
import { describe, expect, it } from 'vitest';
import { navItemsOf, unwrapRpcEnvelope, viewAssetUrl, type PluginView } from './plugins';

function pv(over: Partial<PluginView>): PluginView {
  return {
    id: 'demo-plugin',
    name: 'Demo',
    version: '0.1.0',
    source: { kind: 'drop-in', ref: null },
    enabled: true,
    state: 'ready',
    uiState: 'enabled',
    installedAt: '2026-10-19T00:00:00Z',
    contentHash: 'sha256:x',
    contributions: [],
    permissions: { api: [], data: [], credentials: [], highRisk: [] },
    views: [{ id: 'home', title: 'Home', order: 1 }],
    backend: { running: false, alwaysOn: false, idleTimeoutSecs: 300 },
    assetBytes: 0,
    ...over,
  };
}

describe('navItemsOf（BR-03 同现同隐 / BR-07 故障徽标）', () => {
  it('停用/卸载后入口消失：disabled 插件不产生导航条目', () => {
    expect(navItemsOf([pv({ uiState: 'disabled', enabled: false })])).toEqual([]);
    expect(navItemsOf([])).toEqual([]);
  });

  it('启用且有视图贡献 → 出现入口；多视图按 order 稳定排序', () => {
    const items = navItemsOf([
      pv({
        id: 'b',
        views: [
          { id: 'z', title: 'Z', order: 2 },
          { id: 'a', title: 'A', order: 1 },
        ],
      }),
      pv({ id: 'a-early', name: 'Early', views: [{ id: 'home', title: 'Home', order: 1 }] }),
    ]);
    expect(items.map((i) => `${i.pluginId}:${i.viewId}`)).toEqual([
      'a-early:home',
      'b:a',
      'b:z',
    ]);
  });

  it('故障态保留入口并带徽标与原因（故障可观察，不隐藏）', () => {
    const items = navItemsOf([
      pv({
        uiState: 'fault',
        state: 'fault',
        lastFault: { at: '2026-10-19T00:00:00Z', kind: 'crash', message: '崩溃：backend 已退出' },
      }),
    ]);
    expect(items).toHaveLength(1);
    expect(items[0].fault).toBe(true);
    expect(items[0].faultMessage).toContain('崩溃');
  });

  it('无视图贡献的插件不进导航组', () => {
    expect(navItemsOf([pv({ views: [] })])).toEqual([]);
  });
});

describe('viewAssetUrl（票据路径段形态，tester 1 轮 FAIL-1 修复）', () => {
  it('票据入路径段：相对引用由浏览器按文档目录解析时自动继承（不依赖 cookie/query）', () => {
    expect(viewAssetUrl('demo-plugin', 'index.html', 'tk')).toBe(
      '/plugin-view/demo-plugin/~tk/index.html',
    );
  });

  it('空路径回落 index.html；相对路径不带前导斜杠', () => {
    expect(viewAssetUrl('p', '', 'tk')).toBe('/plugin-view/p/~tk/index.html');
    expect(viewAssetUrl('p', '/app.js', 'tk')).toBe('/plugin-view/p/~tk/app.js');
  });

  it('票据与插件 id 做 URL 编码（防注入）', () => {
    expect(viewAssetUrl('a b', 'x.js', 'a&b')).toBe(
      '/plugin-view/a%20b/~a%26b/x.js',
    );
  });

  it('子目录引用仍落在票据段之下（浏览器相对解析语义）', () => {
    const url = new URL(viewAssetUrl('p', 'index.html', 'tk'), 'http://x');
    expect(new URL('assets/app.js', url).pathname).toBe('/plugin-view/p/~tk/assets/app.js');
  });
});

describe('unwrapRpcEnvelope（tester 2 轮 FAIL-2：宿主桥信封翻译）', () => {
  it('成功信封 → 解出业务结果（视图契约读 res.content，不读信封）', () => {
    const out = unwrapRpcEnvelope({ ok: true, result: { content: [{ type: 'text', text: 'x' }] } });
    expect(out).toEqual({ ok: true, result: { content: [{ type: 'text', text: 'x' }] } });
  });

  it('错误信封 → rpc-error 载荷（不得当成功结果下发）', () => {
    const out = unwrapRpcEnvelope({ ok: false, code: 'violation', message: '未声明' });
    expect(out).toEqual({ ok: false, code: 'violation', message: '未声明' });
    expect(unwrapRpcEnvelope({ ok: false })).toEqual({
      ok: false,
      code: 'plugin_error',
      message: '',
    });
  });

  it('非信封（测试假实现的裸业务结果）→ 原样作为成功结果', () => {
    expect(unwrapRpcEnvelope({ content: [] })).toEqual({ ok: true, result: { content: [] } });
    expect(unwrapRpcEnvelope(null)).toEqual({ ok: true, result: null });
  });
});
