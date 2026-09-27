import { useCallback, useEffect, useState } from 'react';

/// 主题三态（反馈轮 26-A）：auto（跟随系统 prefers-color-scheme，实时）/ dark / light。
/// localStorage 'hub_theme' 持久化；实际主题写 documentElement.dataset.theme，
/// App.css 的 [data-theme='dark'] 覆盖套生效；auto 时移除 data-theme 走 :root 亮色
/// ——系统暗色检测在 effect 内完成（无 data-theme=light 时若系统暗则写 dark）。
export type ThemeMode = 'auto' | 'dark' | 'light';

const STORAGE_KEY = 'hub_theme';

function systemDark(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches;
}

function readStored(): ThemeMode {
  if (typeof window === 'undefined') return 'auto';
  const v = window.localStorage.getItem(STORAGE_KEY);
  return v === 'dark' || v === 'light' || v === 'auto' ? v : 'auto';
}

/** 解析后的实际主题（auto 时按系统） */
function resolve(mode: ThemeMode): 'dark' | 'light' {
  if (mode === 'auto') return systemDark() ? 'dark' : 'light';
  return mode;
}

function apply(mode: ThemeMode): void {
  const effective = resolve(mode);
  const root = document.documentElement;
  if (mode === 'auto') {
    // auto：不落手动属性，交给 prefers-color-scheme 判定（仍写实际值供 CSS 使用）
    root.dataset.theme = effective;
    root.dataset.themeAuto = '1';
  } else {
    root.dataset.theme = effective;
    delete root.dataset.themeAuto;
  }
}

export function useTheme(): {
  mode: ThemeMode;
  effective: 'dark' | 'light';
  cycle: () => void;
} {
  const [mode, setMode] = useState<ThemeMode>(readStored);

  // 初始化 + 模式变化时应用
  useEffect(() => {
    apply(mode);
    window.localStorage.setItem(STORAGE_KEY, mode);
  }, [mode]);

  // auto 模式下系统主题实时跟随（matchMedia change）
  useEffect(() => {
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = (): void => {
      if (readStored() === 'auto') apply('auto');
    };
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  const cycle = useCallback((): void => {
    setMode((m) => (m === 'auto' ? 'dark' : m === 'dark' ? 'light' : 'auto'));
  }, []);

  return { mode, effective: resolve(mode), cycle };
}
