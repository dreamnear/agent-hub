// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import AgentList, { groupKeyOf, buildInstanceGroups } from './AgentList';
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
vi.mock('../api', () => ({
  api: {
    listAgents: () => Promise.resolve([]),
    removeAgent: (id: string) => removeAgentMock(id),
    interruptAgent: (id: string) => interruptMock(id),
    logs: (id: string) => logsMock(id),
    stopAgent: (id: string) => stopAgentMock(id),
    respawnAgent: (id: string) => respawnAgentMock(id),
    getNote: (...a: unknown[]) => getNoteMock(...a),
    putNote: (...a: unknown[]) => putNoteMock(...a),
    cancelAcpSession: (id: string) => cancelAcpSessionMock(id),
  },
  openEventStream: () => () => {},
}));

/// 侧栏会话操作按实例路由的 api 选择器（任务7 前置）：本机/未知 → 模块级 api mock。
const apiFor = vi.hoisted(() => () => ({
  interruptAgent: (id: string) => interruptMock(id),
  stopAgent: (id: string) => stopAgentMock(id),
  respawnAgent: (id: string) => respawnAgentMock(id),
  removeAgent: (id: string) => removeAgentMock(id),
  logs: (id: string) => logsMock(id),
  cancelAcpSession: (id: string) => cancelAcpSessionMock(id),
}));

/// 实例降级态的可变 mock（任务10 测试用）：默认全干净——离线/凭据失效均无，仅本机。
const degradeState = vi.hoisted(() => ({
  offline: {} as Record<string, boolean>,
  authError: {} as Record<string, boolean>,
  instances: [
    { id: null, name: '本机', mode: 'direct', baseUrl: '', config: null },
  ] as { id: string | null; name: string; mode: 'direct' | 'ssh-tunnel'; baseUrl: string; config: null }[],
}));

