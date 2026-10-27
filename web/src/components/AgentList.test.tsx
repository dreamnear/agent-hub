// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import AgentList, { groupKeyOf, buildStatusGroups } from './AgentList';
import AgentRow from './AgentRow';
import type { AgentSummary } from '../types';

const removeAgentMock = vi.hoisted(() => vi.fn());
const interruptMock = vi.hoisted(() => vi.fn());
const logsMock = vi.hoisted(() => vi.fn());
const stopAgentMock = vi.hoisted(() => vi.fn());
const respawnAgentMock = vi.hoisted(() => vi.fn());
const getNoteMock = vi.hoisted(() => vi.fn());
const putNoteMock = vi.hoisted(() => vi.fn());
const cancelAcpSessionMock = vi.hoisted(() => vi.fn());
const unlistAgentMock = vi.hoisted(() => vi.fn());
vi.mock('../api', () => ({
  api: {
    listAgents: () => Promise.resolve([]),
    removeAgent: (...a: unknown[]) => removeAgentMock(...a),
    interruptAgent: (...a: unknown[]) => interruptMock(...a),
    logs: (...a: unknown[]) => logsMock(...a),
    stopAgent: (...a: unknown[]) => stopAgentMock(...a),
    respawnAgent: (...a: unknown[]) => respawnAgentMock(...a),
    getNote: (...a: unknown[]) => getNoteMock(...a),
    putNote: (...a: unknown[]) => putNoteMock(...a),
    cancelAcpSession: (id: string) => cancelAcpSessionMock(id),
    unlistAgent: (...a: unknown[]) => unlistAgentMock(...a),
  },
  openEventStream: () => () => {},
}));

/// 侧栏会话操作按实例路由的 api 选择器（任务7 前置）：本机/未知 → 模块级 api mock。
const apiFor = vi.hoisted(() => () => ({
  interruptAgent: (...a: unknown[]) => interruptMock(...a),
  stopAgent: (...a: unknown[]) => stopAgentMock(...a),
  respawnAgent: (...a: unknown[]) => respawnAgentMock(...a),
  removeAgent: (...a: unknown[]) => removeAgentMock(...a),
  logs: (...a: unknown[]) => logsMock(...a),
  cancelAcpSession: (id: string) => cancelAcpSessionMock(id),
  unlistAgent: (...a: unknown[]) => unlistAgentMock(...a),
}));

/// 实例健康态的可变 mock（agent-hub-instance-health）：默认全干净——三态均 online，
/// 仅本机。agents 可覆盖（agent-list-status-grouping G1-G4 多实例渲染测试用；null = FIXTURE）。
const degradeState = vi.hoisted(() => ({
  health: {} as Record<string, 'online' | 'degraded' | 'offline'>,
  authError: {} as Record<string, boolean>,
  instances: [
    { id: null, name: '本机', mode: 'direct', baseUrl: '', config: null },
  ] as { id: string | null; name: string; mode: 'direct' | 'ssh-tunnel'; baseUrl: string; config: null }[],
  agents: null as AgentSummary[] | null,
}));

vi.mock('../hooks/useAgents', async (importOriginal) => {
  const mod = await importOriginal<typeof import('../hooks/useAgents')>();
  return {
    ...mod,
    useAgents: () => ({
      data: degradeState.agents ?? FIXTURE,
      grouped: mod.groupAgents(degradeState.agents ?? FIXTURE),
      isLoading: false,
      tree: [],
      instances: degradeState.instances,
      health: degradeState.health,
      authError: degradeState.authError,
      registry: {
        instances: degradeState.instances,
        apiFor,
      },
    }),
  };
});

const mk = (id: string, name: string, group: AgentSummary['group']): AgentSummary => ({
  driver: 'claude',
  id,
  name,
  cwd: '/repo/main',
  kind: 'background',
  rawState: null,
  group,
  detail: null,
  tokens: null,
  context_tokens: null,
  startedAt: null,
  sessionId: null,
});

const FIXTURE: AgentSummary[] = [
  mk('a1', '需要输入的会话', 'needs_input'),
  mk('a2', '运行中的会话', 'working'),
  mk('a3', '已完成的会话', 'completed'),
  // R8：interactive 前台会话（合成 id + sessionId）——IC 徽标/只读/从列表移除用
  {
    ...mk('itv-839daca8-a39', 'dev-f1', 'other'),
    kind: 'interactive',
    sessionId: '839daca8-a398-48b6-9ba2-5a0dec0daae1',
  },
  // ACP 会话（批4）：并入状态桶，与 Claude Code 会话统一按 Group 分组；
  // 服务端 idle→other 映射 → 落「空闲」桶
  { ...mk('acp1', 'omp 会话', 'other'), driver: 'acp', kind: 'acp', rawState: 'idle' },
];

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const renderList = (overrides: Partial<Parameters<typeof AgentList>[0]> = {}) => {
  return render(
    <AgentList
      checked={new Set()}
      onClearFilter={vi.fn()}
      onSelect={vi.fn()}
      selectedId={null}
      onOpenSearch={vi.fn()}
      onCollapse={vi.fn()}
      onOpenSettings={vi.fn()}
      onRemoved={vi.fn()}
      onOpenNotes={vi.fn()}
      onOpenSessions={vi.fn()}
      {...overrides}
    />,
    { wrapper },
  );
};

