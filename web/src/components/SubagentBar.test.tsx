// @vitest-environment happy-dom
// agent-hub-subagent-strip：单行 chip 流（S1 装箱+溢出收纳 / S2 +N 可展开可收起 /
// S3 resize 即时重排 / S4 既有能力不回归）。宽度测量用 HTMLElement prototype spy
// 模拟（happy-dom 全零宽）；零宽环境本身即「测量不可用 → 兜底前 3」的覆盖。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, waitFor } from '@testing-library/react';
import SubagentBar from './SubagentBar';
import type { SubagentEntry } from '../types';

const active: SubagentEntry[] = [
  {
    agentId: 'dr-coder-1111',
    name: 'dr-coder',
    agentType: 'dr-coder',
    description: '编码实现',
    model: 'opus',
    status: 'active',
    startedAt: null,
    lastActiveAt: null,
  },
  {
    agentId: 'dr-tester-2222',
    name: 'dr-tester',
    agentType: 'dr-tester',
    description: null,
    model: null,
    status: 'active',
    startedAt: null,
    lastActiveAt: null,
  },
  {
    agentId: 'dr-planner-3333',
    name: 'dr-planner',
    agentType: 'dr-planner',
    description: null,
    model: null,
    status: 'active',
    startedAt: null,
    lastActiveAt: null,
  },
  {
    agentId: 'anon-4444',
    name: '子任务 #1',
    agentType: 'subagent',
    description: '调查 OpenOcta 服务器版需求',
    model: null,
    status: 'active',
    startedAt: null,
    lastActiveAt: null,
  },
];

const finished: SubagentEntry[] = [
  {
    agentId: 'old-9999',
    name: 'dr-coder #2',
    agentType: 'dr-coder',
    description: '上一轮编码',
    model: null,
    status: 'completed',
    startedAt: null,
    lastActiveAt: '2026-09-30T10:00:00Z',
  },
];

/// 宽度模拟：真实行 clientWidth + 测量行替身 offsetWidth（可变对象支持 resize 用例）
function mockWidths(w: { row: number; chip: number; toggle: number }): void {
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function (this: HTMLElement) {
    if (this.classList.contains('subagent-chip-list') && !this.classList.contains('subagent-measure')) {
      return w.row;
    }
    return 0;
  });
  vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockImplementation(function (this: HTMLElement) {
    if (this.dataset.measure === 'chip') return w.chip;
    if (this.dataset.measure === 'expand' || this.dataset.measure === 'history') return w.toggle;
    return 0;
  });
}

afterEach(() => {
  vi.restoreAllMocks();
});

const renderBar = (onSelect = vi.fn()) =>
  render(<SubagentBar subagents={active} finished={finished} activeId={null} onSelect={onSelect} />);

/// 真实行的展开器（排除隐藏测量行里的同构替身）
const expanderOf = (c: HTMLElement): HTMLButtonElement | null =>
  c.querySelector('.subagent-chip-list:not(.subagent-measure) .subagent-toggle:not(.subagent-history-toggle)');

describe('SubagentBar 单行化（S1 装箱）', () => {
  it('零宽环境（测量不可用）兜底前 3 + 溢出 +N；行恒单行（nowrap 类）', () => {
    const { container } = renderBar();
    const row = container.querySelector('.subagent-chip-list.is-collapsed') as HTMLElement;
    expect(row).not.toBeNull();
    expect(container.querySelectorAll('.subagent-chip-list.is-collapsed > .subagent-chip')).toHaveLength(3);
    // 溢出收纳：+1（4-3）；测量行作为数据源存在
    const expand = expanderOf(container) as HTMLButtonElement;
    expect(expand.textContent).toBe('+1');
    expect(container.querySelector('.subagent-measure')).not.toBeNull();
    // 头部只数活跃
    expect(container.querySelector('.subagent-summary-text')?.textContent).toBe('🤖 Subagent 4');
  });

  it('实测全放得下：全量 chip 直显，无展开器（History 预留参与装箱）', async () => {
    mockWidths({ row: 1000, chip: 100, toggle: 80 });
    const { container } = renderBar();
    await waitFor(() => {
      // 4×100 + gap + History 80 ≤ 1000 → 全显
      expect(container.querySelectorAll('.subagent-chip-list.is-collapsed > .subagent-chip')).toHaveLength(4);
    });
    expect(expanderOf(container)).toBeNull();
  });

  it('实测放不下：预留展开器+History 宽度后取最大前缀（1 chip + +3）', async () => {
    mockWidths({ row: 350, chip: 100, toggle: 80 });
    const { container } = renderBar();
    await waitFor(() => {
      // acc=80(hist)+80(exp)=160；160+100=260≤350 收 1 个；+100=360>350 停
      expect(container.querySelectorAll('.subagent-chip-list.is-collapsed > .subagent-chip')).toHaveLength(1);
    });
    const expand = container.querySelector('.subagent-toggle:not(.subagent-history-toggle)') as HTMLButtonElement;
    expect(expand.textContent).toBe('+3');
    // History 入口不被 chip 流挤出（装箱预留，恒在行内）
    expect(container.querySelector('.subagent-history-toggle')).not.toBeNull();
  });
});

