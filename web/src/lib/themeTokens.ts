/// 主题桥（ADR-0004 第 6 条 / BR-16）：宿主 → 插件视图的 token 契约唯一来源。
///
/// 纪律：token 名单**显式维护**（不遍历 computedStyle 全量）——避免把非 `--c-*`
/// 变量也发出去，也避免三态差异带来的键集漂移。名单与 `App.css` 的 `:root` /
/// `[data-theme='dark']` 定义保持同步（新增 token 时两边一起改）。
export const THEME_TOKEN_NAMES = [
  '--c-bg',
  '--c-panel',
  '--c-bubble',
  '--c-code',
  '--c-code-border',
  '--c-surface',
  '--c-selected',
  '--c-text',
  '--c-sub',
  '--c-faint',
  '--c-inverse',
  '--c-border',
  '--c-border-input',
  '--c-accent',
  '--c-accent-strong',
  '--c-needs',
  '--c-working',
  '--c-done',
  '--c-danger',
  '--c-danger-bg',
  '--c-danger-border',
  '--c-danger-text',
  '--c-danger-bright',
  '--c-green-text',
  '--c-warn-bg',
  '--c-warn-border',
  '--c-warn-text',
  '--c-warn-bright',
  '--c-diff-add-bg',
  '--c-diff-add-text',
  '--c-driver-claude-text',
  '--c-driver-claude-bg',
  '--c-driver-opencode-text',
  '--c-driver-opencode-bg',
  '--c-driver-dsh-text',
  '--c-driver-dsh-bg',
  '--c-scrim',
  '--c-sidebar',
  '--c-hover',
  '--c-hover-strong',
  '--c-sidebar-border',
  '--c-sidebar-top',
  '--c-sidebar-line',
  '--c-sidebar-item',
  '--c-sidebar-text',
  '--c-sidebar-text-sel',
  '--c-sidebar-icon',
] as const;

export type ThemeTokenName = (typeof THEME_TOKEN_NAMES)[number];

/// 解析后的主题态（auto 由宿主解析为实际明/暗；视图不重复实现系统偏好媒体查询）。
export function resolvedTheme(): 'dark' | 'light' {
  if (typeof document === 'undefined') return 'light';
  return document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light';
}

/// 当前生效 token 值字典（从 documentElement 的计算样式快照；键集固定）。
export function themeTokenSnapshot(): Record<string, string> {
  if (typeof window === 'undefined') return {};
  const cs = window.getComputedStyle(document.documentElement);
  const out: Record<string, string> = {};
  for (const name of THEME_TOKEN_NAMES) {
    const v = cs.getPropertyValue(name).trim();
    if (v !== '') out[name] = v;
  }
  return out;
}

/// 视图可用的一小组语义别名（插件作者不必记忆全部 token 名）。
/// 宿主在 `hub:init` / `hub:theme` 里除全量字典外附上这组别名，降低接入成本。
export function themeAliases(tokens: Record<string, string>): Record<string, string> {
  const pick = (k: string, fallback: string): string => tokens[k] ?? fallback;
  return {
    bg: pick('--c-bg', '#ffffff'),
    surface: pick('--c-panel', '#f7f7f7'),
    text: pick('--c-text', '#1a1a1a'),
    textMuted: pick('--c-sub', '#666666'),
    border: pick('--c-border', '#e5e5e5'),
    accent: pick('--c-accent', '#10a37f'),
    danger: pick('--c-danger', '#cf222e'),
  };
}