if (typeof (window as unknown as Record<string, unknown>).alert !== 'function') {
  (window as unknown as Record<string, unknown>).alert = () => {};
}
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  degradeState.health = {};
  degradeState.authError = {};
  degradeState.instances = [{ id: null, name: '本机', mode: 'direct', baseUrl: '', config: null }];
  degradeState.agents = null;
});

describe('AgentList Sidebar-v2', () => {
  it('顶部品牌条：agent-hub 品牌 + 搜索/折叠图标', () => {
    renderList();
    expect(screen.getByText('agent-hub')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '搜索会话' }));
    fireEvent.click(screen.getByRole('button', { name: '折叠侧栏' }));
  });

  it('Sidebar-v3 可折叠分组：R9 默认态（工作/等待/空闲展开，仅完成折叠、等待置顶），工作中行带 accent 点', () => {
    // 反馈轮 18：组头 ▾/▸ + 计数；空组隐藏；折叠组只显组头
    const { container } = renderList();
    const heads = [...container.querySelectorAll('.agent-group-head')].map(
      (h) => `${h.textContent}`,
    );
    // ACP 会话（批4）并入状态桶：omp 空闲会话落「空闲」组。无独立 ACP 分组。
    // R8：FIXTURE 增 interactive 条目（dev-f1）→ 空闲桶 +1
    // R9（裁决 A）：等待组严格首位且默认展开（▾）
    expect(heads).toEqual(['▾等待1', '▾工作中1', '▾空闲2', '▸已完成1']);
    // 默认折叠：仅已完成只显组头；展开组行可见且 working 行带 accent 点
    const names = [...container.querySelectorAll('.agent-row .agent-name')].map(
      (el) => el.textContent,
    );
    // R8/R9：展开组行序 = 等待（a1）→ 工作中 → 空闲（dev-f1/omp）
    expect(names).toEqual(['需要输入的会话', '运行中的会话', 'dev-f1', 'omp 会话']);
    expect(container.querySelector('.agent-dot--working')).not.toBeNull();
  });

  it('ACP 会话并入状态桶（批4）：与 Claude Code 会话统一分组（防双行于工作桶）', () => {
    const { container } = renderList();
    // working 状态桶含 claude working 会话——omp idle 会话（group=other）不在「工作中」
    const workingGroup = [...container.querySelectorAll('.agent-group')].find((g) =>
      g.querySelector('.agent-group-head')?.textContent?.includes('工作中'),
    );
    expect(workingGroup?.textContent).toContain('运行中的会话');
    expect(workingGroup?.textContent).not.toContain('omp 会话');
    // omp idle 会话落「空闲」桶，行内带 other 状态圆点
    const otherGroup = [...container.querySelectorAll('.agent-group')].find((g) =>
      g.querySelector('.agent-group-head')?.textContent?.includes('空闲'),
    );
    expect(otherGroup?.textContent).toContain('omp 会话');
    expect(otherGroup?.querySelector('.agent-dot--other')).not.toBeNull();
  });

  it('ACP 空闲会话随桶参与工程过滤（批4 与 Claude 会话同语义）；working/needs_input 仍不被隐', () => {
    const { container } = renderList({ checked: new Set(['/some/other/family']) });
    // 空闲/已完成被滤空隐藏（ACP idle 落入空闲桶 → 一并被滤）
    expect(
      [...container.querySelectorAll('.agent-group-head')].some((h) =>
        h.textContent?.includes('已完成'),
      ),
    ).toBe(false);
    expect(container.textContent).not.toContain('omp 会话');
  });

  it('会话行名字前有状态圆点（按 group 着色，2026-09-23 回归恢复）', () => {
    // f000b73 曾移除行状态点；恢复后每行名字前必须有 agent-dot--{group} 圆点
    const { container } = renderList();
    // R9：等待组默认展开；仅「已完成」默认折叠需展开
    for (const label of ['已完成']) {
      const head = [...container.querySelectorAll('.agent-group-head')].find((h) =>
        h.textContent?.includes(label),
      ) as HTMLButtonElement;
      fireEvent.click(head);
    }
    for (const g of ['needs_input', 'working', 'completed'] as const) {
      expect(container.querySelector(`.agent-dot--${g}`)).not.toBeNull();
    }
    // 圆点在名字之前（行内首个元素）
    const row = container.querySelector('.agent-row');
    expect(row?.firstElementChild?.className).toContain('agent-dot');
    expect(row?.querySelector('.agent-name')).toBeTruthy();
  });

  it('点击组头切换折叠：R9 等待组默认展开，点头收起再点展开', () => {
    const { container } = renderList();
    const waitHead = [...container.querySelectorAll('.agent-group-head')].find((h) =>
      h.textContent?.includes('等待'),
    ) as HTMLButtonElement;
    // R9：等待组默认展开（置顶 + 可见）
    expect(waitHead.getAttribute('aria-expanded')).toBe('true');
    expect(container.textContent).toContain('需要输入的会话');
    fireEvent.click(waitHead);
    expect(waitHead.getAttribute('aria-expanded')).toBe('false');
    expect(container.textContent).not.toContain('需要输入的会话');
    fireEvent.click(waitHead);
    expect(waitHead.getAttribute('aria-expanded')).toBe('true');
    expect(container.textContent).toContain('需要输入的会话');
  });

  it('搜索图标与折叠按钮触发回调', () => {
    const onOpenSearch = vi.fn();
    const onCollapse = vi.fn();
    renderList({ onOpenSearch, onCollapse });
    fireEvent.click(screen.getByRole('button', { name: '搜索会话' }));
    fireEvent.click(screen.getByRole('button', { name: '折叠侧栏' }));
    expect(onOpenSearch).toHaveBeenCalledTimes(1);
    expect(onCollapse).toHaveBeenCalledTimes(1);
  });

  it('S1 底部菜单唯一入口：仅「设置」一项（五项独立入口全部迁设置页分区）', () => {
    const onOpenSettings = vi.fn();
    const { container } = renderList({ onOpenSettings });
    const menuItems = container.querySelectorAll('.sb-menu-item');
    expect(menuItems.length).toBe(1);
    expect(menuItems[0]?.textContent).toContain('设置');
    // 点唯一入口 = 无参调用（分区落上次访问，App 侧解析）
    fireEvent.click(screen.getByRole('button', { name: '设置' }));
    expect(onOpenSettings).toHaveBeenCalledWith();
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    // 被收编五项不再作为独立入口出现
    expect(screen.queryByRole('button', { name: '工程列表' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Agent 配置' })).toBeNull();
    expect(screen.queryByRole('button', { name: '新建 Agent' })).toBeNull();
    expect(screen.queryByRole('button', { name: '历史会话' })).toBeNull();
    expect(screen.queryByRole('button', { name: '实例管理' })).toBeNull();
  });

  it('顶栏主题标记仍定向跳外观分区；底部「设置」无参打开（S1/S4）', () => {
    const onOpenSettings = vi.fn();
    renderList({ onOpenSettings });
    // 顶栏主题按钮：B3 起不再 cycle，定向跳设置页外观分区
    fireEvent.click(screen.getByRole('button', { name: /主题：/ }));
    expect(onOpenSettings).toHaveBeenCalledWith('appearance');
    // 底部唯一入口无参（App 落上次访问分区）
    fireEvent.click(screen.getByRole('button', { name: '设置' }));
    expect(onOpenSettings).toHaveBeenLastCalledWith();
    expect(onOpenSettings).toHaveBeenCalledTimes(2);
  });

  it('选中行带高亮类（左 2px accent 竖条由样式承载）', () => {
    const { container } = renderList({ selectedId: 'a2' });
    expect(container.querySelector('.agent-row-wrap--sel')?.textContent).toContain(
      '运行中的会话',
    );
    expect(container.querySelectorAll('.agent-row-wrap--sel').length).toBe(1);
  });

  it('行 ⋯ 菜单：不允许打开终端（无此入口）+ 序列化会话操作（Logs/工程便签/中断/Stop/Respawn/Remove）', () => {
    const onSelect = vi.fn();
    renderList({ onSelect });
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: '打开终端' })).toBeNull();
    // bg 会话（kind=background）全部会话操作可用
    for (const item of ['Logs', '工程便签', '中断', 'Stop', 'Respawn', '删除会话']) {
      expect(screen.getByRole('menuitem', { name: item })).not.toBeNull();
    }
    expect(document.querySelector('.agent-row-menu-sep')).not.toBeNull();
  });

  it('行 ⋯ 菜单：工程便签入口上抛 onOpenNotes（agent-hub-notes，悬浮卡归 App 层）', async () => {
    const onOpenNotes = vi.fn();
    renderList({ onOpenNotes });
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '工程便签' }));
    await waitFor(() => expect(onOpenNotes).toHaveBeenCalledWith(FIXTURE[1]));
  });

  it('AgentRow ⋯ 菜单：pi 行不显示 Logs/历史会话（R4：两者均为 claude 专属，其余驱动点开必败）', () => {
    const pi: AgentSummary = { ...mk('hub', 'Hub 助手', 'other'), driver: 'pi', kind: 'assistant' };
    render(<AgentRow agent={pi} onSelect={vi.fn()} onAction={vi.fn()} />, { wrapper });
    fireEvent.click(screen.getByRole('button', { name: /Hub 助手 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: 'Logs' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: '历史会话' })).toBeNull();
    expect(screen.getByRole('menuitem', { name: '工程便签' })).not.toBeNull();
  });

  it('AgentRow ⋯ 菜单：interactive 会话隐藏中断/Stop/Respawn/删除，保留「从列表移除」（R8）+ IC 徽标', () => {
    const inter: AgentSummary = {
      ...mk('ai', '交互会话', 'working'),
      kind: 'interactive',
      sessionId: 'sid-interactive-1',
    };
    const { container } = render(
      <AgentRow agent={inter} onSelect={vi.fn()} onAction={vi.fn()} />,
      { wrapper },
    );
    // R8：interactive 前台会话徽标双字母 IC（区别 bg 的 C）
    expect(container.querySelector('.driver-badge')?.textContent).toBe('IC');
    expect(container.querySelector('.driver-badge--wide')).not.toBeNull();
    fireEvent.click(screen.getByRole('button', { name: /交互会话 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: '中断' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Stop' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Respawn' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: '删除会话' })).toBeNull();
    // Logs 仍可用；unlist 属危险区 → claude 行恒有分隔线
    expect(screen.getByRole('menuitem', { name: 'Logs' })).not.toBeNull();
    expect(screen.getByRole('menuitem', { name: '从列表移除' })).not.toBeNull();
    expect(document.querySelector('.agent-row-menu-sep')).not.toBeNull();
    // bg 行徽标仍为单字母 C
    const bg = render(<AgentRow agent={FIXTURE[1]} onSelect={vi.fn()} onAction={vi.fn()} />, {
      wrapper,
    });
    expect(bg.container.querySelector('.driver-badge')?.textContent).toBe('C');
  });

  it('AgentList unlist 接线：确认后按 sessionId 调 ignore 端点并失效清单（R8）', async () => {
    const { container } = renderList();
    fireEvent.click(screen.getByRole('button', { name: /dev-f1 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '从列表移除' }));
    // 确认弹窗出现并带警示
    expect(container.textContent).toContain('从 hub 列表移除');
    fireEvent.click(screen.getByRole('button', { name: '从列表移除' }));
    await waitFor(() =>
      expect(unlistAgentMock).toHaveBeenCalledWith(
        'itv-839daca8-a39',
        'claude',
        '839daca8-a398-48b6-9ba2-5a0dec0daae1',
      ),
    );
  });

  it('AgentRow ⋯ 菜单：acp 行显示「删除会话」并接线 removeAgent（#45 移除通道）', async () => {
    renderList();
    // acp 行（FIXTURE acp1）此前无移除项（/rm 对 acp 404）——补通道后可删
    fireEvent.click(screen.getByRole('button', { name: /omp 会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '删除会话' }));
    // 确认弹窗（危险操作 Confirm，label = confirm.remove.label「Remove」）→ 确认后按 (id, driver) 调 rm 端点
    fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(removeAgentMock).toHaveBeenCalledWith('acp1', 'acp'));
  });

  // review-1 MINOR 1：移除项判定收紧为 claude + acp——omp 文件行（只读事实源）
  // 及 pi/opencode/dsh（remove 均 unsupported）不得误显「删除会话」，危险区为空
  // 也不得剩悬空分隔线
  it('AgentRow ⋯ 菜单：omp 文件行无「删除会话」与危险区分隔线（review-1 MINOR 1）', () => {
    render(
      <AgentRow
        agent={{ ...mk('omp-file-1', 'omp 文件行', 'other'), driver: 'omp', kind: 'omp' }}
        onSelect={vi.fn()}
        onAction={vi.fn()}
      />,
      { wrapper },
    );
    fireEvent.click(screen.getByRole('button', { name: /omp 文件行 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: '删除会话' })).toBeNull();
    expect(document.querySelector('.agent-row-menu-sep')).toBeNull();
    expect(screen.getByRole('menuitem', { name: '中断' })).not.toBeNull();
  });

  // #20：Esc 关闭行菜单（document 级监听——焦点在 body 也生效，原形态 Esc 全失效）
  // + 关闭后首击直达下层真实元素（backdrop 随菜单卸载，不吞点击）
  it('行 ⋯ 菜单：Esc 关闭（焦点在 body），关闭后首击落到行目标（#20）', () => {
    const onSelect = vi.fn();
    renderList({ onSelect });
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    expect(document.querySelector('.agent-row-menu')).not.toBeNull();
    // 模拟焦点在 body：keydown 直接派发到 document（不落在任何菜单内元素）
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(document.querySelector('.agent-row-menu')).toBeNull();
    expect(document.querySelector('.agent-row-menu-backdrop')).toBeNull();
    // 几何/事件断言的 jsdom 等价：backdrop 已卸载，同一次点击激活目标行
    fireEvent.click(screen.getByRole('button', { name: '运行中的会话' }));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it('AgentRow ⋯ 菜单：会话操作经 onAction 发射（中断/删除）', () => {
    const onAction = vi.fn();
    render(
      <AgentRow agent={FIXTURE[1]} onSelect={vi.fn()} onAction={onAction} />,
      { wrapper },
    );
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '中断' }));
    expect(onAction).toHaveBeenCalledWith('interrupt', FIXTURE[1]);
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '删除会话' }));
    expect(onAction).toHaveBeenCalledWith('remove', FIXTURE[1]);
  });

  it('行 ⋯ 菜单：Logs / 中断 直接执行', async () => {
    logsMock.mockResolvedValue({ logs: 'QQQ\u0000log' });
    const alertSpy = vi.spyOn(window, 'alert').mockImplementation(() => {});
    renderList();
    // R9：等待组默认展开（a1 needs_input 行直接可见，无需先展开组）
    fireEvent.click(screen.getByRole('button', { name: /需要输入的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Logs' }));
    await waitFor(() => expect(logsMock).toHaveBeenCalledWith('a1', 'claude'));
    await waitFor(() => expect(alertSpy).toHaveBeenCalled());
    // 中断
    fireEvent.click(screen.getByRole('button', { name: /需要输入的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '中断' }));
    expect(interruptMock).toHaveBeenCalledWith('a1', 'claude');
  });

  it('行 ⋯ 菜单：删除会话走确认弹窗，确认后调 removeAgent 并回调 onRemoved', async () => {
    removeAgentMock.mockResolvedValue(undefined);
    const onRemoved = vi.fn();
    const { container } = renderList({ selectedId: 'a1', onRemoved });
    // R9：a1 在等待组（默认展开），直接操作行菜单
    fireEvent.click(screen.getByRole('button', { name: /需要输入的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '删除会话' }));
    // 确认弹窗出现（复用 ConfirmDialog 危险型），确认执行
    fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(removeAgentMock).toHaveBeenCalledWith('a1', 'claude'));
    await waitFor(() => expect(onRemoved).toHaveBeenCalledWith('a1'));
    expect(container.querySelector('.agent-row-menu')).toBeNull();
  });
});

