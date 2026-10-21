import { useEffect, useState } from 'react';
import { plugins } from '../plugins';
import { registerFenceRenderer, unregisterPluginRenderers, componentForEngine } from './fenceRenderers';
import { registerDocRenderer } from './docRender';

/// 插件渲染器声明 → 围栏注册表转译（B3，ADR-0012 声明档）。
/// App 级挂一次：轮询插件清单（启停即生效，BR-03），把 enabled 插件的
/// renderers 声明逐条注册为「声明 → 内置引擎组件」；插件零代码进宿主。
/// plugin-iframe 档声明暂不注册（回退源码，不假渲染）——随批 1 尾/批 2 接管。

interface RendererDeclView {
  id: string;
  matchFence: string[];
  matchExt: string[];
  engine: string;
  service?: string;
  entry?: string;
}

interface PluginLite {
  id: string;
  enabled: boolean;
  uiState: string;
  renderers?: RendererDeclView[];
}

export function usePluginRenderers(): void {
  const [sig, setSig] = useState('');
  useEffect(() => {
    let alive = true;
    const fetch = (): void => {
      plugins
        .list()
        .then((list) => {
          if (!alive) return;
          const pluginList = list as PluginLite[];
          const enabled = pluginList.filter(
            (p) => p.enabled && p.uiState === 'enabled' && (p.renderers?.length ?? 0) > 0,
          );
          const next = enabled
            .flatMap((p) => (p.renderers ?? []).map((r) => `${r.id}:${r.engine}`))
            .sort()
            .join('|');
          setSig((prev) => (prev === next ? prev : next));
        })
        .catch(() => {});
    };
    fetch();
    const timer = setInterval(fetch, 30_000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);
  useEffect(() => {
    let alive = true;
    // 声明集变化即整体重注册（停用/卸载插件的声明随之消失，BR-03 同现同隐）
    unregisterPluginRenderers();
    if (sig === '') return () => { alive = false; };
    void plugins
      .list()
      .then((list) => {
        if (!alive) return;
        const pluginList = list as PluginLite[];
        for (const p of pluginList) {
          if (!p.enabled || p.uiState !== 'enabled') continue;
          for (const r of p.renderers ?? []) {
            // 服务端已保证 fence/ext 不抢内置（审计失败拒装）；此处纯加法注册
            const comp = componentForEngine(r.engine, { service: r.service, source: p.id, entry: r.entry });
            if (comp == null) continue; // 未知引擎：回退源码，不假渲染
            if (r.matchFence.length > 0) {
              registerFenceRenderer({ id: r.id, fences: r.matchFence, component: comp });
            }
            // ext 声明 → docs 文件预览注册表（DocRenderProps 适配：file.content 作源）
            if (r.matchExt.length > 0) {
              const exts = r.matchExt;
              registerDocRenderer({
                id: r.id,
                match: (kind, name) =>
                  (kind === 'binary' || kind === 'text') &&
                  exts.some((e) => name.toLowerCase().endsWith(`.${e.replace(/^\./, '')}`)),
                component: ({ file }) => {
                  const Fenced = comp;
                  return (
                    <div className="doc-md">
                      <Fenced code={file.content ?? ''} lang={exts[0] ?? ''} />
                    </div>
                  );
                },
              });
            }
          }
        }
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [sig]);
}
