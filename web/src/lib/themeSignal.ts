import { useEffect, useState } from 'react';

/** 当前主题态（data-theme），属性翻转时通知订阅者——渲染引擎重渲染用（B3）。
 *  独立小模块：fenceRenderers 与 iframeRenderer 共用（避免循环 import）。 */
export function useThemeSignal(): string {
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme ?? 'auto');
  useEffect(() => {
    const el = document.documentElement;
    const ob = new MutationObserver(() => setTheme(el.dataset.theme ?? 'auto'));
    ob.observe(el, { attributes: true, attributeFilter: ['data-theme'] });
    return () => ob.disconnect();
  }, []);
  return theme;
}
