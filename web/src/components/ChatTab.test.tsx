// @vitest-environment happy-dom
import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactElement, ReactNode } from 'react';
import { api } from '../api';
import ChatTab from './ChatTab';
import type { AgentSummary, ChatMessage, SubagentEntry } from '../types';

interface SessionState {
  messages: ChatMessage[];
  isLoading: boolean;
  error: Error | null;
  refetch: () => void;
  hasMore: boolean;
  noMore: boolean;
  loadingOlder: boolean;
  loadOlder: () => Promise<boolean>;
}
const defaultSession = (
  _agentId: string,
  _sessionId: string | null,
  subagentId: string | null,
): SessionState => ({
  messages: subagentId ? SUBAGENT_MESSAGES : STALE_TASK_MESSAGES,
  isLoading: false,
  error: null,
  refetch: () => {},
  hasMore: false,
  noMore: false,
  loadingOlder: false,
  loadOlder: async () => false,
});
const useSessionMock = vi.fn(defaultSession);
vi.mock('../hooks/useSession', () => ({
  useSession: (...args: Parameters<typeof useSessionMock>) => useSessionMock(...args),
}));

const agent: AgentSummary = {
  driver: 'claude',
  id: 'empty-session',
  name: '空任务会话',
  cwd: null,
  kind: 'interactive',
  rawState: null,
  group: 'other',
  detail: null,
  tokens: null,
  startedAt: null,
  sessionId: 'empty-session',
};

// 已被 harness 清理的历史任务事件（留在消息流里，r27：不应被空数组回退复活）
const STALE_TASK_MESSAGES: ChatMessage[] = [
  {
    kind: 'tool_use',
    rawType: null,
    text: null,
    toolUseId: 'toolu_old',
    toolName: 'TaskCreate',
    input: { subject: '已清理的旧任务' },
    result: null,
    error: null,
    ts: null,
  },
  {
    kind: 'tool_result',
    rawType: null,
    text: 'Task #1 created successfully: 已清理的旧任务',
    toolUseId: 'toolu_old',
    toolName: null,
    input: null,
    result: null,
    error: null,
    ts: null,
  },
];

// subagent 会话消息（聚焦切换时 useSession 应切到子会话数据源）
const SUBAGENT_MESSAGES: ChatMessage[] = [
  { kind: 'user', rawType: null, text: '子会话派发 prompt', toolUseId: null, toolName: null, input: null, result: null, error: null, ts: null },
];

const SUBAGENTS: SubagentEntry[] = [
  {
    agentId: 'dr-planner-f9dcf57d',
    name: 'dr-planner',
    agentType: 'dr-planner',
    description: 'P1 计划转 tasks.md',
    model: null,
    status: 'completed',
    startedAt: null,
    lastActiveAt: null,
  },
];

afterEach(() => {
  cleanup();
  useSessionMock.mockImplementation(defaultSession);
  vi.restoreAllMocks();
});

describe('ChatTab task bar', () => {
  it('does not render a fixed task bar when the API and session both have no tasks', async () => {
    const tasksRequest = vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={agent} />);
    await act(async () => {
      await tasksRequest.mock.results[0].value;
    });
    expect(tasksRequest).toHaveBeenCalledWith(agent.id);
    expect(container.querySelector('.chat-list')).not.toBeNull();
    expect(container.querySelector('.task-bar')).toBeNull();
  });

  it('treats an authoritative empty task list as final and does not revive stale tasks', async () => {
    // r27：[] 是权威空快照——即使消息流里有历史任务事件也不得回退复活
    const tasksRequest = vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={agent} />);
    await act(async () => {
      await tasksRequest.mock.results[0].value;
    });
    expect(container.querySelector('.task-bar')).toBeNull();
  });

  it('falls back to the session task view when the server has no authoritative view', async () => {
    // null = 无权威视角 → 回退单会话口径，消息流里的任务渲染进固定栏
    const tasksRequest = vi.spyOn(api, 'agentTasks').mockResolvedValue(null);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={agent} />);
    await act(async () => {
      await tasksRequest.mock.results[0].value;
    });
    expect(container.querySelector('.task-bar')).not.toBeNull();
    expect(container.querySelector('.task-bar')?.textContent).toContain('已清理的旧任务');
  });
});

describe('ChatTab streaming label (反馈轮 28-C 裁决)', () => {
  it('shows the subagent-pending label when working but main jsonl is silent', async () => {
    // group=working 且 remoteActive=false（主 jsonl 静默）= 主 LLM 等待 subagent；
    // mount 时末条消息即触发 streamingPulse → 指示可见，文案应区分而非「正在输出」
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const workingAgent: AgentSummary = { ...agent, group: 'working' };
    const { container } = render(<ChatTab agent={workingAgent} />);
    await act(async () => {});
    const label = container.querySelector('.chat-streaming');
    expect(label).not.toBeNull();
    expect(label?.textContent).toContain('subagent 执行中 · 主会话待续');
    expect(label?.textContent).not.toContain('正在输出');
  });

  it('keeps the output label when remoteActive is true', async () => {
    // remoteActive=true（主 jsonl 30s 内有写入）= 真实输出中，文案不变
    vi.spyOn(api, 'sessionActive').mockResolvedValue(true);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const workingAgent: AgentSummary = { ...agent, group: 'working' };
    const { container } = render(<ChatTab agent={workingAgent} />);
    await act(async () => {});
    expect(container.querySelector('.chat-streaming')?.textContent).toContain('正在输出');
  });
});

describe('ChatTab input box', () => {
  it('starts compact at about one text line and caps growth with inner scroll', () => {
    // preview 反馈五：初始一行紧凑（防回胖到 2 行大空框），多行自适应增高封顶内滚
    const { container } = render(<ChatTab agent={agent} />);
    const textarea = container.querySelector('.chat-input textarea') as HTMLTextAreaElement;
    expect(textarea).not.toBeNull();
    expect(textarea.rows).toBe('1');
    const style = getComputedStyle(textarea);
    expect(style.minHeight).toBe('24px');
    expect(style.maxHeight).toBe('160px');
  });
});