describe('AgentList 过滤安全底线（反馈轮 24-A）', () => {
  const expandAll = (container: HTMLElement): void => {
    // R9：仅「已完成」默认折叠；循环点仍折叠的头，行才进 DOM
    [...container.querySelectorAll('.agent-group-head[aria-expanded="false"]')].forEach((b) =>
      fireEvent.click(b),
    );
  };

  it('working/needs_input 会话永不被工程过滤隐藏', () => {
    // 勾选一个与会话 cwd 无关的族 → 空闲/已完成被滤（completed 组空隐藏），
    // 但 working/needs_input 恒全显（展开折叠组后行可见）
    const { container } = renderList({ checked: new Set(['/some/other/family']) });
    expandAll(container);
    expect(screen.getByText('运行中的会话')).toBeTruthy();
    expect(screen.getByText('需要输入的会话')).toBeTruthy();
    expect(container.querySelector('.agent-group')?.textContent).not.toContain('已完成的会话');
    // 已完成组整组被滤空 → 组头都不渲染
    expect(
      [...container.querySelectorAll('.agent-group-head')].some((h) =>
        h.textContent?.includes('已完成'),
      ),
    ).toBe(false);
  });

  it('shows the hidden-count hint and clears the filter on click', () => {
    // a3 已完成 + acp1 空闲（批4 并入空闲桶，随桶被滤）共 2 个会话被隐藏 → 提示条计数 2
    const onClearFilter = vi.fn();
    renderList({ checked: new Set(['/some/other/family']), onClearFilter });
    const hint = screen.getByRole('button', { name: /另有 3 个会话被工程过滤隐藏/ });
    expect(hint.textContent).toContain('点击全部显示');
    fireEvent.click(hint);
    expect(onClearFilter).toHaveBeenCalledTimes(1);
  });
});

