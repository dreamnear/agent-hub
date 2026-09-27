import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactElement } from 'react';
import { api as localApi, type Api } from '../api';
import { useSession } from '../hooks/useSession';
import { detectPendingAsk } from '../hooks/usePendingAsk';
import { detectPendingPermission } from '../hooks/usePendingPermission';
import { combineToolCalls, extractAcpPlan, extractTaskList, type TaskItem } from '../hooks/combineToolCalls';
import { compressToJpeg, fileToBase64 } from '../lib/image';
import DocViewer from './DocViewer';
import ChatMessageView from './ChatMessageView';
import TaskListBar from './TaskListBar';
import SubagentBar from './SubagentBar';
import ConfirmDialog, { type ConfirmRequest } from './ConfirmDialog';
import AcpPermissionDialog from './AcpPermissionDialog';
import type { AgentSummary, DocEntry, SubagentEntry } from '../types';
import './ChatTab.css';

interface Props {
  agent: AgentSummary;
  onClose?: () => void;
  /** 移动端顶栏汉堡：打开左侧会话抽屉（反馈轮 20——fixed 浮层 ☰ 会遮标题，改顶栏内占位） */
  onOpenSidebar?: () => void;
  /** 工程便签悬浮卡（agent-hub-notes 增强）：顶栏切换钮（替代原 ⌘K 搜索钮位；⌘K 快捷键保留，入口在侧栏搜索钮） */
  notesOpen?: boolean;
  onToggleNotes?: () => void;
  /** 多实例（批1 任务4）：该 agent 所属实例的 per-instance api；缺省=本机 api（现状零破坏） */
  api?: Api;
}

