// @vitest-environment happy-dom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReactElement } from 'react';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// 可配置桩：默认全 null（抽屉遮罩用例不受影响）；会话选择回归用例按需注入交互渲染
const stubs = vi.hoisted(() => ({
  agents: [] as Array<{ id: string }>,
  renderAgentList: null as null | ((props: {
    onSelect: (a: { id: string }) => void;
    onRemoved: (id: string) => void;
    onOpenSessions: (a: { id: string }) => void;
  }) => ReactElement),
  renderQuickChat: null as null | ((props: { onPick: (a: { id: string }) => void }) => ReactElement),
  // ChatTab 桩每次挂载记录 agent id，用于断言挂载确实发生
  chatMounts: [] as string[],
  // ChatTab 桩每次渲染记录 agent 展示名（#21 行名跟随断言用）
  chatNames: [] as string[],
  // AgentList 挂载计数（移动抽屉不重挂断言用）
  listMounts: [] as string[],
  // 工程便签悬浮卡挂载记录（cwd）
  notesMounts: [] as string[],
  // Git 面板（会话 cwd 版）经 App 回退 api 调用；记调用断言 cwd 跟随
  gitStatus: vi.fn(),
  // 历史会话弹窗（#18 原子切换）取数/恢复可配桩
  cliSessions: [] as unknown[],
  resume: vi.fn(),
  // 原地恢复（#21）/message 注入桩
  sendMessage: vi.fn(),
  // ChatTab 桩每次渲染记录草稿协议入参（per-key 喂回断言用）
  draftProps: { id: '', initialDraft: '', onDraftChange: undefined as undefined | ((t: string) => void) },
}));

// App 渲染链全量替身：数据 hooks/面板组件各自发请求，此处只测抽屉遮罩（反馈轮 25-A）
vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    open = vi.fn();
    dispose = vi.fn();
    write = vi.fn();
    onData = vi.fn();
    onResize = vi.fn();
    options: { theme: Record<string, string> } = { theme: {} };
  },
}));
vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn();
    rows = 24;
    cols = 80;
  },
}));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));
vi.mock('./api', () => ({
  api: {
    listAgents: () => Promise.resolve([]),
    listProjects: () => Promise.resolve<string[]>([]),
    putProjects: () => Promise.resolve(),
    gitStatus: (...a: unknown[]) => stubs.gitStatus(...a),
    gitDiff: () => Promise.resolve({ diff: '-old\n+new' }),
    submodules: () => Promise.resolve([]),
    createWorktree: () => Promise.resolve({ path: '/repo/.worktree/x' }),
    docsList: () => Promise.resolve([]),
    docsFile: () => Promise.resolve({ kind: 'text', name: 'm.txt', content: 'x', size: 1 }),
    terminalWsUrl: (cwd: string) => `ws://hub.test/ws/terminal?cwd=${encodeURIComponent(cwd)}`,
    cliSessions: () => Promise.resolve(stubs.cliSessions),
    resumeSession: (...a: unknown[]) => stubs.resume(...a),
    sendMessage: (...a: unknown[]) => stubs.sendMessage(...a),
  },
  captureUrlToken: () => false,
  clearStoredToken: () => {},
  onUnauthorized: () => {},
  openEventStream: () => () => {},
}));
vi.mock('./hooks/useAgents', () => ({
  familyAnchors: () => [], // App 侧历史分区 ctx 用（设置收敛批）
  useAgents: () => ({
    data: stubs.agents,
    grouped: { needsInput: [], working: [], completed: [], other: [] },
    isLoading: false,
    tree: [],
    registry: { instances: [{ id: null, name: '本机', mode: 'direct', baseUrl: '', config: null }], apiFor: () => null },
    instances: [{ id: null, name: '本机', mode: 'direct', baseUrl: '', config: null }],
    offline: {},
    authError: {},
  }),
}));
vi.mock('./components/AgentList', async () => {
  const { useEffect } = await import('react');
  return {
    default: (props: {
      onSelect: (a: { id: string }) => void;
      onRemoved: (id: string) => void;
      onOpenSessions: (a: { id: string }) => void;
    }) => {
      useEffect(() => {
        stubs.listMounts.push('m');
      }, []);
      return stubs.renderAgentList ? stubs.renderAgentList(props) : null;
    },
  };
});
vi.mock('./components/ProjectSidebar', () => ({ default: () => null }));
// ChatTab 桩：只记录挂载（视图逻辑归 ChatTab 自身测试）
vi.mock('./components/ChatTab', async () => {
  const { useEffect } = await import('react');
  return {
    default: (props: {
      agent: { id: string; name?: string | null };
      onOpenTerminal?: () => void;
      initialDraft?: string;
      onDraftChange?: (t: string) => void;
    }) => {
      useEffect(() => {
        stubs.chatMounts.push(props.agent.id);
      }, []);
      // 行名跟随（#21）：每次渲染记录最新 agent 展示名（nameOverrides 派生断言用）
      stubs.chatNames.push(props.agent?.name ?? '');
      // 草稿协议入参记录：per-key 喂回（initialDraft）与上行（onDraftChange）断言用
      stubs.draftProps = {
        id: props.agent.id,
        initialDraft: props.initialDraft ?? '',
        onDraftChange: props.onDraftChange,
      };
      return (
        <>
          {props.onOpenTerminal ? (
            <button type="button" data-testid="open-terminal" onClick={props.onOpenTerminal}>
              term
            </button>
          ) : null}
          <button
            type="button"
            data-testid="type-draft"
            onClick={() => props.onDraftChange?.('半截草稿')}
          >
            type
          </button>
        </>
      );
    },
  };
});
vi.mock('./components/StartDialog', () => ({ default: () => null }));
// 工程便签桩：记录挂载 cwd（钉住自动展开断言用）；isNotePinned 走真实现（读 localStorage）
vi.mock('./components/ProjectNotesDialog', async (importOriginal) => {
  const mod = await importOriginal<typeof import('./components/ProjectNotesDialog')>();
  return {
    ...mod,
    default: (props: { cwd: string }) => {
      stubs.notesMounts.push(props.cwd);
      return null;
    },
  };
});
vi.mock('./components/QuickChat', () => ({
  default: (props: { onPick: (a: { id: string }) => void }) =>
    stubs.renderQuickChat ? stubs.renderQuickChat(props) : null,
}));
vi.mock('./components/AgentsConfigPanel', () => ({ default: () => null }));
vi.mock('./components/TokenGate', () => ({ default: () => null }));

