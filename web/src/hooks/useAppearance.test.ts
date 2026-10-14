// @vitest-environment happy-dom
// 外观持久化（agent-hub-settings B2）：读档默认 / 挂载落 documentElement /
// 切档即时生效 + localStorage 保持 / 非法档位回落默认。
import { afterEach, describe, expect, it } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { applyAppearance, FONT_SCALES, useAppearance } from './useAppearance';

afterEach(() => {
  window.localStorage.clear();
  delete document.documentElement.dataset.font;
  document.documentElement.style.removeProperty('--ui-scale');
});

describe('useAppearance（B2 字体族 + 字号缩放）', () => {
  it('默认档：system 字体 + 100% 缩放，挂载即落 dataset 与 --ui-scale', () => {
    const { result } = renderHook(() => useAppearance());
    expect(result.current.family).toBe('system');
    expect(result.current.scale).toBe(1);
    expect(document.documentElement.dataset.font).toBe('system');
    expect(document.documentElement.style.getPropertyValue('--ui-scale')).toBe('1');
  });

  it('切字体族与字号档即时生效并持久化', () => {
    const { result } = renderHook(() => useAppearance());
    act(() => result.current.setFamily('mono'));
    act(() => result.current.setScale(1.5));
    expect(document.documentElement.dataset.font).toBe('mono');
    expect(document.documentElement.style.getPropertyValue('--ui-scale')).toBe('1.5');
    expect(window.localStorage.getItem('hub_font_family')).toBe('mono');
    expect(window.localStorage.getItem('hub_font_scale')).toBe('1.5');
  });

  it('刷新后从 localStorage 恢复', () => {
    window.localStorage.setItem('hub_font_family', 'serif');
    window.localStorage.setItem('hub_font_scale', '1.25');
    const { result } = renderHook(() => useAppearance());
    expect(result.current.family).toBe('serif');
    expect(result.current.scale).toBe(1.25);
    expect(document.documentElement.dataset.font).toBe('serif');
    expect(document.documentElement.style.getPropertyValue('--ui-scale')).toBe('1.25');
  });

  it('非法档位回落默认（手改存储不炸页面）', () => {
    window.localStorage.setItem('hub_font_family', 'comic');
    window.localStorage.setItem('hub_font_scale', '99');
    const { result } = renderHook(() => useAppearance());
    expect(result.current.family).toBe('system');
    expect(result.current.scale).toBe(1);
  });

  it('档位常量为 5 档且含 1 与 1.5（150% 档是移动端走查对象）', () => {
    expect(FONT_SCALES).toHaveLength(5);
    expect(FONT_SCALES).toContain(1);
    expect(FONT_SCALES).toContain(1.5);
  });

  it('applyAppearance 与 hook 写入同口径（index.html 首帧脚本镜像依据）', () => {
    applyAppearance('mono', 0.9);
    expect(document.documentElement.dataset.font).toBe('mono');
    expect(document.documentElement.style.getPropertyValue('--ui-scale')).toBe('0.9');
  });
});