describe('ChatTab subagent view', () => {
  it('hides the subagent card when the session has no subagents', async () => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={agent} />);
    expect(container.querySelector('details.subagent-card')).toBeNull();
    expect(container.querySelector('.chat-input')).not.toBeNull();
  });

  it('switches to the read-only subagent view on chip click and back on re-click', async () => {
    const subagentsRequest = vi.spyOn(api, 'subagents').mockResolvedValue(SUBAGENTS);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={agent} />);
    await act(async () => {
      await subagentsRequest.mock.results[0].value;
    });
    // 主会话态：输入框在，只读提示不在；折叠条默认收起且位于底部（与任务清单并列）
    expect(container.querySelector('.chat-input')).not.toBeNull();
    expect(container.querySelector('.chat-readonly-hint')).toBeNull();
    const card = container.querySelector('details.subagent-card') as HTMLDetailsElement;
    expect(card).not.toBeNull();
    expect(card.open).toBe(false);

    // 展开折叠条后点 chip → 聚焦子会话：useSession 切子数据源、输入框消失、只读提示出现
    card.open = true;
    fireEvent.click(screen.getByText('dr-planner'));
    const lastCall = useSessionMock.mock.calls.at(-1)!;
    expect(lastCall[0]).toBe(agent.id);
    expect(lastCall[2]).toBe('dr-planner-f9dcf57d');
    expect(container.querySelector('.chat-input')).toBeNull();
    expect(container.querySelector('.chat-readonly-hint')?.textContent).toContain('dr-planner');
    expect(container.querySelector('.chat-list')?.textContent).toContain('子会话派发 prompt');

    // 显式返回按钮 → 返回主会话
    fireEvent.click(screen.getByText('← 返回主会话'));
    expect(useSessionMock.mock.calls.at(-1)![2]).toBeNull();
    expect(container.querySelector('.chat-input')).not.toBeNull();
    expect(container.querySelector('.chat-readonly-hint')).toBeNull();

    // 再次进入后保留既有交互：再点当前 chip 也能返回
    fireEvent.click(screen.getByText('dr-planner'));
    fireEvent.click(screen.getByText('dr-planner'));
    expect(useSessionMock.mock.calls.at(-1)![2]).toBeNull();
    expect(container.querySelector('.chat-input')).not.toBeNull();
  });
});

describe('ChatTab session loading state', () => {
  const silenceApi = (): void => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
  };
  const sessionOnce = (session: Partial<SessionState>): void => {
    useSessionMock.mockImplementation(() => ({ ...defaultSession('', null, null), ...session }));
  };

  it('shows a loading hint while the history request is in flight', () => {
    // 反馈轮 11：点开大会话，历史 GET 在途 → 「正在加载会话…」，不落空态文案
    silenceApi();
    sessionOnce({ messages: [], isLoading: true, error: null });
    const { container } = render(<ChatTab agent={agent} />);
    expect(container.querySelector('.chat-session-loading')).not.toBeNull();
    expect(container.querySelector('.chat-empty')).toBeNull();
  });

  it('does not flash the loading hint when cached messages render instantly', () => {
    // 防闪烁：缓存命中秒开（isLoading=false）→ 无加载提示，直接渲染消息
    silenceApi();
    sessionOnce({ messages: SUBAGENT_MESSAGES, isLoading: false, error: null });
    const { container } = render(<ChatTab agent={agent} />);
    expect(container.querySelector('.chat-session-loading')).toBeNull();
    expect(container.querySelector('.chat-session-error')).toBeNull();
    expect(container.querySelector('.chat-list')?.textContent).toContain('子会话派发 prompt');
  });

  it('shows an empty state instead of loading when the history resolved to zero messages', () => {
    // 空会话：加载完成且 0 条 → 空态文案而非加载中
    silenceApi();
    sessionOnce({ messages: [], isLoading: false, error: null });
    const { container } = render(<ChatTab agent={agent} />);
    expect(container.querySelector('.chat-session-loading')).toBeNull();
    expect(container.querySelector('.chat-empty')?.textContent).toContain('暂无消息');
  });

  it('shows an error hint with a retry button that triggers refetch', () => {
    // 加载失败：错误提示 + 重试入口触发 refetch
    silenceApi();
    const refetch = vi.fn();
    sessionOnce({ messages: [], isLoading: false, error: new Error('boom'), refetch });
    const { container } = render(<ChatTab agent={agent} />);
    const box = container.querySelector('.chat-session-error');
    expect(box?.textContent).toContain('会话加载失败');
    fireEvent.click(box!.querySelector('button')!);
    expect(refetch).toHaveBeenCalledTimes(1);
  });

  it('applies the session spinner styling to the loading hint', () => {
    // CSS 回归（同输入框样式断言模式，happy-dom 只解析直接值不解析 var()）：
    // spinner 沿用 send-spin 动画与圆环形态
    silenceApi();
    sessionOnce({ messages: [], isLoading: true, error: null });
    const { container } = render(<ChatTab agent={agent} />);
    const el = container.querySelector('.chat-session-loading') as HTMLElement;
    expect(el.querySelector('.session-spinner')).not.toBeNull();
    const spinner = el.querySelector('.session-spinner') as HTMLElement;
    expect(getComputedStyle(spinner).borderRadius).toBe('999px');
    expect(getComputedStyle(spinner).width).toBe('14px');
    expect(getComputedStyle(spinner).height).toBe('14px');
  });
});