import App from './App';

const openDrawer = (): HTMLElement => {
  // 浮动 ☰（无会话态显示）打开左抽屉
  const { container } = render(
    <QueryClientProvider client={new QueryClient()}>
      <App />
    </QueryClientProvider>,
  );
  const btn = container.querySelector<HTMLButtonElement>('.sidebar-expand');
  if (!btn) throw new Error('sidebar-expand 未渲染');
  fireEvent.click(btn);
  return container;
};

afterEach(() => cleanup());

describe('App drawer overlay（反馈轮 25-A）', () => {
  it('overlay appears when the drawer opens and closes on click', () => {
    const container = openDrawer();
    expect(container.querySelector('.layout')?.className).toContain('left-open');
    const overlay = container.querySelector('.drawer-overlay');
    expect(overlay).not.toBeNull();
    fireEvent.click(overlay as HTMLElement);
    expect(container.querySelector('.drawer-overlay')).toBeNull();
    expect(container.querySelector('.layout')?.className).not.toContain('left-open');
  });

  it('Esc closes the open drawer', () => {
    const container = openDrawer();
    expect(container.querySelector('.drawer-overlay')).not.toBeNull();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(container.querySelector('.drawer-overlay')).toBeNull();
    expect(container.querySelector('.layout')?.className).not.toContain('left-open');
  });
});

describe('App 会话选择（终端入口移除后回归守卫）', () => {  const agent = { id: 'agt-1' };
  beforeEach(() => {
    stubs.agents = [agent];
    stubs.renderAgentList = null;
    stubs.renderQuickChat = null;
    stubs.chatMounts.length = 0; // 桩挂载记录是模块级数组，跨用例须重置
    stubs.notesMounts.length = 0;
    localStorage.clear();
  });
  const q = (container: HTMLElement, id: string): HTMLElement => {
    const el = container.querySelector<HTMLElement>(id);
    if (!el) throw new Error(`未渲染: ${id}`);
    return el;
  };

  it('行选中挂载 ChatTab', () => {
    stubs.renderAgentList = (props) => (
      <button type="button" data-testid="row" onClick={() => props.onSelect(agent)}>
        row
      </button>
    );
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(q(container, '[data-testid="row"]'));
    expect(stubs.chatMounts).toEqual(['agt-1']);
  });

  it('快速对话选会话挂载 ChatTab', () => {
    stubs.renderQuickChat = (props) => (
      <button type="button" data-testid="quick-pick" onClick={() => props.onPick(agent)}>
        pick
      </button>
    );
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.keyDown(window, { key: 'k', metaKey: true }); // ⌘K 快速对话
    fireEvent.click(q(container, '[data-testid="quick-pick"]'));
    expect(stubs.chatMounts).toEqual(['agt-1']);
  });

  it('P4 默认视图：数据到位且未选择时自动选中 Hub 助手（pi 条目）', async () => {
    stubs.agents = [
      { id: 'c1' },
      { id: 'hub-assistant', driver: 'pi', kind: 'pi', instanceId: null } as never,
    ];
    render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    await waitFor(() => {
      expect(stubs.chatMounts).toContain('hub-assistant');
    });
    expect(stubs.chatMounts).toEqual(['hub-assistant']);
  });

  it('P4 尊重用户选择：非空 selectedKey 不被默认选中覆盖', async () => {
    stubs.agents = [
      { id: 'c1' },
      agent,
      { id: 'hub-assistant', driver: 'pi', kind: 'pi', instanceId: null } as never,
    ];
    stubs.renderAgentList = (props) => (
      <button type="button" data-testid="row" onClick={() => props.onSelect(agent)}>
        row
      </button>
    );
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    // 默认选中先发生（助手挂载），随后用户手动切走
    await waitFor(() => {
      expect(stubs.chatMounts).toContain('hub-assistant');
    });
    fireEvent.click(q(container, '[data-testid="row"]'));
    expect(stubs.chatMounts).toEqual(['hub-assistant', 'agt-1']);
  });

  it('钉住的工程：选中其会话自动展开便签卡；未钉不自动开（agent-hub-notes）', () => {
    const pinnedAgent = { id: 'agt-pin', cwd: '/repo/pin' };
    const plainAgent = { id: 'agt-plain', cwd: '/repo/plain' };
    stubs.agents = [pinnedAgent, plainAgent];
    localStorage.setItem('notes_card_pin::/repo/pin', '1'); // 任务8：键带实例段（本机=空段）
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-pin" onClick={() => props.onSelect(pinnedAgent)}>
          pin
        </button>
        <button type="button" data-testid="row-plain" onClick={() => props.onSelect(plainAgent)}>
          plain
        </button>
      </>
    );
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    // 未钉工程：不自动开
    fireEvent.click(q(container, '[data-testid="row-plain"]'));
    expect(stubs.notesMounts).toEqual([]);
    // 钉住工程：选中即自动展开
    fireEvent.click(q(container, '[data-testid="row-pin"]'));
    expect(stubs.notesMounts).toEqual(['/repo/pin']);
  });
});

