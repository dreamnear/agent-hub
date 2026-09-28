import { useCallback, useEffect, useState } from 'react';

/// 外观设置（agent-hub-settings B2）：界面字体族 + 界面字号缩放。
///
/// 两者都只落 `documentElement`（一处 dataset + 一个 CSS 变量），CSS 侧全局读它们：
/// - 字体族：`data-font` 属性 → App.css 的 `[data-font='…']` 覆盖 `--font-ui`
///   （字体栈只写在 CSS 一处，前端不复制字体串）；
/// - 字号缩放：`--ui-scale` 变量 → `html` 根字号 = `100% × --ui-scale`，全站字号
///   一律 rem，所以缩放只改一行变量，不逐组件改字号（110+ 处 font-size 零散改 =
///   半改半不改的温床）。
///
/// 持久化键与 `hub_theme` 同族（`hub_` 前缀），互不冲突。首帧防闪由 index.html
/// 内联脚本承担（与 FOUC 主题防护同一处，镜像本文件的取值口径）。

export type FontFamilyKey = 'system' | 'mono' | 'serif';

/** 字体档位（加一档 = 加一个 key + App.css 一条覆盖规则）；显示名走 i18n
 *  `settings.fontFamily.<key>`（agent-hub-settings C2） */
export const FONT_FAMILIES: { key: FontFamilyKey }[] = [
  { key: 'system' },
  { key: 'mono' },
  { key: 'serif' },
];

/** 字号缩放档位（5 档：90% / 100% / 110% / 125% / 150%） */
export const FONT_SCALES = [0.9, 1, 1.1, 1.25, 1.5] as const;
export type FontScale = (typeof FONT_SCALES)[number];

const FAMILY_KEY = 'hub_font_family';
const SCALE_KEY = 'hub_font_scale';
const DEFAULT_FAMILY: FontFamilyKey = 'system';
const DEFAULT_SCALE: FontScale = 1;

function readFamily(): FontFamilyKey {
  if (typeof window === 'undefined') return DEFAULT_FAMILY;
  const v = window.localStorage.getItem(FAMILY_KEY);
  return FONT_FAMILIES.some((f) => f.key === v) ? (v as FontFamilyKey) : DEFAULT_FAMILY;
}

function readScale(): FontScale {
  if (typeof window === 'undefined') return DEFAULT_SCALE;
  const v = Number(window.localStorage.getItem(SCALE_KEY));
  return (FONT_SCALES as readonly number[]).includes(v) ? (v as FontScale) : DEFAULT_SCALE;
}

/** 落 documentElement（index.html 首帧脚本按同一口径镜像，防缩放闪动） */
export function applyAppearance(family: FontFamilyKey, scale: FontScale): void {
  const root = document.documentElement;
  root.dataset.font = family;
  root.style.setProperty('--ui-scale', String(scale));
}

export function useAppearance(): {
  family: FontFamilyKey;
  scale: FontScale;
  setFamily: (f: FontFamilyKey) => void;
  setScale: (s: FontScale) => void;
} {
  const [family, setFamilyState] = useState<FontFamilyKey>(readFamily);
  const [scale, setScaleState] = useState<FontScale>(readScale);

  useEffect(() => {
    applyAppearance(family, scale);
    window.localStorage.setItem(FAMILY_KEY, family);
    window.localStorage.setItem(SCALE_KEY, String(scale));
  }, [family, scale]);

  const setFamily = useCallback((f: FontFamilyKey): void => setFamilyState(f), []);
  const setScale = useCallback((s: FontScale): void => setScaleState(s), []);

  return { family, scale, setFamily, setScale };
}