describe('ChatTab load older messages', () => {
  const silenceApi = (): void => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
  };
  const sessionOnce = (session: Partial<SessionState>): void => {
    useSessionMock.mockImplementation(() => ({ ...defaultSession('', null, null), ...session }));
  };

  it('shows the load-older button and triggers loadOlder on click', () => {
    // P6 B13：服务端确认有更早消息（hasMore）即显示按钮，点击触发翻页
    silenceApi();
    const loadOlder = vi.fn().mockResolvedValue(true);
    sessionOnce({ hasMore: true, loadOlder });
    const { container } = render(<ChatTab agent={agent} />);
    const btn = container.querySelector('.chat-load-older') as HTMLButtonElement;
    expect(btn).not.toBeNull();
    expect(btn.textContent).toContain('加载更早消息');
    fireEvent.click(btn);
    expect(loadOlder).toHaveBeenCalledTimes(1);
  });

  it('shows loading text while paging and a no-more hint when exhausted', () => {
    // 翻页在途：按钮禁用 + 加载中；全量拉回：「无更多消息」（B13）
    silenceApi();
    sessionOnce({ hasMore: true, loadingOlder: true });
    const { container } = render(<ChatTab agent={agent} />);
    const btn = container.querySelector('.chat-load-older') as HTMLButtonElement;
    expect(btn.textContent).toContain('加载中…');
    expect(btn.disabled).toBe(true);

    sessionOnce({ hasMore: false, noMore: true });
    const { container: c2 } = render(<ChatTab agent={agent} />);
    expect(c2.querySelector('.chat-load-older')).toBeNull();
    expect(c2.querySelector('.chat-no-more')?.textContent).toContain('无更多消息');
  });
});

describe('ChatTab view tabs (2026-09-22 终端移除 + 顶栏重排)', () => {
  it('renders exactly two tabs (Agent/文档) inside the topbar, terminal removed', () => {
    const { container } = render(<ChatTab agent={agent} />);
    const topbar = container.querySelector('.chat-area-topbar');
    expect(topbar).not.toBeNull();
    const tablist = topbar?.querySelector('[role="tablist"]');
    expect(tablist).not.toBeNull();
    const tabs = [...(tablist?.querySelectorAll('[role="tab"]') ?? [])];
    expect(tabs.map((t) => t.textContent)).toEqual(['Agent', '文档']);
    // 顶栏右侧不再有任何会话级操作按钮（Logs/中断/Stop/Remove/Respawn 全收进侧栏菜单）
    expect(container.textContent).not.toMatch(/Logs|Respawn|Remove|Stop|中断|终端/);
    // 内容区顶部不再有独立视图切换条
    expect(container.querySelector('.chat-area-topbar + .chat-view-toggle')).toBeNull();
  });
});