describe('侧栏会话操作按驱动分流（ocr-review 中）', () => {
  it('ACP 行菜单隐藏 claude 专属操作（Logs/Stop/Respawn），保留 工程便签/中断/删除会话（#45 移除通道）', () => {
    renderList();
    fireEvent.click(screen.getByRole('button', { name: /omp 会话 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: 'Logs' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Stop' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Respawn' })).toBeNull();
    // #45：删除会话已对 acp 键实装（/api/agents/acp/{id}/rm），菜单补此项；
    // omp 文件行（driver='omp'）仍只读无删除项
    expect(screen.getByRole('menuitem', { name: '删除会话' })).not.toBeNull();
    expect(screen.getByRole('menuitem', { name: '工程便签' })).not.toBeNull();
    expect(screen.getByRole('menuitem', { name: '中断' })).not.toBeNull();
  });

  it('ACP 行中断/停用分流 cancelAcpSession，不落 claude 端点', () => {
    renderList();
    fireEvent.click(screen.getByRole('button', { name: /omp 会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '中断' }));
    expect(cancelAcpSessionMock).toHaveBeenCalledWith('acp1');
    expect(interruptMock).not.toHaveBeenCalledWith('acp1');
  });
});

describe('多实例侧栏状态分组（agent-list-status-grouping G1/G2）', () => {
  const rem = (id: string, group: AgentSummary['group'], instId: string | null): AgentSummary => ({
    ...mk(id, id, group),
    instanceId: instId,
  });
  const instA = { id: 'instA', name: '远程A', mode: 'direct' as const, baseUrl: 'https://a.example.com', config: null as never };
  const instB = { id: 'instB', name: '远程B', mode: 'direct' as const, baseUrl: 'https://b.example.com', config: null as never };
  const local = { id: null, name: '本机', mode: 'direct' as const, baseUrl: '', config: null as never };

  it('G1 去节点分组：跨实例 agents 按状态合并同组（无实例维度）', () => {
    const grouped = {
      needsInput: [rem('a1', 'needs_input', 'instA')],
      working: [rem('a2', 'working', null), rem('a3', 'working', 'instB')],
      completed: [],
      other: [],
    };
    const groups = buildStatusGroups([local, instA, instB], grouped, new Set(), [], 'all');
    expect(groups.find((x) => x.key === 'working')!.agents.map((a) => a.id)).toEqual(['a2', 'a3']);
    expect(groups.find((x) => x.key === 'needsInput')!.agents.map((a) => a.id)).toEqual(['a1']);
    // 输出无实例维度字段（分组头只剩状态组）
    expect(groups.every((g) => 'key' in g && 'label' in g && 'agents' in g)).toBe(true);
  });

  it('远程豁免族过滤保留（原语义等价）：工程过滤激活时远程空闲会话不消失', () => {
    const grouped = { needsInput: [], working: [], completed: [], other: [rem('r-idle', 'other', 'instA')] };
    const groups = buildStatusGroups([local, instA], grouped, new Set(['/proj/local']), ['/proj/local'], 'all');
    expect(groups.find((x) => x.key === 'other')!.agents.map((a) => a.id)).toEqual(['r-idle']);
  });

  it('G2 节点过滤：按实例/本机筛选各归其位（纯显示层）', () => {
    const grouped = {
      needsInput: [],
      working: [rem('a2', 'working', null), rem('a3', 'working', 'instB')],
      completed: [],
      other: [rem('r1', 'other', 'instA')],
    };
    const g = (f: string, key: string): string[] =>
      buildStatusGroups([local, instA, instB], grouped, new Set(), [], f)
        .find((x) => x.key === key)!
        .agents.map((a) => a.id);
    expect(g('instA', 'other')).toEqual(['r1']);
    expect(g('instA', 'working')).toEqual([]);
    expect(g('instB', 'working')).toEqual(['a3']);
    expect(g('local', 'working')).toEqual(['a2']);
    expect(g('all', 'working')).toEqual(['a2', 'a3']);
  });

  it('折叠键退化为组键（去实例维）——groupKeyOf 兼容保留', () => {
    expect(groupKeyOf(null, 'working')).toBe(':working');
    expect(groupKeyOf('instA', 'working')).toBe('instA:working');
  });
});