describe('App 底部栏面板显隐（Zed 式 toggle）', () => {
  beforeEach(() => {
    stubs.agents = [];
    stubs.renderAgentList = null;
    localStorage.clear();
  });
  const renderApp = (): { container: HTMLElement } =>
    render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );

  it('FAIL1 回归：点 Git → 点回 Files，终点回到首帧快照也必须写盘（reload 所见即所得）', () => {
    const { container } = renderApp();
    const btns = container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle');
    fireEvent.click(btns[2]); // Git
    fireEvent.click(btns[3]); // Files（回到快照态）
    const stored = JSON.parse(window.localStorage.getItem('hub_panels') ?? '{}') as {
      files?: boolean;
      git?: boolean;
    };
    expect(stored.files).toBe(true); // 中间值 {git:true,files:false} 不得残留
    expect(stored.git).toBe(false);
  });

  it('页签横滚契约（tripwire，FAIL2 回修）：tab 禁弹性收缩，溢出走条级横滚', () => {
    const css = readFileSync('src/App.css', 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
    const tab = css.match(/\.main-tab \{[^}]*\}/)?.[0] ?? '';
    expect(tab).toContain('flex-shrink: 0');
    expect(tab).toMatch(/min-width:\s*\d+px/);
    const item = css.match(/\.main-tab-item \{[^}]*\}/)?.[0] ?? '';
    expect(item).toContain('flex-shrink: 0');
    const bar = css.match(/\.main-tabs \{[^}]*\}/)?.[0] ?? '';
    expect(bar).toContain('overflow-x: auto');
  });

  it('默认全显首帧收敛为单视图（files 优先），BottomBar 四开关在位', () => {
    const { container } = renderApp();
    const layout = container.querySelector('.layout');
    if (!layout) throw new Error('layout 未渲染');
    // 桌面（happy-dom 视口 1024px）：单壳挂载；files 优先恢复（新功能默认可见）
    expect(layout.className).toContain('right-open');
    expect(layout.className).not.toContain('left-collapsed');
    expect(container.querySelector('.pane-right--files')).not.toBeNull();
    const btns = container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle');
    expect(btns).toHaveLength(4);
  });

  it('工程 toggle：files 开着点工程 = 互斥换装 projects（R1），再点全关；会话 toggle 切左栏折叠', () => {
    const { container } = renderApp();
    const layout = container.querySelector('.layout');
    if (!layout) throw new Error('layout 未渲染');
    const btns = container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle');
    fireEvent.click(btns[1]); // 工程：files → projects（互斥内生于单状态）
    expect(layout.className).toContain('right-open'); // 壳仍开
    expect(container.querySelector('.pane-right--files')).toBeNull(); // files 视图退场
    expect(container.querySelector('.pane-right')).not.toBeNull(); // 壳换装 projects
    fireEvent.click(btns[1]); // 再点：projects 关 → 壳卸载
    expect(layout.className).not.toContain('right-open');
    expect(container.querySelector('.pane-right')).toBeNull();
    fireEvent.click(btns[0]); // 会话
    expect(layout.className).toContain('left-collapsed');
  });

  it('Git toggle：git=false、files=false 首帧壳装 projects，点击实时换装 git 并写回 hub_panels', () => {
    localStorage.setItem('hub_panels', JSON.stringify({ sessions: true, projects: true, git: false, files: false }));
    const { container } = renderApp();
    expect(container.querySelector('.pane-right--git')).toBeNull();
    fireEvent.click(
      container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle')[2], // Git
    );
    expect(JSON.parse(localStorage.getItem('hub_panels') ?? '{}')).toMatchObject({ git: true });
    expect(container.querySelector('.pane-right--git')).not.toBeNull();
  });

  it('壳装 git 与工程视图互斥：projects=false、git=true、files=false 首帧只装 git（未选中占位）', () => {
    localStorage.setItem('hub_panels', JSON.stringify({ sessions: true, projects: false, git: true, files: false }));
    const { container } = renderApp();
    const gitPane = container.querySelector('.pane-right--git');
    expect(gitPane).not.toBeNull();
    expect(gitPane?.textContent).toContain('Git · —'); // 未选中占位（选中语义见 GitPanel）
  });

  it('宽度持久化两键互不干扰：壳装谁就吃谁的键（渲染消费端断言）', () => {
    // 装 projects：吃 hub_panel_right_w（--panel-w CSS 变量，App.css 769+ 段消费）
    localStorage.setItem('hub_panel_right_w', '400');
    localStorage.setItem('hub_panels', JSON.stringify({ sessions: true, projects: true, git: false, files: false }));
    const first = renderApp();
    expect(first.container.querySelector('.pane-right')?.getAttribute('style')).toContain('400px');
    cleanup();
    // 装 git：吃 hub_panel_git_w（直接内联 width，曾经 CSS 硬编码压掉 var 致拖宽失效，勿回退）
    localStorage.setItem('hub_panel_git_w', '300');
    localStorage.setItem('hub_panels', JSON.stringify({ sessions: true, projects: false, git: true, files: false }));
    const second = renderApp();
    expect((second.container.querySelector('.pane-right--git') as HTMLElement).style.width).toBe(
      '300px',
    );
  });

  it('hub_panels sessions=false：桌面首帧左栏卸载（pane-left 不挂载，回读断言）', () => {
    localStorage.setItem('hub_panels', JSON.stringify({ sessions: false, projects: true, git: true, files: false }));
    const { container } = renderApp();
    // left-collapsed 类保留（浮动 ☰ 桌面可见性依赖），pane-left 本体卸载
    expect(container.querySelector('.layout')?.className).toContain('left-collapsed');
    expect(container.querySelector('.pane-left')).toBeNull();
    // toggle 恢复：实时重挂载
    fireEvent.click(
      container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle')[0], // 会话
    );
    expect(container.querySelector('.pane-left')).not.toBeNull();
  });

  it('hub_panels projects=false：桌面首帧壳不渲染（PanelShell 卸载，宽度持久化值不受影响）', () => {
    localStorage.setItem('hub_panel_right_w', '400'); // 用户自定宽度
    localStorage.setItem(
      'hub_panels',
      JSON.stringify({ sessions: true, projects: false, git: false, files: false }),
    );
    const { container } = renderApp();
    expect(container.querySelector('.layout')?.className).not.toContain('right-open');
    expect(container.querySelector('.pane-right')).toBeNull();
    fireEvent.click(
      container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle')[1], // 工程开→关
    );
    fireEvent.click(
      container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle')[1], // 再开
    );
    expect(localStorage.getItem('hub_panel_right_w')).toBe('400'); // 尺寸键不被 toggle 破坏
  });
});