describe('ChatTab send experience', () => {
  interface Setup {
    container: HTMLElement;
    textarea: HTMLTextAreaElement;
    form: HTMLFormElement;
    rerender: (ui: ReactElement) => void;
    unmount: () => void;
  }
  const setup = (target: AgentSummary = agent): Setup => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const rr = render(<ChatTab agent={target} />);
    const container = rr.container;
    const textarea = container.querySelector('.chat-input textarea') as HTMLTextAreaElement;
    const form = container.querySelector('form.chat-input') as HTMLFormElement;
    return { container, textarea, form, rerender: rr.rerender, unmount: rr.unmount };
  };

  it('clears the input and shows an optimistic bubble immediately on submit', () => {
    // 反馈 7-1：不等 PTY 回流，同步清空 + 本地气泡
    const sendMessage = vi.spyOn(api, 'sendMessage').mockResolvedValue();
    const { container, textarea, form } = setup();
    fireEvent.change(textarea, { target: { value: '帮我跑个构建' } });
    fireEvent.submit(form);
    expect(textarea.value).toBe('');
    expect(container.textContent).toContain('帮我跑个构建');
    expect(sendMessage).toHaveBeenCalledWith(agent.id, '帮我跑个构建');
  });

  it('rolls back the optimistic bubble and restores the draft on failure', async () => {
    // 反馈 7-1 失败路径：撤气泡 + 还原文本 + 错误提示
    vi.spyOn(api, 'sendMessage').mockRejectedValue(new Error('boom'));
    const { container, textarea, form } = setup();
    fireEvent.change(textarea, { target: { value: '会失败的消息' } });
    fireEvent.submit(form);
    await act(async () => {});
    expect(container.querySelector('.chat-optimistic-bubble')).toBeNull();
    expect(textarea.value).toBe('会失败的消息');
    expect(container.querySelector('.chat-error')?.textContent).toContain('boom');
  });

  it('shows the queued bubble when sending while agent is working (反馈轮 24-C)', async () => {
    // working 期发送 = 入队：sendMessage 照发（PTY 注入由 server 放行）、
    // 排队气泡出现；乐观气泡对账落盘后气泡一并销账
    const workingAgent: AgentSummary = { ...agent, group: 'working' };
    const sendMessage = vi.spyOn(api, 'sendMessage').mockReturnValue(new Promise(() => {}));
    const { container, textarea, form, rerender } = setup(workingAgent);
    fireEvent.change(textarea, { target: { value: '排队消息' } });
    fireEvent.submit(form);
    expect(sendMessage).toHaveBeenCalledWith(workingAgent.id, '排队消息');
    expect(container.querySelector('.chat-queued-bubble')?.textContent).toContain(
      '已加入队列 · Agent 完成当前任务后处理',
    );
    // 消息落盘对账 → 排队气泡销账
    useSessionMock.mockImplementation(() => ({
      messages: [
        { kind: 'user' as const, rawType: null, text: '排队消息', toolUseId: null, toolName: null, input: null, result: null, error: null, ts: null },
      ],
      isLoading: false,
      error: null,
      refetch: () => {},
      hasMore: false,
      noMore: false,
      loadingOlder: false,
      loadOlder: async () => false,
    }));
    rerender(<ChatTab agent={workingAgent} />);
    await act(async () => {});
    expect(container.querySelector('.chat-queued-bubble')).toBeNull();
    void form;
  });

  it('clears the queued bubble when the agent leaves working (反馈轮 24-C)', async () => {
    const workingAgent: AgentSummary = { ...agent, group: 'working' };
    const idleAgent: AgentSummary = { ...agent, group: 'other' };
    vi.spyOn(api, 'sendMessage').mockResolvedValue();
    const { container, textarea, form, rerender } = setup(workingAgent);
    fireEvent.change(textarea, { target: { value: '排队二号' } });
    fireEvent.submit(form);
    await act(async () => {});
    expect(container.querySelector('.chat-queued-bubble')).not.toBeNull();
    rerender(<ChatTab agent={idleAgent} />);
    expect(container.querySelector('.chat-queued-bubble')).toBeNull();
    void form;
    void textarea;
  });

  it('shows a clickable spinner while working and interrupts on click', async () => {
    // 反馈 7-3（视觉修正）：工作中按钮 = 转圈（点击中断），非 ■ 文字按钮
    const workingAgent: AgentSummary = { ...agent, group: 'working' };
    const sendMessage = vi.spyOn(api, 'sendMessage').mockResolvedValue();
    const interrupt = vi.spyOn(api, 'interruptAgent').mockResolvedValue();
    const { container, textarea, form, rerender } = setup(workingAgent);
    fireEvent.change(textarea, { target: { value: '长任务' } });
    fireEvent.submit(form);
    await act(async () => {
      await sendMessage.mock.results[0]?.value;
    });
    const spinnerBtn = container.querySelector('.chat-send-btn') as HTMLButtonElement;
    expect(spinnerBtn).not.toBeNull();
    expect(spinnerBtn.querySelector('.send-spinner')).not.toBeNull();
    expect(spinnerBtn.getAttribute('aria-label')).toBe('中断');
    expect(spinnerBtn.getAttribute('title')).toContain('中断');
    fireEvent.click(spinnerBtn);
    await act(async () => {
      await interrupt.mock.results[0]?.value;
    });
    expect(interrupt).toHaveBeenCalledWith(workingAgent.id);
    // 反馈轮 17 新语义：group 仍 working → 按钮保持转圈（agent 收尾中），不提前回 ↑
    const stillSpinner = container.querySelector('.chat-send-btn') as HTMLButtonElement;
    expect(stillSpinner.getAttribute('aria-label')).toBe('中断');
    // 列表轮询刷新 group 回 completed → 恢复 ↑ 发送
    rerender(<ChatTab agent={{ ...workingAgent, group: 'completed' }} />);
    const backToSend = container.querySelector('.chat-send-btn') as HTMLButtonElement;
    expect(backToSend.getAttribute('aria-label')).toBe('发送');
    expect(backToSend.textContent).toBe('↑');
  });

  it('keeps the interrupt spinner while agent.group is working/needs_input (反馈轮 17)', () => {
    // 盲区修复：awaitingReply 首条回复即复位、sessionActive 静默 30s 回摆后，
    // 生命周期信号 group 兜底——工作中/待输入全程按钮保持转圈可点中断
    for (const group of ['working', 'needs_input'] as const) {
      const { container, unmount } = setup({ ...agent, id: `g-${group}`, group });
      const btn = container.querySelector('.chat-send-btn') as HTMLButtonElement;
      expect(btn.getAttribute('aria-label')).toBe('中断');
      expect(btn.querySelector('.send-spinner')).not.toBeNull();
      unmount();
    }
  });

  it('queues the message on button click when draft is non-empty while working (反馈轮 28-B)', () => {
    // 工作态按钮两态：非空点击 = 排队发送（与 Enter 等效），不再被中断按钮挡住
    const workingAgent: AgentSummary = { ...agent, group: 'working' };
    const sendMessage = vi.spyOn(api, 'sendMessage').mockReturnValue(new Promise(() => {}));
    const interrupt = vi.spyOn(api, 'interruptAgent').mockResolvedValue();
    const { container, textarea } = setup(workingAgent);
    fireEvent.change(textarea, { target: { value: '排队第三条' } });
    const btn = container.querySelector('.chat-send-btn') as HTMLButtonElement;
    expect(btn.getAttribute('aria-label')).toBe('排队发送');
    expect(btn.getAttribute('title')).toContain('排队');
    expect(btn.textContent).toBe('↑');
    fireEvent.click(btn);
    expect(sendMessage).toHaveBeenCalledWith(workingAgent.id, '排队第三条');
    expect(interrupt).not.toHaveBeenCalled();
  });

  it('shows the send arrow when agent.group is idle (反馈轮 17)', () => {
    const { container } = setup({ ...agent, id: 'g-idle', group: 'completed' });
    const btn = container.querySelector('.chat-send-btn') as HTMLButtonElement;
    expect(btn.getAttribute('aria-label')).toBe('发送');
    expect(btn.textContent).toBe('↑');
  });
});

describe('ChatTab docs state persistence (反馈轮 15 裁决：B10 跨 Tab 保持)', () => {
  const docsAgent: AgentSummary = {
    ...agent,
    id: 'docs-agent',
    name: '文档会话',
    cwd: '/repo/main',
    sessionId: 'docs-agent',
  };
  const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      {children}
    </QueryClientProvider>
  );

  it('keeps tree expansion and preview alive across tab switches', async () => {
    vi.spyOn(api, 'docsList').mockImplementation((path: string) =>
      path === '/repo/main'
        ? Promise.resolve([
            { name: 'docs', isDir: true, kind: 'dir' },
            { name: 'README.md', isDir: false, kind: 'markdown' },
          ])
        : Promise.resolve([{ name: 'deep.txt', isDir: false, kind: 'text' }]),
    );
    vi.spyOn(api, 'docsFile').mockResolvedValue({
      kind: 'markdown',
      name: 'README.md',
      content: '# T',
    });
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);

    const { container } = render(<ChatTab agent={docsAgent} />, { wrapper });

    // 初始 view=chat：docs 区恒挂载但隐藏（不卸载是保持的前提）
    const area = container.querySelector('.chat-docs-area');
    expect(area).not.toBeNull();
    expect(area?.className).toContain('chat-docs-area--hidden');

    // 切文档 → 树加载 → 展开目录（懒加载）+ 点文件预览
    fireEvent.click(screen.getByRole('tab', { name: '文档' }));
    await waitFor(() => expect(container.querySelectorAll('.doc-row').length).toBe(2));
    fireEvent.click(container.querySelectorAll('.doc-row')[0]);
    await waitFor(() => expect(container.textContent).toContain('deep.txt'));
    const readme = [...container.querySelectorAll('.doc-row')].find((r) =>
      r.textContent?.includes('README.md'),
    ) as HTMLButtonElement;
    fireEvent.click(readme);
    await waitFor(() => expect(container.querySelector('.doc-preview')).not.toBeNull());

    // 切聊天 → docs 区隐藏但 DOM 保留：展开集 + 预览文件不丢
    fireEvent.click(screen.getByRole('tab', { name: 'Agent' }));
    const hidden = container.querySelector('.chat-docs-area');
    expect(hidden?.className).toContain('chat-docs-area--hidden');
    expect(container.querySelector('.doc-preview')).not.toBeNull();
    expect(container.textContent).toContain('deep.txt');
    expect(container.querySelector('.doc-preview-head')?.textContent).toContain('README.md');

    // 切回文档 → 状态完整呈现
    fireEvent.click(screen.getByRole('tab', { name: '文档' }));
    expect(container.querySelector('.chat-docs-area--hidden')).toBeNull();
    expect(container.querySelector('.doc-preview')).not.toBeNull();
    expect(container.textContent).toContain('deep.txt');
  });

  it('shows the no-cwd hint only while docs view is active', () => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={agent} />);
    expect(container.querySelector('.chat-docs-area')).toBeNull();
    expect(container.textContent).not.toContain('该会话无关联目录');
    fireEvent.click(screen.getByRole('tab', { name: '文档' }));
    expect(container.textContent).toContain('该会话无关联目录');
  });
});