describe('Hub 助手独立分类（agent-hub-pi-hub-agent P7）', () => {
  const inst = { id: null, name: '本机', mode: 'direct' as const, baseUrl: '', config: null as never };
  const piAgent = (group: AgentSummary['group']): AgentSummary => ({
    ...mk('hub-assistant', 'hub-assistant', group),
    driver: 'pi',
    kind: 'pi',
    instanceId: null,
  });

  it('driver=pi 条目从四桶剔除，独立 hubAssistant 桶置顶', () => {
    const grouped = {
      needsInput: [],
      working: [{ ...mk('c1', 'c1', 'working') }],
      other: [piAgent('other')],
      completed: [],
    };
    const groups = buildStatusGroups([inst], grouped, new Set(), [], 'all');
    expect(groups[0].key).toBe('hubAssistant');
    expect(groups[0].agents.map((a) => a.id)).toEqual(['hub-assistant']);
    // 四桶不再含 pi 条目（不重复渲染）
    const fourBuckets = groups.filter((g) => g.key !== 'hubAssistant');
    expect(fourBuckets.every((g) => !g.agents.some((a) => a.driver === 'pi'))).toBe(true);
  });

  it('助手运行中（working 态）也独立成桶，不落入 working 桶', () => {
    const grouped = {
      needsInput: [],
      working: [piAgent('working'), { ...mk('c2', 'c2', 'working') }],
      other: [],
      completed: [],
    };
    const groups = buildStatusGroups([inst], grouped, new Set(), [], 'all');
    const hub = groups.find((g) => g.key === 'hubAssistant');
    expect(hub!.agents.map((a) => a.id)).toEqual(['hub-assistant']);
    const working = groups.find((g) => g.key === 'working')!;
    expect(working.agents.map((a) => a.id)).toEqual(['c2']);
  });

  it('助手不存在时无 hubAssistant 桶（空组不渲染）', () => {
    const grouped = { needsInput: [], working: [], other: [{ ...mk('x', 'x', 'other') }], completed: [] };
    const groups = buildStatusGroups([inst], grouped, new Set(), [], 'all');
    expect(groups.every((g) => g.key !== 'hubAssistant')).toBe(true);
  });
});

