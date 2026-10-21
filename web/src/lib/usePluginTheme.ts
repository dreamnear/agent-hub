import { useEffect } from 'react';
import { plugins } from '../plugins';

const STYLE_ID = 'hub-theme-override';

/** 主题 token 覆盖（B3，ADR-0013 覆盖档）：enabled 插件中若有一个声明
 * themeTokens → 注入 :root + [data-theme=dark] 覆写段（仅被覆盖 token）；
 * 停用/卸载 → 整段移除（恢复内置值）。同刻仅一个生效由服务端互斥保证，
 * 前端只消费一个（有即注，无即清空——无第二优先级逻辑）。 */
export function usePluginTheme(): void {
  useEffect(() => {
    let alive = true;
    const apply = (): void => {
      if (!alive) return;
      void plugins
        .list()
        .then((list) => {
          if (!alive) return;
          const theme = (list as Array<{
            enabled: boolean;
            uiState: string;
            themeTokens?: { tokens: Record<string, { light: string; dark: string }> };
          }>).find((p) => p.enabled && p.uiState === 'enabled' && p.themeTokens != null)?.themeTokens;
          let el = document.getElementById(STYLE_ID) as HTMLStyleElement | null;
          if (theme == null) {
            el?.remove();
            return;
          }
          const light: string[] = [];
          const dark: string[] = [];
          for (const [token, v] of Object.entries(theme.tokens)) {
            light.push(`${token}: ${v.light};`);
            dark.push(`${token}: ${v.dark};`);
          }
          const css = `:root{${light.join('')}}\n[data-theme='dark']{${dark.join('')}}`;
          if (el == null) {
            el = document.createElement('style');
            el.id = STYLE_ID;
            document.head.appendChild(el);
          }
          el.textContent = css;
        })
        .catch(() => {});
    };
    apply();
    const timer = setInterval(apply, 10_000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);
}