describe('ChatTab mobile topbar (反馈轮 20：汉堡占位不遮标题)', () => {
  const topbarAgent: AgentSummary = { ...agent, id: 'topbar-agent', name: '顶栏会话' };

  const setup = (): void => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
  };

  it('renders the hamburger as the first topbar child and fires onOpenSidebar', () => {
    setup();
    const onOpenSidebar = vi.fn();
    const { container } = render(<ChatTab agent={topbarAgent} onOpenSidebar={onOpenSidebar} />);
    const topbar = container.querySelector('.chat-area-topbar');
    expect(topbar).not.toBeNull();
    // flex 序首：汉堡独立占位，标题在其右侧（而非 fixed 浮层压字）
    const burger = container.querySelector('.chat-topbar-menu-btn');
    expect(burger).not.toBeNull();
    expect(burger?.parentElement).toBe(topbar);
    expect(topbar?.firstElementChild).toBe(burger);
    fireEvent.click(burger!);
    expect(onOpenSidebar).toHaveBeenCalledTimes(1);
  });

  it('renders no hamburger without onOpenSidebar (desktop layout untouched)', () => {
    setup();
    const { container } = render(<ChatTab agent={topbarAgent} />);
    expect(container.querySelector('.chat-topbar-menu-btn')).toBeNull();
  });

  it('keeps the mobile topbar CSS contract (tripwire)', () => {
    const strip = (css: string): string => css.replace(/\/\*[\s\S]*?\*\//g, '');
    // ChatTab.css：桌面默认隐藏，≤768px 行内占位 + info 补余宽 + 标题 ellipsis
    const chat = strip(readFileSync('src/components/ChatTab.css', 'utf8'));
    expect(chat).toMatch(/\.chat-topbar-menu-btn\s*\{[^}]*display:\s*none/);
    // 首个 ≤768px 查询（汉堡规则所在）到桌面 769px 查询之间即移动端契约区
    const mobile = chat.slice(
      chat.indexOf('@media (max-width: 768px)'),
      chat.indexOf('@media (min-width: 769px)'),
    );
    expect(mobile).toMatch(/\.chat-topbar-menu-btn\s*\{[^}]*display:\s*inline-flex/);
    expect(mobile).toMatch(/\.chat-topbar-menu-btn\s*\{[^}]*min-width:\s*44px/);
    expect(mobile).toMatch(/\.chat-topbar-info\s*\{[^}]*flex:\s*1/);
    expect(mobile).toMatch(/\.chat-topbar-name\s*\{[^}]*text-overflow:\s*ellipsis/);
    // App.css：有会话时隐藏 fixed 浮层 ☰（防双汉堡，且只限 ≤768px）
    const app = strip(readFileSync('src/App.css', 'utf8'));
    expect(app).toMatch(
      /\.layout\.has-session:not\(\.left-open\) \.sidebar-expand\s*\{[^}]*display:\s*none/,
    );
    // App.css 的隐藏规则必须落在 768px 媒体查询内（桌面折叠态浮层行为不变）
    const appMobile = app.slice(app.indexOf('@media (max-width: 768px)'));
    expect(appMobile).toContain('.layout.has-session:not(.left-open) .sidebar-expand');
  });

  it('binds editable-element colors to tokens, dark-proof (tripwire, 反馈轮 27)', () => {
    const strip = (css: string): string => css.replace(/\/\*[\s\S]*?\*\//g, '');
    const app = strip(readFileSync('src/App.css', 'utf8'));
    // 浏览器默认 input/textarea 文字硬编码黑、不随 body 继承，暗色下必须显式走 token：
    // 文字 --c-text（暗色 #f0f0f0）、占位符 --c-faint（暗色 #9a9a9a）、caret 同文字色。
    expect(app).toMatch(/input,\s*\n*select,\s*\n*textarea\s*\{\s*[^}]*color:\s*var\(--c-text\)\s*[^}]*caret-color:\s*var\(--c-text\)/);
    const placeholder = app.match(/input::placeholder,\s*\n*textarea::placeholder\s*\{\s*color:\s*var\(--c-faint\)\s*;\s*\}/);
    expect(placeholder).not.toBeNull();
    // 组件层显式占位符不得用次级 --c-sub 压暗（占位符统一走 --c-faint）
    const chat = strip(readFileSync('src/components/ChatTab.css', 'utf8'));
    expect(chat).toMatch(/\.chat-input\s+textarea::placeholder\s*\{[^}]*color:\s*var\(--c-faint\)/);
    expect(chat).not.toMatch(/::placeholder\s*\{[^}]*color:\s*var\(--c-sub\)/);
  });
});