describe('AgentList 实例降级态（任务10 能力保留 + 去节点分组适配）', () => {
  const remoteCtx = { id: 'instR', name: '远程机', mode: 'direct' as const, baseUrl: 'https://r.example.com', config: null as never };
  const withRemote = (): void => {
    degradeState.instances = [
      { id: null, name: '本机', mode: 'direct', baseUrl: '', config: null },
      remoteCtx,
    ];
  };

  it('G2 离线节点 chip 可辨（· 离线 + 亮红标记）仍可选；本机行照常渲染；离线横幅带文字解释', () => {
    withRemote();
    degradeState.health = { instR: 'offline' };
    renderList();
    const chip = screen.getByRole('button', { name: /远程机/ });
    expect(chip.textContent).toContain('· 离线');
    expect(chip.className).toContain('node-chip--offline');
    // FIXTURE 会话全本机：列表照常
    expect(screen.getByText('运行中的会话')).toBeTruthy();
    // offline 显式文字横幅（原组头文案语义平移；置灰唯一形态在行上，此处是解释）
    expect(screen.getByText(/离线 · 持续无响应/)).toBeTruthy();
  });

  it('凭据失效：实例横幅可点跳设置页实例分区（D1 后唯一入口保留）', () => {
    withRemote();
    degradeState.authError = { instR: true };
    const onOpenSettings = vi.fn();
    renderList({ onOpenSettings });
    const badge = screen.getByRole('button', { name: /凭据失效/ });
    fireEvent.click(badge);
    expect(onOpenSettings).toHaveBeenCalledWith('instances');
  });

  it('离线节点零会话时 chip 仍渲染（降级态不静默消失——原组头语义平移）', () => {
    withRemote();
    degradeState.health = { instR: 'offline' };
    renderList();
    expect(screen.getByRole('button', { name: /远程机/ })).toBeTruthy();
  });

  it('degraded（agent-hub-instance-health）：chip 文案「清单刷新失败 · 点击重试」可点重试，行不置灰', () => {
    withRemote();
    degradeState.health = { instR: 'degraded' };
    degradeState.agents = [...FIXTURE, { ...mk('r1', '远程会话', 'working'), instanceId: 'instR' }];
    const invalidate = vi.spyOn(QueryClient.prototype, 'invalidateQueries');
    try {
      const { container } = renderList();
      // 横幅出现且可点
      const banner = screen.getByRole('button', { name: /清单刷新失败 · 点击重试/ });
      expect(banner.textContent).toContain('远程机');
      fireEvent.click(banner);
      // 立即重试 = invalidate 该实例 agents 查询
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ['agents', 'instR'] });
      // 行不置灰（degraded ≠ offline）：远程行保持正常透明度
      const remoteRow = [...container.querySelectorAll('.agent-row-wrap')].find((w) =>
        w.textContent?.includes('远程会话'),
      ) as HTMLElement;
      expect(remoteRow.className).not.toContain('agent-row--muted');
      // 不出现 offline 横幅
      expect(screen.queryByText(/离线 · 持续无响应/)).toBeNull();
    } finally {
      invalidate.mockRestore();
    }
  });
});

