import { useSyncExternalStore } from 'react';
import { zh } from './zh';
import { en } from './en';

/// i18n（agent-hub-settings C1）：扁平 key 语言包 + 模块级 locale store。
/// 默认跟系统（navigator.language zh* → zh），显式选择持久化 localStorage
/// （键名 hub_locale，与 hub_theme 同族）且优先于系统。t() 落 key 时 zh 包兜底。

export type Locale = 'zh' | 'en';
export type LangChoice = 'system' | Locale;

const STORAGE_KEY = 'hub_locale';
const PACKS: Record<Locale, Record<string, string>> = { zh, en };

let choice: LangChoice = loadChoice();
const listeners = new Set<() => void>();

function loadChoice(): LangChoice {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === 'system' || v === 'zh' || v === 'en') return v;
  } catch {
    // 存储不可用（隐私模式等）时按 system 处理
  }
  return 'system';
}

function applyDocumentLang(): void {
  document.documentElement.lang = resolveLocale() === 'zh' ? 'zh-CN' : 'en';
}

export function systemLocale(): Locale {
  const lang = typeof navigator !== 'undefined' ? navigator.language : 'zh-CN';
  return lang.toLowerCase().startsWith('zh') ? 'zh' : 'en';
}

export function resolveLocale(): Locale {
  return choice === 'system' ? systemLocale() : choice;
}

/// 当前用户选择（'system' 或具体语言）——设置页语言分区回显用
export function getLangChoice(): LangChoice {
  return choice;
}

/// 挂载期调用（main.tsx，早于首屏渲染）：同步 <html lang>，防闪烁/防 UA 表单错向
export function initI18n(): void {
  applyDocumentLang();
}

/// 显式切换语言；选「跟随系统」时清除持久化（回落系统判定）
export function setLangChoice(next: LangChoice): void {
  choice = next;
  try {
    if (next === 'system') localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // 存储不可用时仅内存生效
  }
  applyDocumentLang();
  listeners.forEach((l) => l());
}

/// 翻译：{name} 形式参数插值；缺失 key 回落 zh 包，再缺失回显 key 本身（开发期可见）
export function t(key: string, params?: Record<string, string | number>): string {
  let s = PACKS[resolveLocale()][key] ?? zh[key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      s = s.split(`{${k}}`).join(String(v));
    }
  }
  return s;
}

const subscribe = (cb: () => void): (() => void) => {
  listeners.add(cb);
  return () => listeners.delete(cb);
};

/// React 接入：choice 为原始值，变化即全树重渲（语言切换低频，无需按 key 订阅）
export function useI18n(): (key: string, params?: Record<string, string | number>) => string {
  useSyncExternalStore(subscribe, () => choice);
  return t;
}