describe('App Git 面板跟随会话 cwd（Git 面板三改）', () => {
  const agentOf = (id: string, cwd: string | null, instanceId?: string) => ({
    id,
    driver: 'claude',
    group: 'working',
    name: null,
    cwd,
    kind: null,
    rawState: null,
    detail: null,
    tokens: null,
    startedAt: null,
    sessionId: null,
    instanceId: instanceId ?? null,
  });
  beforeEach(() => {
    stubs.agents = [];
    stubs.renderAgentList = null;
    stubs.chatMounts.length = 0;
    localStorage.clear();
    // files 优先恢复语义下,壳装 git 需显式关 files/projects（GitPanel 挂载前提）
    localStorage.setItem(
      'hub_panels',
      JSON.stringify({ sessions: true, projects: false, git: true, files: false }),
    );
    stubs.gitStatus.mockReset();
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [],
    });
  });
  const renderApp = (): { container: HTMLElement } =>
    render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );

  it('无会话 → Git 视图显示占位，不发 gitStatus 请求', () => {
    const { container } = renderApp();
    const gitPane = container.querySelector('.pane-right--git');
    expect(gitPane).not.toBeNull();
    expect(gitPane?.textContent).toContain('Git · —');
    expect(stubs.gitStatus).not.toHaveBeenCalled();
  });

  it('选中会话 → gitStatus 以其 cwd 重取；切换会话 → cwd 跟随（远端实例同cwd 也按实例隔离）', async () => {
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b', 'instX')];
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
      </>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(stubs.gitStatus).toHaveBeenCalledWith('/repo/a'));
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    await waitFor(() => expect(stubs.gitStatus).toHaveBeenCalledWith('/repo/b'));
    // 面板切到 b 后仍以 /repo/b 探询（30s refetch 语义 → queryKey 换轨）
    await waitFor(() => {
      const last = stubs.gitStatus.mock.calls.at(-1)?.[0];
      expect(last).toBe('/repo/b');
    });
  });

  it('点变更文件 → diff 页签（F4 v2 会话级单例）；ESC 关闭后 ChatTab 不重挂载', async () => {
    stubs.agents = [agentOf('a1', '/repo/a')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [{ x: ' ', y: 'M', path: 'm.txt', origPath: null }],
    });
    stubs.renderAgentList = (props) => (
      <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
        a
      </button>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-file-row')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    expect(container.querySelector('.git-diff-tab')?.textContent).toContain('m.txt');
    expect(container.querySelector('.main-tab--active')?.textContent).toContain('m.txt');
    expect(stubs.chatMounts).toEqual(['a1']);
    fireEvent.keyDown(window, { key: 'Escape' });
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).toBeNull());
    expect(container.querySelector('.main-tabs')).toBeNull(); // 无页签则条收起
    expect(stubs.chatMounts).toEqual(['a1']); // ChatTab 未重挂载
  });

  it('F4 v2 替换语义：同会话点另一文件 → diff 页签仍一个、内容换新；切会话整组保留切回恢复', async () => {
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [
        { x: ' ', y: 'M', path: 'm.txt', origPath: null },
        { x: ' ', y: 'M', path: 'n.txt', origPath: null },
      ],
    });
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
      </>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelectorAll('.git-file-row').length).toBe(2));
    fireEvent.click(container.querySelectorAll('.git-file-row')[0] as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')?.textContent).toContain('m.txt'));
    // 同会话点 n.txt：diff 页签仍一个、内容替换为 n（不增页签）
    fireEvent.click(container.querySelectorAll('.git-file-row')[1] as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')?.textContent).toContain('n.txt'));
    const diffTabs = container.querySelectorAll('.main-tab-item');
    const diffItem = Array.from(diffTabs).filter((el) => el.textContent?.includes('n.txt'));
    expect(diffItem.length).toBe(1); // 单例：替换而非新增
    // 切会话：diff 页签整组保留（隐藏不卸载）；切回恢复聚焦
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    expect(container.querySelectorAll('.git-diff-tab').length).toBe(1); // 隐藏常挂
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() =>
      expect(container.querySelector('.main-tab--active')?.textContent).toContain('n.txt'),
    );
  });

  it('F4 v2：diff 打开时切换 agent → 页签随组保留（不跨会话残留，各组面板隔离）', async () => {
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [{ x: ' ', y: 'M', path: 'm.txt', origPath: null }],
    });
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
      </>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-file-row')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    // 不关 diff 直接切 agent：a1 的 diff 页签随组隐藏保留，b1 纯聊天不被盖住
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    expect(container.querySelectorAll('.git-diff-tab').length).toBe(1); // 隐藏常挂
    const bActive = container.querySelector('.main-tab--active');
    expect(bActive == null || !(bActive.textContent ?? '').includes('m.txt')).toBe(true);
    expect(stubs.chatMounts).toEqual(['a1', 'b1']); // 新会话 chat 已挂载
  });

  it('F4 跳源文件：diff 点跳源 → diff 收 + 文件页签聚焦 + 右面板开文件树；关页签回聊天', async () => {
    stubs.agents = [agentOf('a1', '/repo/a')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [{ x: ' ', y: 'M', path: 'm.txt', origPath: null }],
    });
    stubs.renderAgentList = (props) => (
      <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
        a
      </button>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-file-row')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    // F4：点跳源 → 文件页签聚焦 + 右面板切 files；diff 页签保留（F4 v2 不再收 diff）
    const mountsBefore = stubs.chatMounts.length;
    fireEvent.click(container.querySelector('.git-diff-open-source') as HTMLButtonElement);
    expect(container.querySelector('.pane-right--files')).not.toBeNull();
    expect(container.querySelector('.main-tabs')).not.toBeNull();
    expect(container.querySelector('.main-tab--active')?.textContent).toContain('m.txt');
    expect(container.querySelector('.pane-slot--hidden')).not.toBeNull();
    expect(container.querySelector('.file-tab-panel--hidden')).toBeNull();
    expect(container.querySelector('.git-diff-tab')).not.toBeNull(); // diff 页签保留
    expect(container.querySelector('.git-diff-tab')?.closest('.pane-slot--hidden')).not.toBeNull();
    // 关文件页签 → 聚焦回聊天（diff 页签保留：跳源不冲掉 diff）
    fireEvent.click(container.querySelector('.main-tab-close') as HTMLButtonElement);
    expect(container.querySelector('.git-diff-tab')).not.toBeNull();
    const active = container.querySelector('.main-tab--active');
    expect(active == null || !(active.textContent ?? '').includes('m.txt')).toBe(true);
    // 全程 ChatTab 未重挂（Tab 化不得引入消息重取/状态重置回归）
    expect(stubs.chatMounts.length).toBe(mountsBefore);
  });

  it('F2 多文件页签并存互不冲掉；切走隐藏不卸载、聚焦切换不重挂聊天', async () => {
    stubs.agents = [agentOf('a1', '/repo/a')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [
        { x: ' ', y: 'M', path: 'm.txt', origPath: null },
        { x: ' ', y: 'M', path: 'n.txt', origPath: null },
      ],
    });
    stubs.renderAgentList = (props) => (
      <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
        a
      </button>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelectorAll('.git-file-row').length).toBe(2));
    const mountsBefore = stubs.chatMounts.length;
    // 开 m.txt 页签 → 底栏切回 Git 面板（单壳互斥：跳源后右面板已切 files，
    // git 树已卸载）→ 再开 n.txt 页签（两页签并存）
    fireEvent.click(container.querySelectorAll('.git-file-row')[0] as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-diff-open-source') as HTMLButtonElement);
    fireEvent.click(container.querySelectorAll<HTMLButtonElement>('.bottom-bar-toggle')[2]);
    await waitFor(() => expect(container.querySelectorAll('.git-file-row').length).toBe(2));
    fireEvent.click(container.querySelectorAll('.git-file-row')[1] as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-diff-open-source') as HTMLButtonElement);
    // F4 v2：diff 页签保留 → 页签 = [m.txt 文件, n.txt 文件, diff] 三个 item
    expect(container.querySelectorAll('.main-tab-item').length).toBe(3);
    expect(container.querySelectorAll('.file-tab-panel').length).toBe(2); // 两个文件面板都常挂
    // 切回聊天：聊天槽可见（diff 面板隐藏是合法态），两文件面板隐藏但未卸载
    fireEvent.click(container.querySelector('.main-tab') as HTMLButtonElement);
    // App.test 的 ChatTab 是桩（无 .chat-tab）：聚焦回聊天 = 有可见槽 + diff 槽隐藏
    expect(container.querySelector('.pane-slot:not(.pane-slot--hidden)')).not.toBeNull();
    expect(
      container.querySelector('.git-diff-tab')?.closest('.pane-slot--hidden'),
    ).not.toBeNull();
    expect(container.querySelectorAll('.file-tab-panel--hidden').length).toBe(2);
    // 再聚焦 m.txt：其面板恢复可见，n.txt 保持隐藏
    fireEvent.click(container.querySelectorAll('.main-tab')[1] as HTMLElement);
    expect(container.querySelectorAll('.file-tab-panel--hidden').length).toBe(1);
    expect(stubs.chatMounts.length).toBe(mountsBefore); // 全程聊天未重挂
  });

  it('F2 补强：文件页签按会话分组保留——切会话不销毁、切回即恢复聚焦', async () => {
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [{ x: ' ', y: 'M', path: 'm.txt', origPath: null }],
    });
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
      </>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-file-row')).not.toBeNull());
    // a1 开 m.txt 页签（经 diff 跳源）
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-diff-open-source') as HTMLButtonElement);
    expect(container.querySelector('.main-tab--active')?.textContent).toContain('m.txt');
    // 切 b1：a1 页签组整组保留（面板隐藏），b1 无页签 → 纯聊天无页签条
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    expect(container.querySelectorAll('.file-tab-panel').length).toBe(1); // 隐藏不卸载
    expect(container.querySelector('.main-tabs')).toBeNull();
    // 切回 a1：m.txt 页签恢复且聚焦
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    expect(container.querySelector('.main-tab--active')?.textContent).toContain('m.txt');
    // 手动关闭才销毁
    fireEvent.click(container.querySelector('.main-tab-close') as HTMLButtonElement);
    expect(container.querySelectorAll('.file-tab-panel').length).toBe(0);
  });
});