describe('ChatTab paste/drop image (r71)', () => {
  const silenceApi = (): void => {
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
  };
  const makePng = (): File =>
    new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2, 3])], 'shot.png', {
      type: 'image/png',
    });
  const fireDataEvent = (
    el: Element,
    type: string,
    data: Record<string, unknown>,
  ): void => {
    const evt = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperty(evt, 'clipboardData', { value: data });
    Object.defineProperty(evt, 'dataTransfer', { value: data });
    fireEvent(el, evt);
  };

  const renderInput = (): { container: HTMLElement; textarea: HTMLTextAreaElement; form: HTMLFormElement } => {
    const rr = render(<ChatTab agent={agent} />);
    const textarea = rr.container.querySelector('.chat-input textarea') as HTMLTextAreaElement;
    const form = rr.container.querySelector('form.chat-input') as HTMLFormElement;
    return { container: rr.container, textarea, form };
  };

  it('uploads a pasted image and shows its chip', async () => {
    silenceApi();
    const upload = vi
      .spyOn(api, 'uploadImage')
      .mockResolvedValue({ path: '/tmp/claude-view-uploads/a.png' });
    const { container, textarea } = renderInput();
    fireDataEvent(textarea, 'paste', {
      items: [
        { kind: 'string', type: 'text/plain', getAsFile: () => null },
        { kind: 'file', type: 'image/png', getAsFile: () => makePng() },
      ],
    });
    await waitFor(() => expect(container.querySelector('.image-chip')).not.toBeNull());
    expect(upload).toHaveBeenCalledWith('shot.png', expect.any(String));
    expect(container.querySelector('.image-chips')?.textContent).toContain('[image#1]');
  });

  it('uploads a dropped image file', async () => {
    silenceApi();
    const upload = vi
      .spyOn(api, 'uploadImage')
      .mockResolvedValue({ path: '/tmp/claude-view-uploads/b.png' });
    const { container, form } = renderInput();
    fireDataEvent(form, 'drop', { files: [makePng(), new File(['x'], 'note.txt', { type: 'text/plain' })] });
    await waitFor(() => expect(container.querySelector('.image-chip')).not.toBeNull());
    expect(upload).toHaveBeenCalledTimes(1); // 非图片文件忽略
  });

  it('sends @path references at message tail after the draft text', async () => {
    silenceApi();
    vi.spyOn(api, 'uploadImage').mockResolvedValue({ path: '/tmp/claude-view-uploads/c.png' });
    const sendMessage = vi.spyOn(api, 'sendMessage').mockResolvedValue();
    const { textarea, form } = renderInput();
    fireDataEvent(textarea, 'paste', {
      items: [{ kind: 'file', type: 'image/png', getAsFile: () => makePng() }],
    });
    await waitFor(() => expect(api.uploadImage).toHaveBeenCalled());
    fireEvent.change(textarea, { target: { value: '看这张图' } });
    fireEvent.submit(form);
    await waitFor(() => expect(sendMessage).toHaveBeenCalled());
    // r71 实测格式：正文在前，@绝对路径 引用在尾（CLI 自动 attach 图片）
    expect(sendMessage).toHaveBeenCalledWith(
      agent.id,
      '看这张图\n@/tmp/claude-view-uploads/c.png',
    );
  });

  it('does not intercept plain-text paste', () => {
    silenceApi();
    const upload = vi.spyOn(api, 'uploadImage');
    const { textarea } = renderInput();
    fireDataEvent(textarea, 'paste', {
      items: [{ kind: 'string', type: 'text/plain', getAsFile: () => null }],
    });
    expect(upload).not.toHaveBeenCalled();
  });
});

describe('ChatTab ACP 会话（批2 任务8）', () => {
  const acpAgent: AgentSummary = {
    ...agent,
    driver: 'acp',
    id: 'acp-session-1',
    sessionId: 'acp-session-1',
    kind: 'acp',
    rawState: 'idle',
  };
  const acpMessages: ChatMessage[] = [
    { kind: 'user', rawType: 'acp_user', text: 'hi', toolUseId: null, toolName: null, input: null, result: null, error: null, ts: null },
    { kind: 'assistant', rawType: 'acp_chunk', text: 'he', toolUseId: null, toolName: null, input: null, result: null, error: null, ts: null },
    { kind: 'assistant', rawType: 'acp_chunk', text: 'llo', toolUseId: null, toolName: null, input: null, result: null, error: null, ts: null },
  ];

  const silenceApi = (): void => {
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
  };

  it('历史走 acpMessages、发送走 sendAcpPrompt（claude 端点零调用）', async () => {
    silenceApi();
    useSessionMock.mockImplementation(() => ({
      messages: acpMessages,
      isLoading: false,
      error: null,
      refetch: () => {},
      hasMore: false,
      noMore: false,
      loadingOlder: false,
      loadOlder: async () => false,
    }));
    const sendAcpPrompt = vi.spyOn(api, 'sendAcpPrompt').mockResolvedValue();
    const sendMessage = vi.spyOn(api, 'sendMessage');
    const rr = render(<ChatTab agent={acpAgent} />);
    // useSession 收到 source='acp'
    expect(useSessionMock).toHaveBeenCalledWith(
      acpAgent.id,
      acpAgent.sessionId,
      null,
      'acp',
      null,
      expect.anything(),
    );
    // 合并后的 assistant 气泡渲染 'hello'（Markdown 容器内）
    await waitFor(() => expect(rr.container.textContent).toContain('hello'));
    // 发送
    const textarea = rr.container.querySelector('.chat-input textarea') as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: 'ping' } });
    const form = rr.container.querySelector('form.chat-input') as HTMLFormElement;
    fireEvent.submit(form);
    await waitFor(() => expect(sendAcpPrompt).toHaveBeenCalledWith('acp-session-1', 'ping'));
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it('中断按钮走 cancelAcpSession，claude interrupt 不被调用', async () => {
    silenceApi();
    useSessionMock.mockImplementation(() => ({
      messages: acpMessages,
      isLoading: false,
      error: null,
      refetch: () => {},
      hasMore: false,
      noMore: false,
      loadingOlder: false,
      loadOlder: async () => false,
    }));
    const cancelAcpSession = vi.spyOn(api, 'cancelAcpSession').mockResolvedValue();
    const interruptAgent = vi.spyOn(api, 'interruptAgent');
    // working 态 + 空 draft → 按钮呈中断形态
    const working: AgentSummary = { ...acpAgent, group: 'working', rawState: 'working' };
    const rr = render(<ChatTab agent={working} />);
    await waitFor(() => expect(rr.container.querySelector('[aria-label="中断"]')).not.toBeNull());
    fireEvent.click(rr.container.querySelector('[aria-label="中断"]') as HTMLButtonElement);
    await waitFor(() => expect(cancelAcpSession).toHaveBeenCalledWith('acp-session-1'));
    expect(interruptAgent).not.toHaveBeenCalled();
    // 批3 任务12：中断后状态回闲 —— 列表刷新 group 离开 working → 按钮回 ↑ 发送
    rr.rerender(<ChatTab agent={{ ...working, group: 'other', rawState: 'idle' }} />);
    const sendBtn = rr.container.querySelector('.chat-send-btn') as HTMLButtonElement;
    expect(sendBtn.getAttribute('aria-label')).toBe('发送');
    expect(sendBtn.textContent).toBe('↑');
  });

  it('ACP 跳过 claude 专属轮询（sessionActive/agentTasks/subagents 零调用）', async () => {
    silenceApi();
    const sessionActive = vi.spyOn(api, 'sessionActive');
    const agentTasks = vi.spyOn(api, 'agentTasks');
    const subagents = vi.spyOn(api, 'subagents');
    render(<ChatTab agent={acpAgent} />);
    await new Promise((r) => setTimeout(r, 30));
    expect(sessionActive).not.toHaveBeenCalled();
    expect(agentTasks).not.toHaveBeenCalled();
    expect(subagents).not.toHaveBeenCalled();
  });
});

