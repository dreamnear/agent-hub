// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReactElement } from 'react';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// 可配置桩：默认全 null（抽屉遮罩用例不受影响）；会话选择回归用例按需注入交互渲染
const stubs = vi.hoisted(() => ({
  agents: [] as Array<{ id: string }>,
  renderAgentList: null as null | ((props: {
    onSelect: (a: { id: string }) => void;
  }) => ReactElement),
  renderQuickChat: null as null | ((props: { onPick: (a: { id: string }) => void }) => ReactElement),
  // ChatTab 桩每次挂载记录 agent id，用于断言挂载确实发生
  chatMounts: [] as string[],
  // 工程便签悬浮卡挂载记录（cwd）
  notesMounts: [] as string[],
}));

// App 渲染链全量替身：数据 hooks/面板组件各自发请求，此处只测抽屉遮罩（反馈轮 25-A）
vi.mock('./api', () => ({
  api: {
    listAgents: () => Promise.resolve([]),
    listProjects: () => Promise.resolve<string[]>([]),
    putProjects: () => Promise.resolve(),
  },
  captureUrlToken: () => false,
  clearStoredToken: () => {},
  onUnauthorized: () => {},
  openEventStream: () => () => {},
}));
vi.mock('./hooks/useAgents', () => ({
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
vi.mock('./components/AgentList', () => ({
  default: (props: { onSelect: (a: { id: string }) => void }) =>
    stubs.renderAgentList ? stubs.renderAgentList(props) : null,
}));
vi.mock('./components/ProjectSidebar', () => ({ default: () => null }));
// ChatTab 桩：只记录挂载（视图逻辑归 ChatTab 自身测试）
vi.mock('./components/ChatTab', async () => {
  const { useEffect } = await import('react');
  return {
    default: (props: { agent: { id: string } }) => {
      useEffect(() => {
        stubs.chatMounts.push(props.agent.id);
      }, []);
      return null;
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

describe('App 会话选择（终端入口移除后回归守卫）', () => {
  const agent = { id: 'agt-1' };
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
