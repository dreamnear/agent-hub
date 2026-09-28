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

/// 模块态 + 订阅（agent-hub-settings B3：主题入口迁入设置页后，App 内可能同时有
/// 设置页与侧栏两处 useTheme 消费方——各持一份 useState 会让侧栏的主题标记不跟随
/// 设置页的改动。模块态 + 广播，机制（apply/存储口径/系统跟随）完全不变，只把
/// 「状态在哪」从组件内提到模块内。）
let current: ThemeMode | null = null;
const listeners = new Set<(m: ThemeMode) => void>();

function getMode(): ThemeMode {
  return current ?? readStored();
}

function commit(m: ThemeMode): void {
  current = m;
  apply(m);
  window.localStorage.setItem(STORAGE_KEY, m);
  for (const l of listeners) l(m);
}

export function useTheme(): {
  mode: ThemeMode;
  effective: 'dark' | 'light';
  cycle: () => void;
  /** 设置页外观分区用：直接落某态（B3 主题三态统一入口） */
  setMode: (m: ThemeMode) => void;
} {
  const [mode, setMode] = useState<ThemeMode>(readStored);

  useEffect(() => {
    // 挂载/模式变化即应用（幂等，多消费方同时落同值无害）；首个订阅者确立模块态
    if (current === null) current = mode;
    apply(mode);
    listeners.add(setMode);
    return () => {
      listeners.delete(setMode);
    };
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
    const m = getMode();
    commit(m === 'auto' ? 'dark' : m === 'dark' ? 'light' : 'auto');
  }, []);

  const set = useCallback((m: ThemeMode): void => commit(m), []);

  return { mode, effective: resolve(mode), cycle, setMode: set };
}