describe('App 移动抽屉（≤768px 回归：常挂载，开合不重挂 AgentList）', () => {
  const mqMobile = (): void => {
    vi.spyOn(window, 'matchMedia').mockImplementation((query: string | RegExp) => ({
      matches: typeof query === 'string' && query.includes('max-width'),
      addEventListener: () => {},
      removeEventListener: () => {},
    }) as unknown as MediaQueryList);
  };
  beforeEach(() => {
    stubs.agents = [];
    stubs.renderAgentList = null;
    stubs.listMounts.length = 0;
    stubs.chatMounts.length = 0;
    localStorage.clear();
    mqMobile(); // 视口 1024 的 happy-dom 压成手机
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('sessions=false 的手机：pane-left 首帧即挂载，开合抽屉零重挂，且手机不写 sessions 持久化', () => {
    localStorage.setItem('hub_panels', JSON.stringify({ sessions: false, projects: true, git: true }));
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    expect(container.querySelector('.pane-left')).not.toBeNull(); // 恒挂载
    expect(stubs.listMounts).toHaveLength(1);

    // ☰ 开抽屉：仅 left-open 类，不重挂、不写 sessions（桌面语义不上手机）
    fireEvent.click(container.querySelector('.sidebar-expand') as HTMLElement);
    expect(container.querySelector('.layout')?.className).toContain('left-open');
    expect(container.querySelector('.pane-left')).not.toBeNull();
    expect(stubs.listMounts).toHaveLength(1);
    expect(JSON.parse(localStorage.getItem('hub_panels') ?? '{}')).toMatchObject({ sessions: false });

    // 遮罩关闭：仍不重挂
    fireEvent.click(container.querySelector('.drawer-overlay') as HTMLElement);
    expect(container.querySelector('.layout')?.className).not.toContain('left-open');
    expect(container.querySelector('.pane-left')).not.toBeNull();
    expect(stubs.listMounts).toHaveLength(1);
  });

  it('迷你底栏：会话开左抽屉；Git 直达壳只装 git，遮罩点击必关（R2 死锁回归守卫）', () => {
    // 无会话选中（stubs.agents=[]）= 工单原始死锁场景：git 空态抽屉也曾无关闭路径
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    const bar = container.querySelector('.mini-bar');
    expect(bar).not.toBeNull();
    const btns = bar!.querySelectorAll<HTMLButtonElement>('.mini-bar-btn');
    expect(btns).toHaveLength(3);
    expect(container.textContent).toContain('会话');
    expect(container.textContent).toContain('Git');
    expect(container.textContent).toContain('文件');

    fireEvent.click(btns[0]); // 会话
    expect(container.querySelector('.layout')?.className).toContain('left-open');
    fireEvent.click(container.querySelector('.drawer-overlay') as HTMLElement);
    expect(container.querySelector('.layout')?.className).not.toContain('left-open');

    fireEvent.click(btns[1]); // Git 直达：抽屉仅 git（R3）
    const layout = container.querySelector('.layout');
    expect(layout?.className).toContain('right-open');
    expect(container.querySelector('.pane-right--git')).not.toBeNull();
    // R2：git 空态（无会话）也有全屏遮罩，点击必关——死锁不复存在
    const overlay = container.querySelector('.drawer-overlay');
    expect(overlay).not.toBeNull();
    fireEvent.click(overlay as HTMLElement);
    expect(layout?.className).not.toContain('right-open');
    expect(container.querySelector('.pane-right')).toBeNull();

    // R3 同规（files-panel F4）：文件直达壳只装 files，遮罩点击必关
    fireEvent.click(btns[2]); // 文件
    expect(layout?.className).toContain('right-open');
    expect(container.querySelector('.pane-right--files')).not.toBeNull();
    fireEvent.click(container.querySelector('.drawer-overlay') as HTMLElement);
    expect(layout?.className).not.toContain('right-open');
  });

  it('移动首帧不写 hub_panels（touched 守卫：手机打开 app 不冲桌面面板偏好，R4）', () => {
    localStorage.setItem(
      'hub_panels',
      JSON.stringify({ sessions: true, projects: true, git: true }),
    );
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    // 首帧渲染（单状态收敛为 null，抽屉全关）不得触发持久化写回
    expect(JSON.parse(localStorage.getItem('hub_panels') ?? '{}')).toMatchObject({
      sessions: true,
      projects: true,
      git: true,
    });
    // 用户真实操作（Git 直达开抽屉）后才同步写回
    fireEvent.click(
      container.querySelectorAll<HTMLButtonElement>('.mini-bar-btn')[1],
    );
    expect(JSON.parse(localStorage.getItem('hub_panels') ?? '{}')).toMatchObject({
      projects: false,
      git: true,
    });
  });
});

describe('App 底部栏（桌面语境：MiniBar 不渲染）', () => {
  beforeEach(() => {
    stubs.agents = [];
    stubs.renderAgentList = null;
    localStorage.clear();
  });

  it('桌面不渲染迷你底栏（走全功能 BottomBar）', () => {
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    expect(container.querySelector('.mini-bar')).toBeNull();
    expect(container.querySelector('.bottom-bar')).not.toBeNull();
  });
});

/// 花括号配对提取规则块（媒体查询可嵌套，正则到首个 } 会截断）；
/// 收集全部同名块（App.css 有多个 ≤768 媒体查询段）拼接返回
const cssBlocks = (css: string, header: string): string => {
  let out = '';
  let from = 0;
  for (;;) {
    const start = css.indexOf(header, from);
    if (start < 0) return out;
    const open = css.indexOf('{', start);
    let depth = 0;
    let end = -1;
    for (let i = open; i < css.length; i++) {
      if (css[i] === '{') depth++;
      else if (css[i] === '}') {
        depth--;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    if (end < 0) return out;
    out += css.slice(open + 1, end);
    from = end + 1;
  }
};

describe('App.css 静态守卫（happy-dom 不算布局，纯 CSS 盲区 vitest 测不到）', () => {
  it('≤768px 段 .drawer-overlay 必须重声明 display:block（桌面 display:none 同特异性级联全局，漏写遮罩 0×0 不可见=r1 实机 BLOCKER）', () => {
    // vitest root = web/（npm test 口径），import.meta.url 经 vite transform 非 file
    // scheme 不可靠，走 cwd 解析
    const css = readFileSync(resolve(process.cwd(), 'src/App.css'), 'utf8');
    const mobile = cssBlocks(css, '@media (max-width: 768px)').replace(/\/\*[\s\S]*?\*\//g, '');
    expect(mobile).not.toBe('');
    const overlay = mobile.match(/\.drawer-overlay\s*\{([^}]*)\}/)?.[1] ?? '';
    expect(overlay).toMatch(/display:\s*block/);
  });
});

describe('App 终端页签（agent-hub-terminal T1/T2/T3）', () => {
  const agentOf = (id: string, cwd: string | null) => ({
    id,
    driver: 'claude',
    group: 'working',
    name: null,
    cwd,
    kind: null,
    rawState: null,
    detail: null,
    tokens: null,
    startedAt: null,
    sessionId: null,
    instanceId: null,
  });
  const renderApp = (): { container: HTMLElement } =>
    render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
  // 最小 WS 假体：不发 open（App 层只关心 DOM 语义；xterm/WS 细归 TerminalTab.test）
  class WsFake {
    url: string;
    close = vi.fn();
    send = vi.fn();
    onopen: (() => void) | null = null;
    onmessage: ((ev: unknown) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor(url: string) {
      this.url = url;
    }
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('入口开终端页签（cwd=会话 cwd）；关闭回聊天；切会话整组保留、切回恢复聚焦（F2 补强）', async () => {
    vi.stubGlobal('WebSocket', WsFake as unknown as typeof WebSocket);
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b')];
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
      </>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() =>
      expect(container.querySelector('[data-testid="open-terminal"]')).not.toBeNull(),
    );
    // T1：入口 → 页签条出现（聊天 + 终端），终端面板挂载、cwd 取会话 cwd
    fireEvent.click(container.querySelector('[data-testid="open-terminal"]') as HTMLElement);
    expect(container.querySelector('.main-tabs')).not.toBeNull();
    expect(container.querySelector('.main-tab--active')?.textContent).toContain('终端');
    expect(container.querySelector('.terminal-tab')).not.toBeNull();
    expect(container.querySelector('.terminal-cwd')?.textContent).toContain('/repo/a');
    expect(container.querySelector('.pane-slot--hidden')).not.toBeNull(); // 聊天隐藏
    // T2：关终端页签 → 回聊天（页签条收起，聊天槽恢复）
    fireEvent.click(
      container.querySelector('[aria-label="关闭页签 终端"]') as HTMLButtonElement,
    );
    expect(container.querySelector('.terminal-tab')).toBeNull();
    expect(container.querySelector('.main-tabs')).toBeNull();
    expect(container.querySelector('.pane-slot--hidden')).toBeNull();
    // F2 补强（Tab 组按会话分组）：切会话 a1 终端整组保留——面板隐藏不卸载，
    // b1 无终端（聊天槽可见）；切回 a1 终端页签恢复聚焦
    fireEvent.click(container.querySelector('[data-testid="open-terminal"]') as HTMLElement);
    expect(container.querySelector('.terminal-tab')).not.toBeNull();
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    const bActive = container.querySelector('.main-tab--active');
    expect(bActive == null || !(bActive.textContent ?? '').includes('终端')).toBe(true);
    expect(container.querySelectorAll('.terminal-tab').length).toBe(1); // a1 终端仍挂载
    const a1Slot = container.querySelector('.terminal-tab')?.closest('.pane-slot');
    expect(a1Slot?.classList.contains('pane-slot--hidden')).toBe(true); // 隐藏保活
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() =>
      expect(container.querySelector('.main-tab--active')?.textContent).toContain('终端'),
    ); // 切回即恢复聚焦（组 active 保留）
  });

  it('MINOR 回修：agent 真删除清其页签组（面板卸载回收 PTY）；未删除会话组不受影响', async () => {
    // 壳装 git 面板（经 .git-file 开 diff 跳源的前提）
    localStorage.setItem(
      'hub_panels',
      JSON.stringify({ sessions: true, projects: false, git: true, files: false }),
    );
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [{ x: ' ', y: 'M', path: 'm.txt', origPath: null }],
    });
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
        <button type="button" data-testid="rm-a" onClick={() => props.onRemoved('a1')}>
          rm
        </button>
      </>
    );
    const { container } = renderApp();
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-file-row')).not.toBeNull());
    // a1 开 m.txt 页签（经 diff 跳源）
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-diff-open-source') as HTMLButtonElement);
    expect(container.querySelectorAll('.file-tab-panel').length).toBe(1);
    // b1 也留一组（终端），证明清理只作用于被删会话
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    fireEvent.click(container.querySelector('[data-testid="open-terminal"]') as HTMLElement);
    expect(container.querySelectorAll('.file-tab-panel').length).toBe(1); // a 的隐藏常挂
    // 真删除 a1：其组清空（面板卸载回收），b1 的终端组保留
    fireEvent.click(container.querySelector('[data-testid="rm-a"]') as HTMLElement);
    expect(container.querySelectorAll('.file-tab-panel').length).toBe(0);
    expect(container.querySelector('.terminal-tab')).not.toBeNull(); // b1 终端完好
  });

  it('BLOCKER 回归：双会话 diff 并存，ESC 只关当前会话的 diff（隐藏组不误关）', async () => {
    localStorage.setItem(
      'hub_panels',
      JSON.stringify({ sessions: true, projects: false, git: true, files: false }),
    );
    stubs.agents = [agentOf('a1', '/repo/a'), agentOf('b1', '/repo/b')];
    stubs.gitStatus.mockResolvedValue({
      branch: 'main',
      upstream: null,
      ahead: 0,
      behind: 0,
      files: [{ x: ' ', y: 'M', path: 'm.txt', origPath: null }],
    });
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(stubs.agents[0])}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(stubs.agents[1])}>
          b
        </button>
      </>
    );
    const { container } = renderApp();
    // a 开 diff（m.txt）→ 切 b 开 diff（m.txt，各组一个）
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-file-row')).not.toBeNull());
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.git-diff-tab')).not.toBeNull());
    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelectorAll('.git-file-row').length).toBe(1));
    fireEvent.click(container.querySelector('.git-file-row') as HTMLElement);
    await waitFor(() => {
      const visibleDiff = [...container.querySelectorAll('.pane-slot:not(.pane-slot--hidden)')]
        .filter((s) => s.querySelector('.git-diff-tab'));
      expect(visibleDiff.length).toBe(1); // b 的 diff 聚焦可见
    });
    // B 内按 ESC：只关 b 的 diff——a 的 diff 面板仍常挂（隐藏）
    fireEvent.keyDown(window, { key: 'Escape' });
    await waitFor(() => expect(container.querySelectorAll('.git-diff-tab').length).toBe(1));
    const aDiffSlot = container.querySelector('.git-diff-tab')?.closest('.pane-slot');
    expect(aDiffSlot?.classList.contains('pane-slot--hidden')).toBe(true); // a 的隐藏保留
    // 切回 a：diff 页签恢复聚焦（组语义未被 ESC 误伤）
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    await waitFor(() =>
      expect(container.querySelector('.main-tab--active')?.textContent).toContain('m.txt'),
    );
  });
});