/// 对话主区域（P5 A1）：选中 agent 即对话；顶栏含状态与操作（logs/stop/respawn/rm）；
/// 支持 C1 流入指示 / C2 中断 / C3 图片 / C4 斜杠补全 / C5 ANSI 颜色 / C6 工具收敛。
/// 界面按 Penpot 规格换皮（ui-spec-penpot.md）；危险操作统一走 ConfirmDialog。
export default function ChatTab({
  agent,
  onClose,
  onOpenSidebar,
  notesOpen,
  onToggleNotes,
  api = localApi,
}: Props): ReactElement | null {
  const sessionId = agent.sessionId;
  // ACP 会话（acp-omp 批2 任务8）：历史/发送/中断走 ACP 端点，活跃度轮询与
  // subagent/任务聚合（claude jsonl 专属）跳过
  const isAcp = agent.driver === 'acp';
  // subagent 聚焦（P5+）：非空时消息/WS 切到该 subagent，会话只读；置 null 返回主会话
  const [focusSubagent, setFocusSubagent] = useState<string | null>(null);
  // 历史 GET 用 agent.id（分页，P6 B12/B13）；WS 订阅用 sessionId；增量合流 query cache；
  // isLoading/error/refetch 驱动消息区加载/失败态（反馈轮 11）；hasMore/loadOlder 驱动向上翻页
  // 多实例（批1 任务4）：useSession 按 agent.instanceId 路由 per-instance api + queryKey 隔离
  const { messages, isLoading, error, refetch, hasMore, noMore, loadingOlder, loadOlder } =
    useSession(agent.id, sessionId, focusSubagent, isAcp ? 'acp' : 'claude', agent.instanceId ?? null, api);
  const pendingAsk = detectPendingAsk(messages);
  const [draft, setDraft] = useState('');
  const [err, setErr] = useState('');
  const [sending, setSending] = useState(false);
  // 发送按钮 spinner：POST 2xx 起转，agent 离开 working/needs_input（回复完成）后恢复 ↑
  const [awaitingReply, setAwaitingReply] = useState(false);
  const sentRef = useRef<number | null>(null);
  // 乐观气泡（反馈 7-1）：发送即显，真实行到达销账
  const [optimisticText, setOptimisticText] = useState<string | null>(null);
  // 排队气泡（反馈轮 24-C）：working 期发送成功即显，消息落盘或 agent 离开 working 销账
  const [queuedHint, setQueuedHint] = useState(false);
  const [view, setView] = useState<'chat' | 'docs'>('chat');
  // C3：待发送图片 chips（[image#N] → 真实路径，发送时映射为引导语）
  const [pendingImages, setPendingImages] = useState<{ tag: string; path: string }[]>([]);
  const [uploading, setUploading] = useState(false);
  // 统一确认弹窗（替换原生 confirm）
  const [confirmReq, setConfirmReq] = useState<ConfirmRequest | null>(null);
  // C1：输出中指示——双信号合成（反馈 7-2）：消息块脉冲（即时亮）+ jsonl mtime
  // 活跃度轮询（稳定续期，块间静默不抖动，30s 阈值内=有输出）。
  const [streamingPulse, setStreamingPulse] = useState(false);
  const [remoteActive, setRemoteActive] = useState(false);
  const streaming = streamingPulse || remoteActive;
  // 工作态（反馈 7-3 / 17）：发送已受理、输出活跃或 agent 生命周期为工作中 → 按钮切中断。
  // agent.group 是 server 侧 CLI 状态映射（working/running/active/blocked），覆盖
  // awaitingReply 首条回复即复位、remoteActive jsonl 静默 30s 回摆两个盲区——
  // 长工具/长思考期间按钮持续转圈（宁可误报不可漏报：误报后果仅是可点中断）。
  const working =
    !sending &&
    (awaitingReply || remoteActive || agent.group === 'working' || agent.group === 'needs_input');
  // busy 变量已随反馈轮 24-C 移除：sending 单独挡 POST 在途，awaitingReply 不再挡发送
  // （agent 处理中再发 = 入队）
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // ACP 权限弹卡（批3 任务11）：挂起请求 → 弹卡；权限卡/回执消息由弹卡承载，
  // 不进消息流（other 兜底会渲染成用户气泡）。permDismissed 兜回执事件未到时的本地关闭；
  // ocr-review 高：dismissed 随挂起请求/会话切换重置——否则旧 dismissed 永久压卡
  // （A 关掉后 B 收场回落 A，卡不再弹出，agent 工具链阻塞死锁）或跨会话误屏蔽
  const pendingPerm = isAcp ? detectPendingPermission(messages) : null;
  const [permDismissed, setPermDismissed] = useState<string | null>(null);
  useEffect(() => {
    setPermDismissed(null);
  }, [agent.id, pendingPerm?.toolUseId]);
  const activePerm =
    pendingPerm != null && pendingPerm.toolUseId !== permDismissed ? pendingPerm : null;
  const chatMessages = useMemo(
    () =>
      messages.filter(
        (m) => m.rawType !== 'acp_permission' && m.rawType !== 'acp_permission_resolved',
      ),
    [messages],
  );

  const items = combineToolCalls(chatMessages);
  // 任务清单固定栏（preview 反馈：不随消息流滚动，固定于会话窗口底部）。
  // 数据源：server 跨会话聚合 API 优先（全局清单口径）；失败/缺失回退单会话 extractTaskList。
  const fallbackTasks = extractTaskList(messages);
  const [remoteTasks, setRemoteTasks] = useState<TaskItem[] | null>(null);
  useEffect(() => {
    if (isAcp) return; // ACP：任务清单走 plan（批2 任务10），不查 claude 聚合端点
    let alive = true;
    api
      .agentTasks(agent.id)
      .then((t) => {
        if (alive) setRemoteTasks(t);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [agent.id, messages.length, isAcp]);
  // r27 语义反转：null = 服务端无权威视角，回退单会话口径兜底；
  // [] = 权威空快照（harness 无任务），不回退防旧任务复活。
  // ACP（批2 任务10）：任务清单 = 最后一条 plan update（无 plan → null → 空栏不渲染）
  const taskList = isAcp ? extractAcpPlan(messages) : (remoteTasks ?? fallbackTasks);

  // subagent 清单（P5+）：目录缺失/空 → []，固定栏不渲染；切 agent 重置聚焦
  const [subagents, setSubagents] = useState<SubagentEntry[]>([]);
  useEffect(() => {
    if (isAcp) return; // ACP 无 subagent 概念
    let alive = true;
    setFocusSubagent(null);
    api
      .subagents(agent.id)
      .then((list) => {
        if (alive) setSubagents(list);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [agent.id, isAcp]);

  // POST 2xx 起由 awaitingReply 驱动 spinner；复位条件：
  // 有新消息到达且 messages 长度超过发送时的快照（且为 assistant 新块），才复位——否则历史 lastKind 立即命中
  const lastKind = messages[messages.length - 1]?.kind;
  useEffect(() => {
    if (!awaitingReply) return;
    const base = sentRef.current;
    if (base == null) return;
    if (messages.length > base && lastKind === 'assistant') {
      sentRef.current = null;
      setAwaitingReply(false);
    } else if (agent.group !== 'working' && agent.group !== 'needs_input') {
      // 非增量流的完成态（列表轮询回到 idle/completed）同样复位
      sentRef.current = null;
      setAwaitingReply(false);
    }
  }, [agent.group, awaitingReply, lastKind, messages.length]);

  // C1：新块（末条消息变化 = 尾部追加）到达 → 心跳 + 自动滚动；5s 窗口内续期——
  // 流式期间持续可见，块间隔超窗口（工具执行/思考中）由 mtime 轮询信号接管，不再闪烁。
  // 依赖末条引用而非 length：向上翻页 prepend（P6 B13）不改变末条，不触发滚底
  const lastMsg = messages[messages.length - 1];
  useEffect(() => {
    if (!lastMsg) return;
    setStreamingPulse(true);
    const t = setTimeout(() => setStreamingPulse(false), 5000);
    bottomRef.current?.scrollIntoView({ block: 'end' });
    return () => clearTimeout(t);
  }, [lastMsg]);

  // 向上翻页锚点保持（P6 B13）：补页前记 scrollHeight，DOM 增高后按高度差补偿 scrollTop，
  // 视口停在原有消息上不跳；补偿走 useLayoutEffect（提交前完成，无闪烁帧）。
  // 记录与置位拆开（r1 M-1）：prepend 真正落地才挂补偿——翻页在途的 WS 追加先落地时
  // ref 尚为 null 不误消耗，真 prepend 到达时补偿仍有效
  const anchorHeightRef = useRef<number | null>(null);
  const loadOlderWithAnchor = async (): Promise<boolean> => {
    const prev = listRef.current?.scrollHeight ?? null;
    const ok = await loadOlder();
    if (ok && prev != null) anchorHeightRef.current = prev;
    return ok;
  };
  useLayoutEffect(() => {
    const prev = anchorHeightRef.current;
    if (prev == null) return;
    anchorHeightRef.current = null;
    const el = listRef.current;
    if (el) el.scrollTop += el.scrollHeight - prev;
  }, [messages.length]);

  // mtime 活跃度轮询（反馈 7-2）：5s 间隔，仅主会话视角（ACP 无 jsonl，
  // 工作态由 Tick 刷新的 agent.group + WS chunk 脉冲驱动）
  useEffect(() => {
    if (focusSubagent || isAcp) return;
    let alive = true;
    const tick = (): void => {
      api
        .sessionActive(agent.id)
        .then((a) => {
          if (alive) setRemoteActive(a);
        })
        .catch(() => {});
    };
    tick();
    const iv = setInterval(tick, 5000);
    return () => {
      alive = false;
      clearInterval(iv);
    };
  }, [agent.id, focusSubagent, isAcp]);

  const send = async (): Promise<void> => {
    const t = draft.trim();
    // r71 实测（tmux 靶子）：@绝对路径 文本注入后 CLI 自动 attach 图片（多模态直读），
    // 比纯文本路径提示（依赖模型自发 Read 工具）可靠——引用置于消息尾，正文保持用户原文
    const imageRefs = pendingImages.map((p) => `@${p.path}`).join(' ');
    const finalText = imageRefs ? (t ? `${t}\n${imageRefs}` : imageRefs) : t;
    // 反馈轮 24-C：sending（POST 在途）仍挡防双击；awaitingReply 不再挡——
    // agent 处理中再发消息 = 进入 CLI 队列（working 期排队实测支持）
    if (!finalText || sending) return;
    // 受理即显排队气泡（乐观语义）：以 agent 组判断（不看 sending 瞬时值）
    const busyFamily = agent.group === 'working' || agent.group === 'needs_input';
    setSending(true);
    setErr('');
    // 乐观 UI（反馈 7-1）：立即清空输入框 + 本地气泡，不等 PTY 回流；
    // 真实用户行落盘后由下方 effect 对账销账
    setDraft('');
    setPendingImages([]);
    setOptimisticText(finalText);
    if (busyFamily) setQueuedHint(true);
    try {
      if (isAcp) {
        // ACP（批2 任务8）：prompt POST 在途 = 整轮处理；chunk 由 WS 房间流式到达
        await api.sendAcpPrompt(agent.id, finalText);
      } else {
        await api.sendMessage(agent.id, finalText);
      }
      sentRef.current = messages.length;
      setAwaitingReply(true);
    } catch (e) {
      // 失败回滚：还原文本、撤气泡
      setOptimisticText(null);
      setQueuedHint(false);
      setDraft(t);
      setErr(String(e));
    } finally {
      setSending(false);
    }
  };

  // 乐观气泡对账：真实用户行（同文本）经 tail/refetch 进入消息流即销账，防重复气泡
  useEffect(() => {
    if (optimisticText == null) return;
    if (messages.some((m) => m.kind === 'user' && m.text === optimisticText)) {
      setOptimisticText(null);
      setQueuedHint(false); // 排队消息已被 CLI 处理落盘（反馈轮 24-C）
    }
  }, [messages, optimisticText]);

  // 排队气泡清理：agent 生命周期组离开工作态（回复完成/中断/退出）时撤销。
  // 不能用 working 瞬时值判（sending 在途时 working 恒 false，会误清刚受理的气泡）
  useEffect(() => {
    if (agent.group !== 'working' && agent.group !== 'needs_input') setQueuedHint(false);
  }, [agent.group]);

  const interruptNow = async (): Promise<void> => {
    try {
      // ACP（批2 任务8）：cancel 端点（批3 任务12 完善按钮语义）；claude 走原链路
      if (isAcp) await api.cancelAcpSession(agent.id);
      else await api.interruptAgent(agent.id);
    } catch (e) {
      setErr(String(e));
    }
    sentRef.current = null;
    setAwaitingReply(false);
    setOptimisticText(null);
  };

  const sendWithCheck = (): void => {
    if (sending) return; // 反馈轮 24-C：仅挡 POST 在途；agent 忙时发送 = 入队
    if (pendingAsk) {
      // pendingAsk 确认改走 ConfirmDialog（提醒型）
      setConfirmReq({
        title: '仍要发送？',
        message: 'agent 正在等待选项回答（TUI 模态），普通文本可能被丢弃。',
        banner: '建议选择选项后按 Enter 提交',
        variant: 'warn',
        confirmLabel: '仍要发送',
        action: () => send(),
      });
      return;
    }
    void send();
  };

  const pickImage = async (file: File): Promise<void> => {
    setErr('');
    setUploading(true);
    try {
      // 移动端排查（P5 preview）：>2MB 或 HEIC 先压图（canvas → jpeg），解码失败给清晰错误
      let { base64, name } = await (async (): Promise<{ base64: string; name: string }> => {
        // 压图触发口径计 base64 ×1.37 膨胀（tester-r6）：>5MB 二进制即压
        if (file.size > 5 * 1024 * 1024 || file.type === 'image/heic') {
          const compressed = await compressToJpeg(file);
          if (compressed == null) {
            throw new Error('图片格式无法解码（iPhone HEIC 请先转 JPG）');
          }
          return compressed;
        }
        return { base64: await fileToBase64(file), name: file.name };
      })();
      if (base64.length > 7 * 1024 * 1024) {
        throw new Error('压缩后图片仍过大，请手动缩小后重试');
      }
      const { path } = await api.uploadImage(name, base64);
      // [image#N] 标签显示（不显真实长路径），发送时前端映射替换为引导语（P5 preview 反馈）
      setPendingImages((prev) => [...prev, { tag: `[image#${prev.length + 1}]`, path }]);
    } catch (e) {
      setErr(String(e));
    } finally {
      setUploading(false);
    }
  };

  // C4：`/` 开头时弹斜杠补全（命令清单按当前实例路由，任务7）
  const slashQuery = draft.startsWith('/') ? draft.slice(1).toLowerCase() : null;
  const slashMatches = useSlashMatches(slashQuery, api);

  // 批5 MINOR-3：@ 文件引用补全（placeholder「@ 引用」的落地实现）——
  // 输入 @word 时列 cwd 相对路径候选（docsList 白名单接口），选中插入 @path
  // 交 CLI 解析引用。@ 后含 / 时按目录段渐进下钻。
  const atExec = /(?:^|\s)@([^\s@]*)$/.exec(draft);
  const atToken = atExec ? atExec[1] : null;
  const atSlash = atToken != null ? atToken.lastIndexOf('/') : -1;
  const atDir = atSlash >= 0 ? atToken!.slice(0, atSlash + 1) : '';
  const atFilter = atSlash >= 0 ? atToken!.slice(atSlash + 1).toLowerCase() : (atToken ?? '').toLowerCase();
  const [atEntries, setAtEntries] = useState<DocEntry[]>([]);
  const [atIdx, setAtIdx] = useState(0);
  const [atDismissed, setAtDismissed] = useState(false);
  useEffect(() => {
    setAtIdx(0);
    setAtDismissed(false);
  }, [atDir]);
  useEffect(() => {
    if (atToken == null || !agent.cwd) return;
    let alive = true;
    api
      .docsList(`${agent.cwd}/${atDir}`)
      .then((entries) => {
        if (alive) setAtEntries(entries);
      })
      .catch(() => {
        if (alive) setAtEntries([]);
      });
    return () => {
      alive = false;
    };
  }, [atToken == null, atDir, agent.cwd]);
  const atCandidates = atToken == null ? [] : atEntries.filter((e) => e.name.toLowerCase().includes(atFilter)).slice(0, 8);
  // atIdx 钳制：entries 异步刷新（目录切换 fetch 间隙）时旧 atIdx 可能越界——
  // r66 实测 Enter 触发 pickAt(undefined) 抛 TypeError 且消息静默不发
  const atSafeIdx = Math.min(atIdx, Math.max(0, atCandidates.length - 1));
  const atOpen = atToken != null && !atDismissed && atCandidates.length > 0;
  const pickAt = (entry: DocEntry): void => {
    if (!atExec || !entry) return;
    const path = `${atDir}${entry.name}${entry.isDir ? '/' : ''}`;
    const lead = atExec[0].startsWith(' ') ? ' ' : '';
    setDraft(draft.slice(0, atExec.index) + lead + `@${path} `);
    inputRef.current?.focus();
  };

  // 输入框自适应增高（preview 反馈五：初始一行紧凑，多行随内容撑高，封顶后滚动）
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  return (
    <div className="chat-tab">
      <div className="chat-area-topbar">
        {/* 移动端顶栏行首汉堡（反馈轮 20）：独立占位，替代 fixed 浮层 ☰（桌面 display:none） */}
        {onOpenSidebar ? (
          <button
            type="button"
            className="chat-topbar-menu-btn"
            aria-label="展开侧栏"
            onClick={onOpenSidebar}
          >
            ☰
          </button>
        ) : null}
        <div className="chat-topbar-info">
          <span className="chat-topbar-name">{agent.name ?? agent.id.slice(0, 8)}</span>
          <span className={`chat-state-badge chat-state-badge--${agent.group}`}>
            {agent.rawState ?? '—'}
          </span>
          <span className="chat-topbar-meta">{agent.cwd ?? ''}</span>
          <span className="chat-topbar-meta">tokens: {agent.tokens ?? '—'}</span>
        </div>
        <div className="chat-topbar-actions">
          {/* 会话级操作全部移入侧栏 ⋯ 菜单（2026-09-22 用户定案）；顶栏右侧 =
              Agent/文档视图切换 + 工程便签切换（原 ⌘K 搜索钮位，⌘K 快捷键保留侧栏搜索）+ 关闭 */}
          <div className="chat-view-toggle" role="tablist" aria-label="view">
            <button type="button" role="tab" aria-selected={view === 'chat'} onClick={() => setView('chat')}>
              Agent
            </button>
            {/* 终端功能彻底移除（2026-09-22 用户终审定案）：Tab 只留 Agent/文档，
                网页端不再提供任何终端入口 */}
            <button
              type="button"
              role="tab"
              aria-selected={view === 'docs'}
              onClick={() => setView('docs')}
            >
              文档
            </button>
          </div>
          {onToggleNotes ? (
            <button
              type="button"
              className={`btn-topbar-notes ${notesOpen ? 'btn-topbar-notes--open' : ''}`}
              aria-label="工程便签"
              aria-pressed={notesOpen}
              title="工程便签（服务端口、测试账密等备忘，同一工程所有会话共享）"
              onClick={onToggleNotes}
            >
              📋
            </button>
          ) : null}
          {onClose ? (
            <button type="button" onClick={onClose} aria-label="Close">
              ×
            </button>
          ) : null}
        </div>
      </div>

      {/* 反馈轮 15 裁决修复（B10 语义）：文档区恒渲染、display 切换——切 Tab 不卸载，
          树展开集/懒加载缓存/当前预览文件跨 Tab 保持 */}
      {agent.cwd ? (
        <div className={view === 'docs' ? 'chat-docs-area' : 'chat-docs-area chat-docs-area--hidden'}>
          <DocViewer root={agent.cwd} onClose={() => setView('chat')} />
        </div>
      ) : view === 'docs' ? (
        <p className="doc-hint">该会话无关联目录，无法浏览文档</p>
      ) : null}

      {view === 'chat' ? (
        <>
          {focusSubagent ? (
            <div className="chat-readonly-hint" role="status">
              <span>
                🔎 正在查看 subagent「
                {subagents.find((s) => s.agentId === focusSubagent)?.name ?? focusSubagent}
                」会话（只读）
              </span>
              <button type="button" onClick={() => setFocusSubagent(null)}>
                ← 返回主会话
              </button>
            </div>
          ) : null}
          {!focusSubagent && pendingAsk ? (
            <div className="chat-pending-ask" role="status">
              ⏸ agent 正在等待选项回答——TUI 模态中，普通文本会被丢弃；请选择选项后按 Enter 提交
            </div>
          ) : null}
          <div className="chat-list" ref={listRef} role="log" aria-label="conversation">
            {hasMore ? (
              <button
                type="button"
                className="chat-load-older"
                onClick={() => void loadOlderWithAnchor()}
                disabled={loadingOlder}
              >
                {loadingOlder ? '加载中…' : '加载更早消息'}
              </button>
            ) : null}
            {noMore ? <p className="chat-no-more">无更多消息</p> : null}
            {isLoading && items.length === 0 ? (
              <div className="chat-session-loading" role="status">
                <span className="session-spinner" aria-hidden /> 正在加载会话…
              </div>
            ) : error && items.length === 0 ? (
              <div className="chat-session-error" role="alert">
                会话加载失败
                <button type="button" onClick={refetch}>
                  重试
                </button>
              </div>
            ) : items.length === 0 ? (
              <p className="chat-empty">
                {focusSubagent ? '该 subagent 会话暂无消息' : '暂无消息，输入开始对话'}
              </p>
            ) : (
              items.map((item) => (
                <ChatMessageView key={item.key} item={item} />
              ))
            )}
            {streaming && !focusSubagent ? (
              <div className="chat-streaming flex items-center gap-1.5" role="status" data-bui>
                {/* Beautiful UI 批2 光标形态（官网 streaming-text）：细竖条替代 ▍ */}
                <span
                  aria-hidden
                  className="inline-block h-3 w-0.5 translate-y-0.5 rounded-full bg-ink"
                  style={{ animation: 'fade-in 150ms ease-out both' }}
                />
                {/* 反馈轮 28-C 裁决：group=working 且主 jsonl 静默 = 主 LLM 空转等
                    subagent，文案区分「subagent 执行中」与真实输出，判定链不动。
                    ACP 无 jsonl 静默语义（批2 任务8），恒显输出中文案 */}
                {agent.group === 'working' && !remoteActive && !isAcp
                  ? 'subagent 执行中 · 主会话待续'
                  : 'agent 正在输出…'}
              </div>
            ) : null}
            {queuedHint && !focusSubagent ? (
              <div className="chat-queued-bubble" role="status">
                ⏳ 已加入队列 · Agent 完成当前任务后处理
              </div>
            ) : null}
            {optimisticText ? (
              <div className="chat-optimistic-bubble">
                <ChatMessageView
                  item={combineToolCalls([
                    {
                      kind: 'user',
                      rawType: null,
                      text: optimisticText,
                      toolUseId: null,
                      toolName: null,
                      input: null,
                      result: null,
                      error: null,
                      ts: null,
                    },
                  ])[0]}
                />
              </div>
            ) : null}
            <div ref={bottomRef} />
          </div>
          {/* subagent 折叠条 + 任务清单：底部并列同构（反馈轮 6 形态调整） */}
          <SubagentBar
            subagents={subagents}
            activeId={focusSubagent}
            onSelect={(id) => setFocusSubagent((prev) => (prev === id ? null : id))}
          />
          {!focusSubagent && (taskList?.length ?? 0) > 0 ? <TaskListBar tasks={taskList ?? []} /> : null}
          {focusSubagent == null ? (
            <>
              {err ? <p className="dialog-error chat-error">{err}</p> : null}
              {uploading ? (
                <div className="chat-uploading" role="status">
                  上传中…
                </div>
              ) : null}
              {pendingImages.length > 0 ? (
                <div className="image-chips">
                  {pendingImages.map((p, i) => (
                    <span key={p.path} className="image-chip">
                      [image#{i + 1}]
                      <button
                        type="button"
                        aria-label={`移除 image#${i + 1}`}
                        onClick={() => setPendingImages((prev) => prev.filter((_, j: number) => j !== i))}
                      >
                        ×
                      </button>
                    </span>
                  ))}
                </div>
              ) : null}
              {atOpen ? (
                <ul className="slash-menu" role="listbox" aria-label="file mentions">
                  {atCandidates.map((entry, i) => (
                    <li key={`${entry.name}:${i}`}>
                      <button
                        type="button"
                        aria-selected={i === atSafeIdx}
                        onClick={() => pickAt(entry)}
                        onMouseEnter={() => setAtIdx(i)}
                      >
                        <span className="slash-name">
                          {entry.isDir ? '▸ ' : ''}
                          {entry.name}
                          {entry.isDir ? '/' : ''}
                        </span>
                        <span className="slash-src">@</span>
                        <span className="slash-desc">{atDir || '工作目录'}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              ) : null}
              {slashMatches.length > 0 ? (
                <ul className="slash-menu" role="listbox" aria-label="slash commands">
                  {slashMatches.map((c) => (
                    <li key={`${c.source}:${c.name}`}>
                      <button
                        type="button"
                        onClick={() => {
                          setDraft(`/${c.name} `);
                        }}
                      >
                        <span className="slash-name">/{c.name}</span>
                        <span className="slash-src">{c.source}</span>
                        <span className="slash-desc">{c.description ?? ''}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              ) : null}
          </> ) : null}
          {focusSubagent == null ? (
          <form
            className="chat-input"
            onSubmit={(e) => {
              e.preventDefault();
              sendWithCheck();
            }}
            onDragOver={(e) => e.preventDefault()}
            onDrop={(e) => {
              // r71 拖拽图片：与粘贴同走上传链路；非图片文件忽略
              e.preventDefault();
              const imgs = Array.from(e.dataTransfer.files).filter((f) =>
                f.type.startsWith('image/'),
              );
              imgs.forEach((f) => void pickImage(f));
            }}
          >
            <textarea
              ref={inputRef}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onPaste={(e) => {
                // r71 粘贴图片：clipboardData 里的 image blob 走既有上传链路；纯文本粘贴不拦截
                const file = Array.from(e.clipboardData.items).find(
                  (it) => it.kind === 'file' && it.type.startsWith('image/'),
                )?.getAsFile();
                if (file) {
                  e.preventDefault();
                  void pickImage(file);
                }
              }}
              placeholder={
                pendingAsk ? 'TUI 模态中：选择选项后按 Enter 提交，普通文本将被丢弃' : '输入消息…（Enter 发送，/ 命令，@ 引用）'
              }
              rows={1}
              onKeyDown={(e) => {
                // @ 引用菜单键位优先（批5 MINOR-3）：↓↑ 选择、Enter 插入、Esc 关闭
                if (atOpen && atCandidates.length > 0) {
                  if (e.key === 'ArrowDown') {
                    e.preventDefault();
                    setAtIdx((i) => Math.min(i + 1, atCandidates.length - 1));
                    return;
                  }
                  if (e.key === 'ArrowUp') {
                    e.preventDefault();
                    setAtIdx((i) => Math.max(i - 1, 0));
                    return;
                  }
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    pickAt(atCandidates[atSafeIdx]);
                    return;
                  }
                  if (e.key === 'Escape') {
                    e.preventDefault();
                    setAtDismissed(true);
                    return;
                  }
                }
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  sendWithCheck();
                }
              }}
            />
            <div className="chat-input-actions">
              <input
                ref={fileRef}
                type="file"
                accept="image/png,image/jpeg,image/gif,image/webp"
                style={{ display: 'none' }}
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void pickImage(f);
                  e.target.value = '';
                }}
              />
              <button
                type="button"
                className="chat-image-btn"
                onClick={() => fileRef.current?.click()}
                title="附图片（支持粘贴 / 拖拽）"
              >
                🖼
              </button>
              {working || sending ? (
                draft.trim() ? (
                  /* 反馈轮 28-B：工作态按钮两态——非空=排队发送（与 Enter 等效，
                     r24-C 队列），空=中断（r17 语义）；同一按钮不再打架 */
                  <button
                    type="button"
                    className="chat-send-btn"
                    title="排队发送（Agent 完成当前任务后处理）"
                    aria-label="排队发送"
                    onClick={sendWithCheck}
                  >
                    ↑
                  </button>
                ) : (
                  <button
                    type="button"
                    className="chat-send-btn"
                    title="点击中断 agent 当前处理"
                    aria-label="中断"
                    onClick={() => void interruptNow()}
                  >
                    <span className="send-spinner" aria-hidden />
                  </button>
                )
              ) : (
                <button
                  type="submit"
                  className="chat-send-btn"
                  disabled={!draft.trim()}
                  title="发送"
                  aria-label="发送"
                >
                  ↑
                </button>
              )}
            </div>
          </form>
          ) : null}
        </>
      ) : null}
      {confirmReq ? <ConfirmDialog request={confirmReq} onClose={() => setConfirmReq(null)} /> : null}
      {activePerm ? (
        <AcpPermissionDialog
          agentId={agent.id}
          request={activePerm}
          api={api}
          onClose={() => setPermDismissed(activePerm.toolUseId)}
        />
      ) : null}
    </div>
  );
}

/// C4：/ 补全候选（内置 + 自定义命令，前缀过滤）。命令清单按当前实例 api 拉取（任务7）。
function useSlashMatches(
  query: string | null,
  api: Api,
): { name: string; source: string; description: string | null }[] {
  const [all, setAll] = useState<{ name: string; source: string; description: string | null }[]>([]);
  useEffect(() => {
    api
      .listCommands()
      .then(setAll)
      .catch(() => setAll([]));
  }, [api]);
  if (query == null) return [];
  return all.filter((c) => c.name.toLowerCase().startsWith(query)).slice(0, 8);
}