describe('SubagentBar 展开交互（S2）', () => {
  it('点 +N 展开全量列表（解除单行约束 is-expanded wrap），点 Collapse 收回 +N', async () => {
    mockWidths({ row: 350, chip: 100, toggle: 80 });
    const { container } = renderBar();
    const expand = (await waitFor(() => {
      const el = expanderOf(container) as HTMLButtonElement;
      expect(el.textContent).toBe('+3');
      return el;
    })) as HTMLButtonElement;
    fireEvent.click(expand);
    const row = container.querySelector('.subagent-chip-list.is-expanded') as HTMLElement;
    expect(row).not.toBeNull();
    expect(container.querySelectorAll('.subagent-chip-list.is-expanded > .subagent-chip')).toHaveLength(4);
    const collapse = expanderOf(container) as HTMLButtonElement;
    expect(collapse.textContent).toBe('Collapse');
    fireEvent.click(collapse);
    expect(container.querySelector('.subagent-chip-list.is-collapsed')).not.toBeNull();
    expect(container.querySelectorAll('.subagent-chip-list.is-collapsed > .subagent-chip')).toHaveLength(1);
  });

  it('展开态信息不丢失：状态点/选中上报/History 入口全保留', async () => {
    mockWidths({ row: 350, chip: 100, toggle: 80 });
    const onSelect = vi.fn();
    const { container } = render(
      <SubagentBar subagents={active} finished={finished} activeId="dr-tester-2222" onSelect={onSelect} />,
    );
    fireEvent.click(container.querySelector('.subagent-toggle:not(.subagent-history-toggle)') as HTMLButtonElement);
    expect(container.querySelectorAll('.subagent-chip-list.is-expanded > .subagent-chip')).toHaveLength(4);
    expect(container.querySelectorAll('.subagent-dot--active').length).toBeGreaterThan(0);
    // 点任一 chip 上报选中
    const chip = container.querySelectorAll('.subagent-chip-list.is-expanded > .subagent-chip')[3] as HTMLButtonElement;
    fireEvent.click(chip);
    expect(onSelect).toHaveBeenCalledWith('anon-4444');
    // History 仍在且可用
    fireEvent.click(container.querySelector('.subagent-history-toggle') as HTMLButtonElement);
    expect(container.querySelectorAll('.subagent-history-list .subagent-chip')).toHaveLength(1);
  });
});

describe('SubagentBar 响应式（S3）', () => {
  it('容器变宽即时重排：窄行 1+3，resize 后全显（展开器消失）', async () => {
    const w = { row: 350, chip: 100, toggle: 80 };
    mockWidths(w);
    const { container } = renderBar();
    await waitFor(() => {
      expect(container.querySelectorAll('.subagent-chip-list.is-collapsed > .subagent-chip')).toHaveLength(1);
    });
    w.row = 1000; // 变宽
    window.dispatchEvent(new Event('resize'));
    await waitFor(() => {
      expect(container.querySelectorAll('.subagent-chip-list.is-collapsed > .subagent-chip')).toHaveLength(4);
    });
    expect(expanderOf(container)).toBeNull();
  });
});

describe('SubagentBar 既有能力不回归（S4）', () => {
  it('renders nothing when there are no active subagents (history entry absent)', () => {
    const { container } = render(
      <SubagentBar subagents={[]} finished={finished} activeId={null} onSelect={() => {}} />,
    );
    expect(container.querySelector('.subagent-card')).toBeNull();
  });

  it('exposes chips with status dots and reports selection', () => {
    const onSelect = vi.fn();
    mockWidths({ row: 1000, chip: 100, toggle: 80 });
    const { container } = render(
      <SubagentBar subagents={active} finished={[]} activeId="dr-tester-2222" onSelect={onSelect} />,
    );
    expect(container.querySelector('.subagent-dot--active')).not.toBeNull();
    const sel = container.querySelector('.subagent-chip--active') as HTMLButtonElement;
    expect(sel.textContent).toContain('dr-tester');
    sel.click();
    expect(onSelect).toHaveBeenCalledWith('dr-tester-2222');
  });

  it('history entry reveals finished chips; their hash id is tooltip-secondary', async () => {
    mockWidths({ row: 1000, chip: 100, toggle: 80 });
    const { container } = renderBar();
    fireEvent.click(container.querySelector('.subagent-history-toggle') as HTMLButtonElement);
    expect(container.querySelectorAll('.subagent-history-list .subagent-chip')).toHaveLength(1);
    const done = container.querySelector('.subagent-history-list .subagent-chip') as HTMLButtonElement;
    expect(done.textContent).toContain('dr-coder #2');
    // 哈希 id 降级为 tooltip（description · agentId），不做主标签
    expect(done.title).toBe('上一轮编码 · old-9999');
  });

  it('loads its stylesheet for chip shape and single-line constraint (missing-import guard)', () => {
    const { container } = render(
      <SubagentBar subagents={active} finished={[]} activeId={null} onSelect={() => {}} />,
    );
    const chip = container.querySelector('.subagent-chip') as HTMLElement;
    expect(getComputedStyle(chip).borderRadius).toBe('999px');
    const cssText = [...document.styleSheets].length;
    expect(cssText).toBeGreaterThan(0);
  });
});