describe('ChatTab ACP plan 固定栏（批2 任务10）', () => {
  const acpAgent: AgentSummary = {
    ...agent,
    driver: 'acp',
    id: 'acp-session-2',
    sessionId: 'acp-session-2',
    kind: 'acp',
    rawState: 'idle',
  };
  const plan = (entries: { content: string; status: string }[]): ChatMessage => ({
    kind: 'other',
    rawType: 'acp_plan',
    text: null,
    toolUseId: null,
    toolName: null,
    input: null,
    result: { sessionUpdate: 'plan', entries },
    error: null,
    ts: null,
  });

  it('plan update 渲染 TaskListBar（固定栏），第二次更新原地覆盖（只显最新）', () => {
    useSessionMock.mockImplementation(() => ({
      messages: [
        { kind: 'user', rawType: 'acp_user', text: 'go', toolUseId: null, toolName: null, input: null, result: null, error: null, ts: null },
        plan([
          { content: '第一步', status: 'pending' },
          { content: '第二步', status: 'pending' },
        ]),
        plan([
          { content: '第一步', status: 'completed' },
          { content: '第二步', status: 'in_progress' },
        ]),
      ],
      isLoading: false,
      error: null,
      refetch: () => {},
      hasMore: false,
      noMore: false,
      loadingOlder: false,
      loadOlder: async () => false,
    }));
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={acpAgent} />);
    const bar = container.querySelector('.task-bar') as HTMLElement;
    expect(bar).not.toBeNull();
    // 摘要 = 排序后第一项（in_progress 优先）+ 总数 2（非 3：旧 plan 被覆盖）
    expect(container.querySelector('.task-bar-summary-text')?.textContent).toContain('第二步');
    expect(container.querySelector('.task-bar-summary-text')?.textContent).toContain('(2)');
    // plan 消息不出现在消息流（防重复）
    expect(container.querySelector('[role="log"]')?.textContent).not.toContain('第一步');
  });
});

describe('ChatTab ACP 流式文案（批2 任务8 修正）', () => {
  it('working 且无 jsonl 活跃时 ACP 显示「agent 正在输出…」而非 subagent 待续', async () => {
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const acpAgent: AgentSummary = {
      driver: 'acp',
      id: 'acp-s',
      name: 'omp',
      cwd: '/tmp',
      kind: 'acp',
      rawState: 'working',
      group: 'working',
      detail: null,
      tokens: null,
      startedAt: null,
      sessionId: 'acp-s',
    };
    const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        {children}
      </QueryClientProvider>
    );
    const { container } = render(<ChatTab agent={acpAgent} />, { wrapper });
    await waitFor(() =>
      expect(container.querySelector('[role="status"].chat-streaming')?.textContent).toContain(
        'agent 正在输出…',
      ),
    );
    expect(container.querySelector('[role="status"].chat-streaming')?.textContent).not.toContain(
      'subagent',
    );
  });
});

