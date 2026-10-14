// @vitest-environment happy-dom
// 单面板壳（Git 竖栏化重构）：左缘手柄调宽（拖拽跟手/键盘 clamp/双击复位）、
// 移动端不渲染手柄、宽度持久化分键读取。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { useState, type ReactElement } from 'react';
import PanelShell, {
  PANEL_FILES_W,
  PANEL_FILES_W_KEY,
  PANEL_GIT_W,
  PANEL_GIT_W_KEY,
  PANEL_W,
  PANEL_W_KEY,
  loadPanelPrefs,
} from './PanelShell';

const mqDesktop = (): void => {
  vi.spyOn(window, 'matchMedia').mockReturnValue({
    matches: true,
    addEventListener: () => {},
    removeEventListener: () => {},
  } as unknown as MediaQueryList);
};

const mqMobile = (): void => {
  vi.spyOn(window, 'matchMedia').mockReturnValue({
    matches: false,
    addEventListener: () => {},
    removeEventListener: () => {},
  } as unknown as MediaQueryList);
};

/// 受控 harness：宽度渲染到 DOM 供 clamp 断言
function Harness(props: { min?: number; max?: number; resetWidth?: number }): ReactElement {
  const [w, setW] = useState<number>(PANEL_W.default);
  return (
    <>
      <div data-testid="w">{w}</div>
      <PanelShell width={w} onWidthChange={setW} min={props.min} max={props.max} resetWidth={props.resetWidth}>
        <div>panel-content</div>
      </PanelShell>
    </>
  );
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe('PanelShell（单面板壳）', () => {
  it('桌面渲染一个宽度手柄 + 内容槽', () => {
    mqDesktop();
    const { container, getAllByText } = render(<Harness />);
    expect(container.querySelectorAll('[role="separator"]').length).toBe(1);
    expect(getAllByText('panel-content').length).toBe(1);
  });

  it('arrow keys adjust width within range', () => {
    mqDesktop();
    const { getByRole, getByTestId } = render(<Harness />);
    const handle = getByRole('separator', { name: /panel width|面板宽度/i });
    fireEvent.keyDown(handle, { key: 'ArrowLeft' }); // 左移分隔条 = 变宽
    expect(getByTestId('w').textContent).toBe(`${PANEL_W.default + 16}`);
    // 连按 100 次变窄 → 永不低于 220
    for (let i = 0; i < 100; i += 1) fireEvent.keyDown(handle, { key: 'ArrowRight' });
    expect(getByTestId('w').textContent).toBe(`${PANEL_W.min}`);
    // 连按 100 次变宽 → 永不超 560
    for (let i = 0; i < 100; i += 1) fireEvent.keyDown(handle, { key: 'ArrowLeft' });
    expect(getByTestId('w').textContent).toBe(`${PANEL_W.max}`);
  });

  it('Git 栏区间（min/max/resetWidth props）：clamp 到 200–480，双击复位 240', () => {
    mqDesktop();
    const { getByRole, getByTestId } = render(
      <Harness min={PANEL_GIT_W.min} max={PANEL_GIT_W.max} resetWidth={PANEL_GIT_W.default} />,
    );
    const handle = getByRole('separator');
    for (let i = 0; i < 100; i += 1) fireEvent.keyDown(handle, { key: 'ArrowLeft' });
    expect(getByTestId('w').textContent).toBe(`${PANEL_GIT_W.max}`);
    for (let i = 0; i < 100; i += 1) fireEvent.keyDown(handle, { key: 'ArrowRight' });
    expect(getByTestId('w').textContent).toBe(`${PANEL_GIT_W.min}`);
    fireEvent.doubleClick(handle);
    expect(getByTestId('w').textContent).toBe(`${PANEL_GIT_W.default}`);
  });

  it('drag on width handle follows pointer (按起点算总位移，无累计漂移)', () => {
    mqDesktop();
    const { getByRole, getByTestId } = render(<Harness />);
    const handle = getByRole('separator');
    fireEvent.pointerDown(handle, { pointerId: 1, clientX: 500 });
    // 向左拖 60px = 变宽：280+60=340
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: 440 });
    expect(getByTestId('w').textContent).toBe('340');
    // 拖回起点右侧 40px（总位移 -40）→ 280-40=240（按起点算，无累计）
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: 540 });
    expect(getByTestId('w').textContent).toBe('240');
  });

  it('double-click on width handle resets to default', () => {
    mqDesktop();
    let reported = 500;
    const { getByRole } = render(
      <PanelShell
        width={500}
        onWidthChange={(w) => {
          reported = w;
        }}
      >
        null
      </PanelShell>,
    );
    fireEvent.doubleClick(getByRole('separator', { name: /panel width|面板宽度/i }));
    expect(reported).toBe(PANEL_W.default);
  });

  it('renders no handle on mobile (≤768px), content intact', () => {
    mqMobile();
    const { container, getByText, queryAllByRole } = render(<Harness />);
    expect(queryAllByRole('separator')).toEqual([]);
    expect(getByText('panel-content')).not.toBeNull();
    expect(container.querySelector('.panel-slot')).not.toBeNull();
  });
});

describe('loadPanelPrefs（工程/Git 宽度分键）', () => {
  it('reads stored values and clamps out-of-range ones', () => {
    window.localStorage.setItem(PANEL_W_KEY, '400');
    window.localStorage.setItem(PANEL_GIT_W_KEY, '300');
    window.localStorage.setItem(PANEL_FILES_W_KEY, '350');
    expect(loadPanelPrefs()).toEqual({ width: 400, gitWidth: 300, filesWidth: 350 });
    window.localStorage.setItem(PANEL_W_KEY, '99999');
    window.localStorage.setItem(PANEL_GIT_W_KEY, '-5');
    expect(loadPanelPrefs().width).toBe(PANEL_W.max);
    expect(loadPanelPrefs().gitWidth).toBe(PANEL_GIT_W.min);
    expect(loadPanelPrefs().filesWidth).toBe(350); // 合法值保持（未越界不钳）
  });

  it('falls back to defaults when nothing stored', () => {
    expect(loadPanelPrefs()).toEqual({
      width: PANEL_W.default,
      gitWidth: PANEL_GIT_W.default,
      filesWidth: PANEL_FILES_W.default,
    });
  });
});