vi.mock('../hooks/useAgents', async (importOriginal) => {
  const mod = await importOriginal<typeof import('../hooks/useAgents')>();
  return {
    ...mod,
    useAgents: () => ({
      data: FIXTURE,
      grouped: mod.groupAgents(FIXTURE),
      isLoading: false,
      tree: [],
      instances: degradeState.instances,
      offline: degradeState.offline,
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
  startedAt: null,
  sessionId: null,
});

const FIXTURE: AgentSummary[] = [
  mk('a1', '需要输入的会话', 'needs_input'),
  mk('a2', '运行中的会话', 'working'),
  mk('a3', '已完成的会话', 'completed'),
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
      onOpenProjects={vi.fn()}
      onOpenConfig={vi.fn()}
      onStart={vi.fn()}
      onOpenSettings={vi.fn()}
      onRemoved={vi.fn()}
      onOpenNotes={vi.fn()}
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
  degradeState.offline = {};
  degradeState.authError = {};
  degradeState.instances = [{ id: null, name: '本机', mode: 'direct', baseUrl: '', config: null }];
});

describe('AgentList Sidebar-v2', () => {
  it('顶部品牌条：agent-hub 品牌 + 搜索/折叠图标', () => {
    renderList();
    expect(screen.getByText('agent-hub')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '搜索会话' }));
    fireEvent.click(screen.getByRole('button', { name: '折叠侧栏' }));
  });

  it('Sidebar-v3 可折叠分组：默认态按设计稿（工作/空闲展开，等待/完成折叠），工作中行带 accent 点', () => {
    // 反馈轮 18：组头 ▾/▸ + 计数；空组隐藏；折叠组只显组头
    const { container } = renderList();
    const heads = [...container.querySelectorAll('.agent-group-head')].map(
      (h) => `${h.textContent}`,
    );
    // ACP 会话（批4）并入状态桶：omp 空闲会话落「空闲」组；等待组折叠。无独立 ACP 分组。
    expect(heads).toEqual(['▾工作中1', '▾空闲1', '▸等待1', '▸已完成1']);
    // 默认折叠：等待/已完成只显组头；展开组行可见且 working 行带 accent 点
    const names = [...container.querySelectorAll('.agent-row .agent-name')].map(
      (el) => el.textContent,
    );
    expect(names).toEqual(['运行中的会话', 'omp 会话']);
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
    // 展开默认折叠的等待/完成两组
    for (const label of ['等待', '已完成']) {
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

  it('点击组头切换折叠：等待组展开显示行，再点收起', () => {
    const { container } = renderList();
    const waitHead = [...container.querySelectorAll('.agent-group-head')].find((h) =>
      h.textContent?.includes('等待'),
    ) as HTMLButtonElement;
    expect(waitHead.getAttribute('aria-expanded')).toBe('false');
    expect(container.textContent).not.toContain('需要输入的会话');
    fireEvent.click(waitHead);
    expect(waitHead.getAttribute('aria-expanded')).toBe('true');
    expect(container.textContent).toContain('需要输入的会话');
    fireEvent.click(waitHead);
    expect(container.textContent).not.toContain('需要输入的会话');
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

  it('底部功能菜单三项触发对应回调', () => {
    const onOpenProjects = vi.fn();
    const onOpenConfig = vi.fn();
    const onStart = vi.fn();
    renderList({ onOpenProjects, onOpenConfig, onStart });
    fireEvent.click(screen.getByRole('button', { name: '工程列表' }));
    fireEvent.click(screen.getByRole('button', { name: 'Agent 配置' }));
    fireEvent.click(screen.getByRole('button', { name: '新建 Agent' }));
    expect(onOpenProjects).toHaveBeenCalledTimes(1);
    expect(onOpenConfig).toHaveBeenCalledTimes(1);
    expect(onStart).toHaveBeenCalledTimes(1);
  });

  it('底部「设置」项与顶栏主题标记均跳设置页外观分区（agent-hub-settings B1/B3）', () => {
    const onOpenSettings = vi.fn();
    renderList({ onOpenSettings });
    fireEvent.click(screen.getByRole('button', { name: '设置' }));
    expect(onOpenSettings).toHaveBeenCalledWith('appearance');
    // 顶栏主题按钮：B3 起不再 cycle，唯一入口收口到设置页
    fireEvent.click(screen.getByRole('button', { name: /主题：/ }));
    expect(onOpenSettings).toHaveBeenCalledWith('appearance');
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
    const { container } = renderList({ onSelect });
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: '打开终端' })).toBeNull();
    // bg 会话（kind=background）全部会话操作可用
    for (const item of ['Logs', '工程便签', '中断', 'Stop', 'Respawn', '删除会话']) {
      expect(screen.getByRole('menuitem', { name: item })).not.toBeNull();
    }
    expect(container.querySelector('.agent-row-menu-sep')).not.toBeNull();
  });

  it('行 ⋯ 菜单：工程便签入口上抛 onOpenNotes（agent-hub-notes，悬浮卡归 App 层）', async () => {
    const onOpenNotes = vi.fn();
    renderList({ onOpenNotes });
    fireEvent.click(screen.getByRole('button', { name: /运行中的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '工程便签' }));
    await waitFor(() => expect(onOpenNotes).toHaveBeenCalledWith(FIXTURE[1]));
  });

  it('AgentRow ⋯ 菜单：interactive 会话隐藏中断/Stop/Respawn/删除（非 bg 任务）', () => {
    const inter: AgentSummary = { ...mk('ai', '交互会话', 'working'), kind: 'interactive' };
    const { container } = render(
      <AgentRow agent={inter} onSelect={vi.fn()} onAction={vi.fn()} />,
      { wrapper },
    );
    fireEvent.click(screen.getByRole('button', { name: /交互会话 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: '中断' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Stop' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Respawn' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: '删除会话' })).toBeNull();
    // Logs 仍可用；无危险分组 → 无分隔线
    expect(screen.getByRole('menuitem', { name: 'Logs' })).not.toBeNull();
    expect(container.querySelector('.agent-row-menu-sep')).toBeNull();
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
    // 展开等待组（a1 needs_input 在等待组）
    const el = [...document.querySelectorAll('.agent-group-head[aria-expanded="false"]')].find((b) => b.textContent?.includes('等待')) as HTMLButtonElement;
    fireEvent.click(el);
    fireEvent.click(screen.getByRole('button', { name: /需要输入的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Logs' }));
    await waitFor(() => expect(logsMock).toHaveBeenCalledWith('a1'));
    await waitFor(() => expect(alertSpy).toHaveBeenCalled());
    // 中断
    fireEvent.click(screen.getByRole('button', { name: /需要输入的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '中断' }));
    expect(interruptMock).toHaveBeenCalledWith('a1');
  });

  it('行 ⋯ 菜单：删除会话走确认弹窗，确认后调 removeAgent 并回调 onRemoved', async () => {
    removeAgentMock.mockResolvedValue(undefined);
    const onRemoved = vi.fn();
    const { container } = renderList({ selectedId: 'a1', onRemoved });
    // a1 在等待组（默认折叠）：先展开组头再操作
    fireEvent.click([...container.querySelectorAll('.agent-group-head')].find((h) => h.textContent?.includes('等待')) as HTMLButtonElement);
    fireEvent.click(screen.getByRole('button', { name: /需要输入的会话 更多操作/ }));
    fireEvent.click(screen.getByRole('menuitem', { name: '删除会话' }));
    // 确认弹窗出现（复用 ConfirmDialog 危险型），确认执行
    fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(removeAgentMock).toHaveBeenCalledWith('a1'));
    await waitFor(() => expect(onRemoved).toHaveBeenCalledWith('a1'));
    expect(container.querySelector('.agent-row-menu')).toBeNull();
  });
});

describe('AgentList 过滤安全底线（反馈轮 24-A）', () => {
  const expandAll = (container: HTMLElement): void => {
    // 等待/已完成组默认折叠只显组头：展开后行才进 DOM
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
    const hint = screen.getByRole('button', { name: /另有 2 个会话被工程过滤隐藏/ });
    expect(hint.textContent).toContain('点击全部显示');
    fireEvent.click(hint);
    expect(onClearFilter).toHaveBeenCalledTimes(1);
  });
});

describe('侧栏会话操作按驱动分流（ocr-review 中）', () => {
  it('ACP 行菜单隐藏 claude 专属操作（Logs/Stop/Respawn/删除会话），保留 工程便签/中断', () => {
    renderList();
    fireEvent.click(screen.getByRole('button', { name: /omp 会话 更多操作/ }));
    expect(screen.queryByRole('menuitem', { name: 'Logs' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Stop' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Respawn' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: '删除会话' })).toBeNull();
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

describe('多实例侧栏分组（任务6）', () => {
  const rem = (id: string, group: AgentSummary['group'], instId: string | null): AgentSummary => ({
    ...mk(id, id, group),
    instanceId: instId,
  });
  const instA = { id: 'instA', name: '远程A', mode: 'direct' as const, baseUrl: 'https://a.example.com', config: null as never };
  const instB = { id: 'instB', name: '远程B', mode: 'direct' as const, baseUrl: 'https://b.example.com', config: null as never };
  const local = { id: null, name: '本机', mode: 'direct' as const, baseUrl: '', config: null as never };

  it('各实例 agents 各归各实例组', () => {
    const grouped = {
      needsInput: [rem('a1', 'needs_input', 'instA')],
      working: [rem('a2', 'working', null), rem('a3', 'working', 'instB')],
      completed: [],
      other: [],
    };
    const groups = buildInstanceGroups([local, instA, instB], grouped, new Set(), []);
    expect(groups.find((g) => g.instId === null)!.groups.find((x) => x.key === 'working')!.agents.map((a) => a.id)).toEqual(['a2']);
    expect(groups.find((g) => g.instId === 'instA')!.groups.find((x) => x.key === 'needsInput')!.agents.map((a) => a.id)).toEqual(['a1']);
    expect(groups.find((g) => g.instId === 'instB')!.groups.find((x) => x.key === 'working')!.agents.map((a) => a.id)).toEqual(['a3']);
  });

  it('远程实例不套本机族锚：工程过滤激活时远程组整组保留（空闲桶也不消失）', () => {
    const grouped = { needsInput: [], working: [], completed: [], other: [rem('r-idle', 'other', 'instA')] };
    const groups = buildInstanceGroups([local, instA], grouped, new Set(['/proj/local']), ['/proj/local']);
    const instAGroups = groups.find((g) => g.instId === 'instA')!.groups;
    expect(instAGroups.find((x) => x.key === 'other')!.agents.map((a) => a.id)).toEqual(['r-idle']);
    // 本机无匹配会话 → 各桶空
    const localGroups = groups.find((g) => g.instId === null)!.groups;
    expect(localGroups.every((x) => x.agents.length === 0)).toBe(true);
  });

  it('折叠状态键按实例隔离（组头独立展开/收起互不影响）', () => {
    expect(groupKeyOf(null, 'working')).toBe(':working');
    expect(groupKeyOf('instA', 'working')).toBe('instA:working');
    expect(groupKeyOf(null, 'working') === groupKeyOf('instA', 'working')).toBe(false);
  });
});

describe('AgentList 实例降级态（任务10：离线/凭据失效）', () => {
  const remoteCtx = { id: 'instR', name: '远程机', mode: 'direct' as const, baseUrl: 'https://r.example.com', config: null };
  const withRemote = (): void => {
    degradeState.instances = [
      { id: null, name: '本机', mode: 'direct', baseUrl: '', config: null },
      remoteCtx,
    ];
  };

  it('离线实例组置灰 + 离线标记；本机组会话照常可见可操作', () => {
    withRemote();
    degradeState.offline = { instR: true };
    renderList();
    expect(screen.getByText('远程机')).toBeTruthy();
    expect(screen.getByText('· 离线')).toBeTruthy();
    // 本机组不受影响：会话行照常渲染
    expect(screen.getByText('运行中的会话')).toBeTruthy();
  });

  it('凭据失效：组头显示可点标记，点击跳设置页实例分区（D1 迁移后唯一入口）', () => {
    withRemote();
    degradeState.authError = { instR: true };
    const onOpenSettings = vi.fn();
    renderList({ onOpenSettings });
    const badge = screen.getByRole('button', { name: /凭据失效/ });
    fireEvent.click(badge);
    expect(onOpenSettings).toHaveBeenCalledWith('instances');
  });

  it('离线实例零会话时组头仍渲染（不静默消失——修复前 visible=0 直接 return null）', () => {
    // FIXTURE 会话全归本机；离线远程组零会话也必须可见
    withRemote();
    degradeState.offline = { instR: true };
    renderList();
    expect(screen.getByText('远程机')).toBeTruthy();
    expect(screen.getByText('· 离线')).toBeTruthy();
  });
});
