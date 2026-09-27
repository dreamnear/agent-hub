// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { useTheme, type ThemeMode } from './useTheme';

afterEach(() => {
  window.localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
  document.documentElement.removeAttribute('data-theme-auto');
});

describe('useTheme（反馈轮 26-A）', () => {
  it('cycles auto → dark → light and persists to localStorage', () => {
    const { result } = renderHook(() => useTheme());
    expect(result.current.mode).toBe('auto');
    act(() => result.current.cycle());
    expect(result.current.mode).toBe('dark');
    expect(window.localStorage.getItem('hub_theme')).toBe('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
    act(() => result.current.cycle());
    expect(result.current.mode).toBe('light');
    expect(document.documentElement.dataset.theme).toBe('light');
    act(() => result.current.cycle());
    expect(result.current.mode).toBe('auto');
    expect(document.documentElement.dataset.themeAuto).toBe('1');
  });

  it('restores the persisted mode on mount', () => {
    window.localStorage.setItem('hub_theme', 'dark');
    const { result } = renderHook(() => useTheme());
    expect((result.current.mode as ThemeMode) === 'dark').toBe(true);
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('auto mode resolves via prefers-color-scheme', () => {
    vi.spyOn(window, 'matchMedia').mockReturnValue({
      matches: true,
      addEventListener: () => {},
      removeEventListener: () => {},
    } as unknown as MediaQueryList);
    const { result } = renderHook(() => useTheme());
    expect(result.current.mode).toBe('auto');
    expect(result.current.effective).toBe('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
  });
});