describe('ChatTab ACP 权限弹卡（批3 任务11）', () => {
  const acpAgent: AgentSummary = {
    ...agent,
    driver: 'acp',
    id: 'acp-session-p',
    sessionId: 'acp-session-p',
    kind: 'acp',
    rawState: 'working',
    group: 'working',
  };
  const PERM: ChatMessage = {
    kind: 'other',
    rawType: 'acp_permission',
    text: null,
    toolUseId: 'perm:acp-session-p:9001',
    toolName: 'risky op',
    input: {
      options: [
        { optionId: 'opt-allow', name: 'Allow', kind: 'allow_once' },
        { optionId: 'opt-reject', name: 'Reject', kind: 'reject_once' },
      ],
    },
    result: null,
    error: null,
    ts: null,
  };
  const acpSession = (messages: ChatMessage[]): SessionState => ({
    messages,
    isLoading: false,
    error: null,
    refetch: () => {},
    hasMore: false,
    noMore: false,
    loadingOlder: false,
    loadOlder: async () => false,
  });
  const setup = (messages: ChatMessage[]) => {
    useSessionMock.mockImplementation(() => acpSession(messages));
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    return render(<ChatTab agent={acpAgent} />);
  };

  it('挂起权限请求弹卡：呈现请求标题与全部选项（无默认放行，均需点击）', () => {
    vi.spyOn(api, 'answerAcpPermission').mockResolvedValue();
    const { container } = setup([PERM]);
    const dialog = container.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    expect(dialog?.textContent).toContain('risky op');
    const labels = [...dialog!.querySelectorAll('button')].map((b) => b.textContent);
    expect(labels).toEqual(expect.arrayContaining(['Allow', 'Reject']));
  });

  it('点选项回传 answerAcpPermission（会话id + permId + optionId），弹卡关闭', async () => {
    const answer = vi.spyOn(api, 'answerAcpPermission').mockResolvedValue();
    const { container } = setup([PERM]);
    fireEvent.click(screen.getByRole('button', { name: 'Allow' }));
    await act(async () => {
      await answer.mock.results[0]?.value;
    });
    expect(answer).toHaveBeenCalledWith('acp-session-p', 'perm:acp-session-p:9001', 'opt-allow');
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it('流内已有 resolved 回执 → 不弹卡；权限卡消息不进消息流', () => {
    vi.spyOn(api, 'answerAcpPermission').mockResolvedValue();
    const resolved: ChatMessage = {
      kind: 'other',
      rawType: 'acp_permission_resolved',
      text: '已应答: opt-allow',
      toolUseId: 'perm:acp-session-p:9001',
      toolName: null,
      input: null,
      result: null,
      error: null,
      ts: null,
    };
    const { container } = setup([PERM, resolved]);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    // 权限消息由弹卡承载，不出现在消息流（防 other 兜底渲染成用户气泡）
    expect(container.querySelector('[role="log"]')?.textContent).not.toContain('risky op');
  });

  it('关掉的旧请求在新请求收场后重新弹卡（permDismissed 随 pending 变化重置，ocr-review 高）', async () => {
    // 场景：A 关闭（记 dismissed）→ B 到达弹出 → B 收场 → detect 回落 A——
    // dismissed 不重置则 A 永久压卡，agent 侧工具调用阻塞死锁且无恢复入口
    const answer = vi.spyOn(api, 'answerAcpPermission').mockResolvedValue();
    const PERM_B: ChatMessage = {
      ...PERM,
      toolUseId: 'perm:acp-session-p:9002',
      toolName: 'second op',
    };
    const RECEIPT_B: ChatMessage = {
      kind: 'other',
      rawType: 'acp_permission_resolved',
      text: '已应答: opt-allow',
      toolUseId: 'perm:acp-session-p:9002',
      toolName: null,
      input: null,
      result: null,
      error: null,
      ts: null,
    };
    useSessionMock.mockImplementation(() => acpSession([PERM]));
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const view = render(<ChatTab agent={acpAgent} />);
    expect(view.container.querySelector('[role="dialog"]')).not.toBeNull();
    // 本地关闭 A（取消 = 记 dismissed）
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    await act(async () => {
      await answer.mock.results[0]?.value;
    });
    expect(view.container.querySelector('[role="dialog"]')).toBeNull();
    // B 到达（A 仍 pending）→ 弹 B
    useSessionMock.mockImplementation(() => acpSession([PERM, PERM_B]));
    view.rerender(<ChatTab agent={acpAgent} />);
    const dialog = view.container.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    expect(dialog?.textContent).toContain('second op');
    // B 收场 → pending 回落 A → 卡必须重现为 A（修复前 dismissed=A 永久压卡）
    useSessionMock.mockImplementation(() => acpSession([PERM, PERM_B, RECEIPT_B]));
    view.rerender(<ChatTab agent={acpAgent} />);
    const dialog2 = view.container.querySelector('[role="dialog"]');
    expect(dialog2).not.toBeNull();
    expect(dialog2?.textContent).toContain('risky op');
  });
});

describe('ChatTab 跨实例会话操作（multi-instance 任务7）', () => {
  const remoteAgent: AgentSummary = { ...agent, id: 'agent-x', instanceId: 'instA' };

  const setupWithApi = (stub: Partial<typeof api>): { container: HTMLElement; textarea: HTMLTextAreaElement; form: HTMLFormElement } => {
    // 先 spy 本机 api（防旁路 fetch），再复制出 per-instance 桩并覆盖被测方法
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const instApi = { ...api, ...stub };
    const rr = render(<ChatTab agent={remoteAgent} api={instApi as typeof api} />);
    const container = rr.container;
    return {
      container,
      textarea: container.querySelector('.chat-input textarea') as HTMLTextAreaElement,
      form: container.querySelector('form.chat-input') as HTMLFormElement,
    };
  };

  it('发消息路由到 per-instance api（本机 api 不被调用）', () => {
    const sendMessage = vi.fn(() => Promise.resolve());
    const { textarea, form } = setupWithApi({ sendMessage });
    fireEvent.change(textarea, { target: { value: '跨实例消息' } });
    fireEvent.submit(form);
    expect(sendMessage).toHaveBeenCalledWith('agent-x', '跨实例消息');
  });

  it('中断路由到 per-instance api（working 态按钮=中断）', async () => {
    const interruptAgent = vi.fn(() => Promise.resolve());
    // working 态 + 空输入 → 按钮位显示中断（r17 语义）
    const workingAgent: AgentSummary = { ...remoteAgent, group: 'working' };
    vi.spyOn(api, 'subagents').mockResolvedValue([]);
    vi.spyOn(api, 'agentTasks').mockResolvedValue([]);
    vi.spyOn(api, 'sessionActive').mockResolvedValue(false);
    vi.spyOn(api, 'listCommands').mockResolvedValue([]);
    const { container } = render(<ChatTab agent={workingAgent} api={{ ...api, interruptAgent } as typeof api} />);
    fireEvent.click(container.querySelector('.chat-send-btn') as HTMLButtonElement);
    await waitFor(() => expect(interruptAgent).toHaveBeenCalledWith('agent-x'));
  });
});