describe('App 历史会话原子切换 + 草稿保留（agent-hub-pick-switch #18）', () => {
  const agentA = { id: 'agt-a', name: '小甲', sessionId: 'sess-a', instanceId: null } as never;
  const agentB = { id: 'agt-b', name: '小乙', sessionId: 'sess-b', instanceId: null } as never;
  const entryB = {
    sessionId: 'sess-b',
    mtimeSecs: 1_790_000_000,
    sizeBytes: 2048,
    branch: 'main',
    cwd: '/repo/proj',
    summary: '小乙的历史会话',
  };

  beforeEach(() => {
    stubs.agents = [agentA, agentB];
    stubs.chatMounts.length = 0;
    stubs.cliSessions = [entryB];
    stubs.resume = vi.fn().mockResolvedValue({ status: 'current' });
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(agentA)}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(agentB)}>
          b
        </button>
        <button type="button" data-testid="hist-b" onClick={() => props.onOpenSessions(agentB)}>
          b-history
        </button>
      </>
    );
  });

  it('current 路径：#18 根因回归——非选中行弹窗选中该行自己的当前会话 → 主区切到该 agent + toast', async () => {
    // 用户先在 A；从 B 行开历史、选中 B 自己的当前会话（服务端 current）
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    expect(stubs.chatMounts).toEqual(['agt-a']);

    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    // 弹窗标题带 agent 名（交互模型 5）
    expect(container.querySelector('.history-title')?.textContent).toBe('小乙 的历史会话');
    fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);

    // 主区切到 B（重挂载 = 原子切换的主区一环），弹窗关闭，toast 出现
    await waitFor(() => expect(stubs.chatMounts).toEqual(['agt-a', 'agt-b']));
    expect(container.querySelector('.sesshist-card')).toBeNull();
    expect(container.querySelector('.toast')?.textContent).toBe('已切换到 小乙的历史会话');
  });

  it('resumed 路径：新 bg 条目快照承接主区，toast 同现', async () => {
    stubs.resume = vi.fn().mockResolvedValue({ status: 'resumed', id: 'bg-new' });
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);
    await waitFor(() => expect(stubs.chatMounts).toContain('bg-new'));
    expect(container.querySelector('.toast')?.textContent).toBe('已切换到 小乙的历史会话');
  });

  it('恢复失败：视图不动、无 toast、弹窗留场显示错误（交互模型 2）', async () => {
    stubs.resume = vi.fn().mockRejectedValue(new Error('resume 挂了'));
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);
    await waitFor(() => expect(container.textContent).toContain('resume 挂了'));
    expect(stubs.chatMounts).toEqual(['agt-a']); // 停在原地
    expect(container.querySelector('.toast')).toBeNull();
    expect(container.querySelector('.sesshist-card')).not.toBeNull();
  });

  it('草稿跨切换保留：A 打字 → 切 B（不带出）→ 切回 A（草稿仍在）（交互模型 4）', async () => {
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    expect(stubs.draftProps.id).toBe('agt-a');
    expect(stubs.draftProps.initialDraft).toBe('');
    // A 输入半截草稿（上行到 App 的 per-key 表）
    fireEvent.click(container.querySelector('[data-testid="type-draft"]') as HTMLElement);

    fireEvent.click(container.querySelector('[data-testid="row-b"]') as HTMLElement);
    expect(stubs.draftProps.id).toBe('agt-b');
    expect(stubs.draftProps.initialDraft).toBe(''); // B 不串味

    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    expect(stubs.draftProps.id).toBe('agt-a');
    expect(stubs.draftProps.initialDraft).toBe('半截草稿'); // 切回 A 草稿复原
  });
});

