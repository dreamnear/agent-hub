// @vitest-environment happy-dom
// Zed 式底部工具栏（面板显隐 toggle）：三个开关的回调/激活态、Git 置灰联动、
// ≤768px 不渲染、hub_panels 读写容错。
import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { useState, type ReactElement } from 'react';
import BottomBar, { PANELS_KEY, loadPanelVis, type PanelKey, type PanelVis } from './BottomBar';

const mq = (mobile: boolean): void => {
  vi.spyOn(window, 'matchMedia').mockImplementation((query: string) => ({
    matches: mobile ? query.includes('max-width') : query.includes('min-width'),
    addEventListener: () => {},
    removeEventListener: () => {},
  }) as unknown as MediaQueryList);
};

/// 受控 harness：toggle 回灌渲染（激活态可视化）
function Harness(props: { initial?: PanelVis }): ReactElement {
  const [vis, setVis] = useState<PanelVis>(
    props.initial ?? { sessions: true, projects: true, git: true, files: true },
  );
  const onToggle = (k: PanelKey): void => setVis((p) => ({ ...p, [k]: !p[k] }));
  return (
    <BottomBar vis={vis} onToggle={onToggle} sessionCount={3} instanceCount={2} />
  );
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe('BottomBar', () => {
  it('桌面渲染四个 toggle 与被动状态区；激活态/aria-pressed 随 vis', () => {
    mq(false);
    const { container } = render(<Harness />);
    const btns = container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle');
    expect(btns).toHaveLength(4);
    expect(container.textContent).toContain('会话');
    expect(container.textContent).toContain('工程');
    expect(container.textContent).toContain('Git');
    expect(container.textContent).toContain('3 会话 · 2 实例');
    // 默认全显：三个都激活
    btns.forEach((b) => expect(b.getAttribute('aria-pressed')).toBe('true'));
  });

  it('左右分组（用户预览反馈）：左组仅会话，右组工程/Git/文件贴右边', () => {
    mq(false);
    const { container } = render(<Harness />);
    const groups = container.querySelectorAll('.bottom-bar-toggles');
    expect(groups.length).toBe(2);
    // 左组：仅会话
    const left = groups[0]!.querySelectorAll('.bottom-bar-toggle');
    expect(left.length).toBe(1);
    expect(left[0]!.textContent).toContain('会话');
    // 右组：工程/Git/文件 三开关，贴右（margin-left:auto）
    const right = groups[1]!.querySelectorAll('.bottom-bar-toggle');
    expect(right.length).toBe(3);
    expect(right[0]!.textContent).toContain('工程');
    expect(right[2]!.textContent).toContain('文件');
    const css = readFileSync('src/components/BottomBar.css', 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
    expect(css).toMatch(/\.bottom-bar-toggles--right\s*\{[^}]*margin-left:\s*auto/);
  });

  it('toggle 回调按 key 触发并回灌激活态', () => {
    mq(false);
    const { container } = render(<Harness />);
    const btns = container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle');
    fireEvent.click(btns[0]); // sessions
    expect(container.querySelector('.bottom-bar-toggle')?.className).not.toContain('is-on');
    expect(container.querySelectorAll('.bottom-bar-toggle')[1]?.className).toContain('is-on');
  });

  it('Git 与工程独立：工程隐藏时 Git toggle 照常可点（联动已取消）', () => {
    mq(false);
    const { container } = render(
      <Harness initial={{ sessions: true, projects: false, git: true, files: true }} />,
    );
    const btns = container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle');
    expect((btns[2] as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(btns[2]);
    // Git 独立切换为隐藏
    expect(btns[2].getAttribute('aria-pressed')).toBe('false');
    // 工程隐藏不受影响
    expect(btns[1].getAttribute('aria-pressed')).toBe('false');
  });

  it('≤768px 不渲染', () => {
    mq(true);
    const { container } = render(<Harness />);
    expect(container.querySelector('.bottom-bar')).toBeNull();
  });
});

describe('loadPanelVis（hub_panels 读写容错）', () => {
  it('无值回退默认全显；坏 JSON/缺项/类型不符逐项回退', () => {
    expect(loadPanelVis()).toEqual({ sessions: true, projects: true, git: true, files: true });

    window.localStorage.setItem(PANELS_KEY, JSON.stringify({ sessions: false, projects: false, git: false }));
    expect(loadPanelVis()).toEqual({ sessions: false, projects: false, git: false, files: true });

    window.localStorage.setItem(PANELS_KEY, JSON.stringify({ sessions: false }));
    expect(loadPanelVis()).toEqual({ sessions: false, projects: true, git: true, files: true });

    window.localStorage.setItem(PANELS_KEY, JSON.stringify({ sessions: 'yes', git: 1 }));
    expect(loadPanelVis()).toEqual({ sessions: true, projects: true, git: true, files: true });

    window.localStorage.setItem(PANELS_KEY, '{broken json');
    expect(loadPanelVis()).toEqual({ sessions: true, projects: true, git: true, files: true });
  });
});