describe('侧栏去节点分组渲染（G1-G4）', () => {
  const twoNodes = (): void => {
    degradeState.instances = [
      { id: null, name: '本机', mode: 'direct', baseUrl: '', config: null },
      { id: 'instR', name: '远程机', mode: 'direct', baseUrl: 'https://r.example.com', config: null },
    ];
  };
  const mixedAgents = (): AgentSummary[] => [
    ...FIXTURE,
    { ...mk('r1', '远程会话', 'working'), instanceId: 'instR' },
  ];
  // chip 定位限定 .node-chip（G3 行后缀的可达名同样含节点名，role 查询会撞）
  const chipOf = (c: HTMLElement, name: string): HTMLButtonElement =>
    [...c.querySelectorAll<HTMLButtonElement>('.node-chip')].find((ch) =>
      ch.textContent?.includes(name),
    ) as HTMLButtonElement;

  it('G1 多实例下不再渲染实例分组头，状态组跨实例合并 + 过滤 chips 就位', () => {
    twoNodes();
    degradeState.agents = mixedAgents();
    const { container } = renderList();
    expect(container.querySelector('.inst-group-head')).toBeNull();
    const heads = [...container.querySelectorAll('.agent-group-head')].map((h) => h.textContent);
    expect(heads.some((t) => t?.includes('工作中'))).toBe(true);
    expect(screen.getByRole('button', { name: '全部' })).toBeTruthy();
    expect(chipOf(container, '远程机')).toBeTruthy();
    // 跨实例合并：工作中组同时含本机与远程会话
    const working = [...container.querySelectorAll('.agent-group')].find((g) =>
      g.querySelector('.agent-group-head')?.textContent?.includes('工作中'),
    ) as HTMLElement;
    expect(working.textContent).toContain('运行中的会话');
    expect(working.textContent).toContain('远程会话');
  });

  it('G2 切节点过滤：选离线节点只显其会话（行置灰），选全部恢复', () => {
    twoNodes();
    degradeState.health = { instR: 'offline' };
    degradeState.agents = mixedAgents();
    const { container } = renderList();
    // 全部视图：远程会话在且行置灰（离线实例行级 muted——G4）
    expect(screen.getByText('远程会话')).toBeTruthy();
    const mutedRow = [...container.querySelectorAll('.agent-row-wrap')].find((w) =>
      w.textContent?.includes('远程会话'),
    ) as HTMLElement;
    expect(mutedRow.className).toContain('agent-row--muted');
    // 选远程节点 chip → 只显该节点会话
    fireEvent.click(chipOf(container, '远程机'));
    expect(screen.getByText('远程会话')).toBeTruthy();
    expect(screen.queryByText('运行中的会话')).toBeNull();
    // 选全部恢复
    fireEvent.click(screen.getByRole('button', { name: '全部' }));
    expect(screen.getByText('运行中的会话')).toBeTruthy();
    expect(container.querySelector('.agent-row--muted')).not.toBeNull();
  });

  it('G3 多实例视图会话名带「· 节点名」后缀（本机/远端统一形态）', () => {
    twoNodes();
    degradeState.agents = mixedAgents();
    const { container } = renderList();
    const suffixes = [...container.querySelectorAll('.agent-node-suffix')].map((el) => el.textContent);
    expect(suffixes).toContain('· 本机');
    expect(suffixes).toContain('· 远程机');
  });

  it('G3 单本机视图无后缀（无歧义不添噪——需求裁量分支）', () => {
    const { container } = renderList();
    expect(container.querySelectorAll('.agent-node-suffix')).toHaveLength(0);
    expect(container.querySelectorAll('.agent-name').length).toBeGreaterThan(0);
  });

  it('assistant-singleton 特例：助手行不带节点后缀（一般 agent 后缀保持）', () => {
    twoNodes();
    degradeState.agents = [
      ...mixedAgents(),
      { ...mk('hub-assistant', 'Hub 助手', 'working'), driver: 'pi', instanceId: 'instR' },
    ];
    const { container } = renderList();
    const piRow = [...container.querySelectorAll('.agent-row-wrap')].find((w) =>
      w.textContent?.includes('Hub 助手'),
    ) as HTMLElement;
    expect(piRow).toBeTruthy();
    expect(piRow.querySelector('.agent-node-suffix')).toBeNull();
    // 一般 agent（claude）后缀语义不动
    const generalRow = [...container.querySelectorAll('.agent-row-wrap')].find((w) =>
      w.textContent?.includes('远程会话'),
    ) as HTMLElement;
    expect(generalRow.querySelector('.agent-node-suffix')).not.toBeNull();
  });
});