describe('App 原地恢复（agent-hub-resume-inplace #21）', () => {
  // 两行都 claude、同工程、idle——满足注入判据的死会话场景
  const agentA = {
    id: 'agt-a', driver: 'claude', cwd: '/repo/proj', group: 'other',
    name: '小甲', sessionId: 'sess-a', instanceId: null,
  } as never;
  const agentB = {
    id: 'agt-b', driver: 'claude', cwd: '/repo/proj', group: 'other',
    name: '小乙', sessionId: 'sess-b', instanceId: null,
  } as never;
  const deadEntry = {
    sessionId: 'a1b2c3d4-e5f6-7890-abcd-ef0123456789', mtimeSecs: 1_790_000_000,
    sizeBytes: 2048, branch: 'main', cwd: '/repo/proj', summary: '死会话',
  }; // sessionId 须合法 UUID（注入白名单，round1 审查 MINOR 收紧）

  beforeEach(() => {
    stubs.agents = [agentA, agentB];
    stubs.chatMounts.length = 0;
    stubs.chatNames.length = 0;
    stubs.cliSessions = [deadEntry];
    stubs.resume = vi.fn().mockResolvedValue({ status: 'resumed', id: 'bg-new' });
    stubs.sendMessage = vi.fn().mockResolvedValue(undefined);
    stubs.renderAgentList = (props) => (
      <>
        <button type="button" data-testid="row-a" onClick={() => props.onSelect(agentA)}>
          a
        </button>
        <button type="button" data-testid="row-b" onClick={() => props.onSelect(agentB)}>
          b
        </button>
        <button type="button" data-testid="hist-b" onClick={() => props.onOpenSessions(agentB)}>
          b-history
        </button>
      </>
    );
  });

  it('死会话 + 选中行 idle → 注入当前 TUI 原地装载：行不变、resume-session 不打、toast 说明', async () => {
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);

    // 注入走 /message 管道打当前选中行；后端 spawn 路径未触发
    await waitFor(() => expect(stubs.sendMessage).toHaveBeenCalledTimes(1));
    expect(stubs.sendMessage).toHaveBeenCalledWith(
      'agt-a',
      `/resume ${deadEntry.sessionId}`,
      'claude',
    );
    expect(stubs.resume).not.toHaveBeenCalled();
    // 行不变：ChatTab 无重挂（无新 mount），弹窗关、toast 原地恢复
    expect(stubs.chatMounts).toEqual(['agt-a']);
    expect(container.querySelector('.sesshist-card')).toBeNull();
    expect(container.querySelector('.toast')?.textContent).toBe(
      '已在当前会话原地恢复「死会话」· 历史装载中',
    );
  });

  it('行名跟随：原地恢复后该行展示名切到被恢复会话名', async () => {
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);
    await waitFor(() => expect(stubs.sendMessage).toHaveBeenCalledTimes(1));
    // nameOverrides 派生 data → ChatTab 收到的 agent.name 变为会话名
    await waitFor(() => expect(stubs.chatNames.at(-1)).toBe('死会话'));
  });

  it('选中行忙（working）→ 回退 spawn 新行 + 专属 toast 说明（busy 策略 b）', async () => {
    const busyA = {
      id: 'agt-a', driver: 'claude', cwd: '/repo/proj', group: 'working',
      name: '小甲', sessionId: 'sess-a', instanceId: null,
    } as never;
    stubs.agents = [busyA, agentB];
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);

    await waitFor(() => expect(stubs.resume).toHaveBeenCalledTimes(1));
    expect(stubs.sendMessage).not.toHaveBeenCalled(); // 忙时注入会被吞，禁走
    expect(container.querySelector('.toast')?.textContent).toBe(
      '目标会话正忙，已新增行恢复「死会话」',
    );
  });

  it('装载失败（sessionId 未跟随）→ 有界超时明确报错 toast，视图不动', async () => {
    // 先真 timers 渲染到 item 就绪（react-query 首挂链需要 waitFor 轮询排干，
    // act 微任务两轮不够），再切 fake 推进 watcher 心跳/超时
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <App />
      </QueryClientProvider>,
    );
    fireEvent.click(container.querySelector('[data-testid="row-a"]') as HTMLElement);
    fireEvent.click(container.querySelector('[data-testid="hist-b"]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('.sesshist-item')).not.toBeNull());
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] });
    try {
      await act(async () => {
        fireEvent.click(container.querySelector('.sesshist-item') as HTMLElement);
      });
      expect(stubs.sendMessage).toHaveBeenCalledTimes(1);
      expect(container.querySelector('.toast')?.textContent).toContain('原地恢复');
      // toast 2.6s 自动消失 + watcher 15s 超时：推进到超时点
      await act(async () => {
        await vi.advanceTimersByTimeAsync(16_000);
      });
      expect(container.querySelector('.toast')?.textContent).toBe(
        '原地恢复未生效（会话可能装载失败），视图未变动',
      );
      expect(stubs.chatMounts).toEqual(['agt-a']); // 视图未动
    } finally {
      vi.useRealTimers();
    }
  });
});
