import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';
import { plugins } from '../plugins';
import type { FileDecorationView } from '../plugins';

interface DecoEntry {
  pluginId: string;
  icon: string;
  color?: FileDecorationView['color'];
}

const Ctx = createContext<Map<string, DecoEntry>>(new Map());

function buildMap(
  list: Array<{ id: string; enabled: boolean; uiState: string; fileDecorations?: FileDecorationView[] }>,
): Map<string, DecoEntry> {
  const m = new Map<string, DecoEntry>();
  for (const p of list) {
    if (!p.enabled || p.uiState !== 'enabled') continue;
    for (const d of p.fileDecorations ?? []) {
      for (const e of d.matchExt ?? []) {
        const key = e.replace(/^\./, '').toLowerCase();
        if (!m.has(key)) m.set(key, { pluginId: p.id, icon: d.icon, color: d.color as FileDecorationView['color'] });
      }
    }
  }
  return m;
}

export function FileDecoProvider({ children }: { children: ReactNode }): ReactNode {
  const [deco, setDeco] = useState<Map<string, DecoEntry>>(() => new Map());
  useEffect(() => {
    let alive = true;
    const fetch = (): void => {
      void plugins
        .list()
        .then((list) => {
          if (!alive) return;
          setDeco(buildMap(list as Array<{ id: string; enabled: boolean; uiState: string; fileDecorations?: FileDecorationView[] }>));
        })
        .catch(() => {});
    };
    fetch();
    const timer = setInterval(fetch, 10_000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);
  return <Ctx.Provider value={deco}>{children}</Ctx.Provider>;
}

export function decoExt(name: string): string {
  const base = (name.split('/').pop() ?? name).toLowerCase();
  const dot = base.lastIndexOf('.');
  return dot >= 0 ? base.slice(dot + 1) : '';
}

export function useDeco(ext: string): DecoEntry | null {
  return useContext(Ctx).get(ext) ?? null;
}
