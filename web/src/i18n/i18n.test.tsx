// @vitest-environment happy-dom
// i18n 框架（agent-hub-settings C1）：默认解析/持久化/显式优先/参数插值/
// <html lang> 同步/双包 key 一致/React 切换即时生效。
// 各用例用 vi.resetModules + 动态 import 取全新模块态（choice/loadChoice 重跑）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactElement } from 'react';

beforeEach(() => {
  window.localStorage.clear();
  vi.resetModules();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  window.localStorage.clear();
});

const fresh = (): Promise<typeof import('./index')> => import('./index');

function stubNavigatorLanguage(lang: string): void {
  vi.stubGlobal('navigator', { language: lang });
}

describe('locale 解析（C1）', () => {
  it('默认跟随系统：navigator.language zh* → zh，其余 → en', async () => {
    stubNavigatorLanguage('zh-CN');
    const i18n = await fresh();
    expect(i18n.systemLocale()).toBe('zh');
    expect(i18n.resolveLocale()).toBe('zh');
    expect(i18n.t('settings.language.label')).toBe('界面语言');

    stubNavigatorLanguage('en-US');
    expect(i18n.resolveLocale()).toBe('en');
    expect(i18n.t('settings.language.label')).toBe('Interface language');
  });

  it('显式选择持久化 localStorage 且优先于系统语言', async () => {
    stubNavigatorLanguage('en-US');
    const i18n = await fresh();
    i18n.setLangChoice('zh');
    expect(window.localStorage.getItem('hub_locale')).toBe('zh');
    expect(i18n.resolveLocale()).toBe('zh');
    expect(i18n.t('settings.language.label')).toBe('界面语言');
  });

  it('选「跟随系统」清除持久化并回落系统判定', async () => {
    stubNavigatorLanguage('en-US');
    const i18n = await fresh();
    i18n.setLangChoice('zh');
    expect(window.localStorage.getItem('hub_locale')).toBe('zh');
    i18n.setLangChoice('system');
    expect(window.localStorage.getItem('hub_locale')).toBeNull();
    expect(i18n.resolveLocale()).toBe('en');
  });

  it('存储中的非法值按 system 处理', async () => {
    window.localStorage.setItem('hub_locale', 'fr');
    const i18n = await fresh();
    expect(i18n.getLangChoice()).toBe('system');
  });

  it('模块加载时从 localStorage 恢复显式选择', async () => {
    window.localStorage.setItem('hub_locale', 'en');
    const i18n = await fresh();
    expect(i18n.getLangChoice()).toBe('en');
    expect(i18n.resolveLocale()).toBe('en');
  });
});

describe('t()（C1）', () => {
  it('参数插值 {name} 形式', async () => {
    stubNavigatorLanguage('zh-CN');
    const i18n = await fresh();
    expect(i18n.t('settings.language.label')).toBe('界面语言');
    expect(i18n.t('__nonexistent__', { n: 1 })).toBe('__nonexistent__');
  });

  it('<html lang> 随语言更新', async () => {
    stubNavigatorLanguage('zh-CN');
    const i18n = await fresh();
    i18n.initI18n();
    expect(document.documentElement.lang).toBe('zh-CN');
    i18n.setLangChoice('en');
    expect(document.documentElement.lang).toBe('en');
  });

  it('zh/en 语言包 key 集合一致且值非空', async () => {
    const { zh } = await import('./zh');
    const { en } = await import('./en');
    expect(Object.keys(en).sort()).toEqual(Object.keys(zh).sort());
    for (const [k, v] of Object.entries(zh)) {
      expect(v.trim(), `zh[${k}] 为空`).not.toBe('');
    }
    for (const [k, v] of Object.entries(en)) {
      expect(v.trim(), `en[${k}] 为空`).not.toBe('');
    }
  });
});

describe('React 切换即时生效（C1）', () => {
  it('setLangChoice 后已挂载组件文案即时切换', async () => {
    stubNavigatorLanguage('zh-CN');
    const i18n = await fresh();
    function Probe(): ReactElement {
      const t = i18n.useI18n();
      return (
        <button type="button" onClick={() => i18n.setLangChoice('en')}>
          {t('settings.language.label')}
        </button>
      );
    }
    render(<Probe />);
    const btn = screen.getByRole('button');
    expect(btn.textContent).toBe('界面语言');
    act(() => {
      fireEvent.click(btn);
    });
    expect(btn.textContent).toBe('Interface language');
    expect(window.localStorage.getItem('hub_locale')).toBe('en');
  });
});
