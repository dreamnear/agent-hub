import { Fragment, memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api as localApi, type Api, type ModelSettings } from '../api';
import { useSession } from '../hooks/useSession';
import { detectPendingAsk } from '../hooks/usePendingAsk';
import { detectPendingPermission } from '../hooks/usePendingPermission';
import { combineToolCalls, extractAcpPlan, extractTaskList, type RenderItem, type TaskItem } from '../hooks/combineToolCalls';
import { compressToJpeg, fileToBase64 } from '../lib/image';
import { fuzzyFilter } from '../lib/fuzzy';
import { useI18n } from '../i18n';
import ChatMessageView from './ChatMessageView';
import TaskListBar from './TaskListBar';
import SubagentBar from './SubagentBar';
import ConfirmDialog, { type ConfirmRequest } from './ConfirmDialog';
import AcpPermissionDialog from './AcpPermissionDialog';
import DshQuestionCard from './DshQuestionCard';
import AskUserQuestionCard from './AskUserQuestionCard';
import ModelPanel from './ModelPanel';
import DriverBadge from './DriverBadge';
import type { AgentSummary, DocEntry, SubagentEntry } from '../types';
import { plugins, type PluginCommandItem } from '../plugins';
import CapabilityFace from './CapabilityFace';
import './ChatTab.css';

interface Props {
  agent: AgentSummary;
  onClose?: () => void;
  /** 移动端顶栏汉堡：打开左侧会话抽屉（反馈轮 20——fixed 浮层 ☰ 会遮标题，改顶栏内占位） */
  onOpenSidebar?: () => void;
  /** 工程便签悬浮卡（agent-hub-notes 增强）：顶栏切换钮（替代原 ⌘K 搜索钮位；⌘K 快捷键保留，入口在侧栏搜索钮） */
  notesOpen?: boolean;
  onToggleNotes?: () => void;
  /** 打开终端页签（agent-hub-terminal T1）：仅选中会话有 cwd 时传入 */
  onOpenTerminal?: () => void;
  /** 多实例（批1 任务4）：该 agent 所属实例的 per-instance api；缺省=本机 api（现状零破坏） */
  api?: Api;
  /** 草稿按会话保留（agent-hub-pick-switch #18 交互模型 4）：挂载时的初值。
   * 本组件随会话键重挂载，草稿保存在 App 的 per-key 表里，切换会话再切回不丢 */
  initialDraft?: string;
  /** 草稿上行（每次变更回报 App，由其按会话键存表）；不传则纯本组件内态 */
  onDraftChange?: (text: string) => void;
  /** hub-plugin view 型 command：打开插件视图（B2 批4，App 持 pluginView 态） */
  onOpenPluginView?: (pluginId: string, viewId: string) => void;
}

/// 对话主区域（P5 A1）：选中 agent 即对话；顶栏含状态与操作（logs/stop/respawn/rm）；
/// 支持 C1 流入指示 / C2 中断 / C3 图片 / C4 斜杠补全 / C5 ANSI 颜色 / C6 工具收敛。
/// 界面按 Penpot 规格换皮（ui-spec-penpot.md）；危险操作统一走 ConfirmDialog。

/// 文本归一化（pi-busy-send S4）：trim + 折叠连续空白——乐观气泡与落盘真实行的
/// 匹配键（PTY/CLI 回写行可能差行尾符/连续空白，精确相等会失手致双渲染）。
const normWs = (s: string): string => s.replace(/\s+/g, ' ').trim();

/// 消息列表区域（2026-10-02 输入卡顿根治）：React.memo 隔离——draft 每击键
/// 重渲染 ChatTab 壳层，但本区域 props 引用/值稳定时不参与协调，消息列表
///（可能数百条含工具卡/diff/thinking）不再全量重渲染。
/// ponytail ceiling：不做虚拟化——消息 >1k 条再考虑。
const ChatMessageRegion = memo(function ChatMessageRegion({
  items,
  streaming,
  focusSubagent,
  agentWorking,
  remoteActive,
  isAcp,
  queuedHint,
  optimisticItem,
  optimisticDismissed,
  sendErrorItem,
  subagentWorkingLabel,
  outputtingLabel,
  queuedLabel,
  agentId,
  driver,
  pendingAskToolId,
}: {
  items: RenderItem[];
  streaming: boolean;
  focusSubagent: string | null;
  agentWorking: boolean;
  remoteActive: boolean;
  isAcp: boolean;
  /** pi-busy-send S4：落盘行已含同文（归一化）——乐观气泡渲染期守卫 */
  optimisticDismissed: boolean;
  queuedHint: boolean;
  optimisticItem: RenderItem | null;
  sendErrorItem: RenderItem | null;
  subagentWorkingLabel: string;
  outputtingLabel: string;
  queuedLabel: string;
  agentId: string;
  driver: string;
  pendingAskToolId: string | null;
}): ReactElement {
  return (
    <>
      {items.map((item) =>
        item.kind === 'message' &&
        item.msg.rawType === 'dsh_question' &&
        !focusSubagent ? (
          <DshQuestionCard
            key={item.key}
            item={item.msg}
            agentId={agentId}
            driver={driver}
          />
        ) : item.kind === 'tool_call' &&
          item.toolName === 'AskUserQuestion' &&
          !focusSubagent ? (
          <AskUserQuestionCard
            key={item.key}
            item={{
              kind: 'tool_use',
              rawType: 'tool_use',
              text: null,
              toolUseId: item.toolUseId,
              toolName: item.toolName,
              input: item.input,
              result: item.result,
              error: item.isError,
              ts: null,
            }}
            pending={item.result == null && pendingAskToolId === item.toolUseId}
          />
        ) : (
          <ChatMessageView key={item.key} item={item} />
        ),
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
          {agentWorking && !remoteActive && !isAcp ? subagentWorkingLabel : outputtingLabel}
        </div>
      ) : null}
      {queuedHint && !focusSubagent ? (
        <div className="chat-queued-bubble" role="status">{queuedLabel}</div>
      ) : null}
      {optimisticItem && !optimisticDismissed ? (
        <div className="chat-optimistic-bubble"><ChatMessageView item={optimisticItem} /></div>
      ) : null}
      {sendErrorItem ? (
        <div className="chat-send-error-card" role="alert">
          <ChatMessageView item={sendErrorItem} />
        </div>
      ) : null}
    </>
  );
});

/// context 占用缩写（CTX 徽标）：87k / 460k / 1.2M；千位以下原样。
/// 百分比不做——各模型上下限不同，不硬编码。
export function fmtCtx(v: number): string {
  if (v < 1000) return String(v);
  if (v < 1_000_000) return `${Math.round(v / 1000)}k`;
  return `${(v / 1_000_000).toFixed(1)}M`;
}

export default function ChatTab({
  agent,
  onClose,
  onOpenSidebar,
  notesOpen,
  onToggleNotes,
  onOpenTerminal,
  api = localApi,
  initialDraft = '',
  onDraftChange,
  onOpenPluginView,
}: Props): ReactElement | null {
  const t = useI18n();
  // 会话视图（Agent/文档）：本地状态即可——App 以 key={`${instanceId}:${id}`} 挂载，
  // 换会话即重挂载，「换会话回 Agent 视图」语义由重挂载天然保证（2026-09-30 撤回 App 提升）
  const sessionId = agent.sessionId;
  // ACP 会话（acp-omp 批2 任务8）：历史/发送/中断走 ACP 端点，活跃度轮询与
  // subagent/任务聚合（claude jsonl 专属）跳过
  const isAcp = agent.driver === 'acp';
  const driver = agent.driver;
  // R7 会话粘连：interactive 前台会话（用户终端手跑，不经 hub daemon 管理）——
  // 只读展示照常，输入通道禁用（发送必然失败的路径不暴露给用户）
  const isInteractive = agent.kind === 'interactive';
  // subagent 聚焦（P5+）：非空时消息/WS 切到该 subagent，会话只读；置 null 返回主会话
  const [focusSubagent, setFocusSubagent] = useState<string | null>(null);
  // 历史 GET 用 agent.id（分页，P6 B12/B13）；WS 订阅用 sessionId；增量合流 query cache；
  // isLoading/error/refetch 驱动消息区加载/失败态（反馈轮 11）；hasMore/loadOlder 驱动向上翻页
  // 多实例（批1 任务4）：useSession 按 agent.instanceId 路由 per-instance api + queryKey 隔离
  const { messages, isLoading, error, refetch, hasMore, noMore, loadingOlder, loadOlder } =
    useSession(agent.id, sessionId, focusSubagent, isAcp ? 'acp' : 'claude', agent.instanceId ?? null, api, driver);
  const pendingAsk = detectPendingAsk(messages);
  const [draft, setDraft] = useState(initialDraft);
  // 草稿上行（#18 交互模型 4）：变更回报 App 按会话键存表；发送清空/图片清空
  // 同样走此路径（草稿状态只有这一处真相）。首挂载也回报一次（值多与原值相同，
  // App 侧同值不写，无重渲环）
  useEffect(() => {
    onDraftChange?.(draft);
  }, [draft, onDraftChange]);
  const [err, setErr] = useState('');
  const [sending, setSending] = useState(false);
  // 发送按钮 spinner：POST 2xx 起转，agent 离开 working/needs_input（回复完成）后恢复 ↑
  const [awaitingReply, setAwaitingReply] = useState(false);
  const sentRef = useRef<number | null>(null);
  // 乐观气泡（反馈 7-1）：发送即显，真实行到达销账
  const [optimisticText, setOptimisticText] = useState<string | null>(null);
  // 发送失败错误卡（chat 流内可见，2026-10-03 dsh 历史发送静默吞错修复）
  const [sendErrorText, setSendErrorText] = useState<string | null>(null);
  // 排队气泡（反馈轮 24-C）：working 期发送成功即显，消息落盘或 agent 离开 working 销账
  const [queuedHint, setQueuedHint] = useState(false);
  // C3：待发送图片 chips（[image#N] → 真实路径，发送时映射为引导语）
  const [pendingImages, setPendingImages] = useState<{ tag: string; path: string }[]>([]);
  const [uploading, setUploading] = useState(false);
  // 统一确认弹窗（替换原生 confirm）
  const [confirmReq, setConfirmReq] = useState<ConfirmRequest | null>(null);
  // C1：输出中指示——双信号合成（反馈 7-2）：消息块脉冲（即时亮）+ jsonl mtime
  // 活跃度轮询（稳定续期，块间静默不抖动，30s 阈值内=有输出）。
  const [streamingPulse, setStreamingPulse] = useState(false);
  const [remoteActive, setRemoteActive] = useState(false);
  // agent-hub-assistant-idle-stuck 根因修复：pi 的输出指示锚定后端状态机
  // （dispatch 泵 agent_start/agent_end 全权翻转，group 是唯一真源）。原 claude
  // 消息块脉冲对 pi 有两处错位：恢复场景初次历史加载同样换 lastMsg 引用 → 每次
  // 进入误亮；熄灭仅靠 5s timer，cache 抖动（lastMsg 短暂为空）时 timer 被清理
  // 且 effect 早退不复位 → 可卡死常驻。claude 链路（jsonl mtime + 块脉冲）原语义
  // 不动（回归范围红线）。
  const streaming =
    driver === 'pi' ? agent.group === 'working' : streamingPulse || remoteActive;
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
  // 权限弹卡线形跨驱动复用（acp + dsh 单一 Dialog；dsh 审批亦映射为 acp_permission）
  const pendingPerm = detectPendingPermission(messages);
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

  // 会话外观（agent-hub-model-thinking）：模型/思考芯片读回自驱动（hub 不存状态）。
  // acp 等不支持驱动 caps 全 false → 不渲染芯片（不虚构）。
  const [modelPanelOpen, setModelPanelOpen] = useState(false);
  const modelQc = useQueryClient();
  const modelSettings = useQuery<ModelSettings>({
    queryKey: ['model-settings', agent.id, agent.instanceId ?? null],
    queryFn: () => api.getModelSettings(agent.id, driver),
    enabled: !isAcp,
    staleTime: 30_000,
  });
  const ms = modelSettings.data;
  const modelChipLabel = ms?.caps.canRead
    ? ms.current != null
      ? `${ms.current.id}${ms.current.variant != null ? `·${ms.current.variant}` : ''}`
      : null
    : null;
  // 引用稳定化：items 数组仅消息变化时重建——ChatMessageRegion memo 生效前提
  //（每击键重算会让 memo 永远失效，实测 150 次消息组件渲染 → 0）
  const items = useMemo(() => combineToolCalls(chatMessages), [chatMessages]);
  // 乐观气泡 item 引用稳定化（仅 optimisticText 变化时重建）——ChatMessageRegion
  // memo 生效前提：props 每击键保持稳定
  const optimisticItem = useMemo<RenderItem | null>(
    () =>
      optimisticText == null
        ? null
        : combineToolCalls([
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
          ])[0],
    [optimisticText],
  );
  // 发送失败错误卡 item（系统卡形态，role=assistant + error 标记供样式区分）
  const sendErrorItem = useMemo<RenderItem | null>(
    () =>
      sendErrorText == null
        ? null
        : combineToolCalls([
            {
              kind: 'assistant',
              rawType: 'send_error',
              text: sendErrorText,
              toolUseId: null,
              toolName: null,
              input: null,
              result: null,
              error: true,
              ts: null,
            },
          ])[0],
    [sendErrorText],
  );
  // dsh 0.2.0 迁移器拒绝旧格式日志的专属友好文案（识别特征：dsh 侧迁移报错）
  const friendlySendError = (driverName: string, raw: string): string => {
    if (
      driverName === 'dsh' &&
      (/refuses this format/i.test(raw) ||
        /resume failed/i.test(raw) ||
        /tool-call-delta/i.test(raw))
    ) {
      return t('chat.dshLegacyFormat');
    }
    return raw;
  };
  // 任务清单固定栏（preview 反馈：不随消息流滚动，固定于会话窗口底部）。
  // 数据源：server 跨会话聚合 API 优先（全局清单口径）；失败/缺失回退单会话 extractTaskList。
  const fallbackTasks = extractTaskList(messages);
  const [remoteTasks, setRemoteTasks] = useState<TaskItem[] | null>(null);
  useEffect(() => {
    if (isAcp) return; // ACP：任务清单走 plan（批2 任务10），不查 claude 聚合端点
    let alive = true;
    api
      .agentTasks(agent.id, driver)
      .then((t) => {
        if (alive) setRemoteTasks(t);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [agent.id, messages.length, isAcp, driver]);
  // r27 语义反转：null = 服务端无权威视角，回退单会话口径兜底；
  // [] = 权威空快照（harness 无任务），不回退防旧任务复活。
  // ACP（批2 任务10）：任务清单 = 最后一条 plan update（无 plan → null → 空栏不渲染）
  const taskList = isAcp ? extractAcpPlan(messages) : (remoteTasks ?? fallbackTasks);

  // subagent 清单（P5+）：目录缺失/空 → []，固定栏不渲染；切 agent 重置聚焦。
  // 2026-10-01 定案：面板只列活跃；已结束进历史入口（倒序，最近活跃在前）。
  // 一次 opt-in 全量拉取（include_finished）后前端按 status 分流，数据不删。
  const [subagents, setSubagents] = useState<SubagentEntry[]>([]);
  const [finishedSubagents, setFinishedSubagents] = useState<SubagentEntry[]>([]);
  useEffect(() => {
    if (isAcp) return; // ACP 无 subagent 概念
    let alive = true;
    setFocusSubagent(null);
    api
      .subagents(agent.id, true, driver)
      .then((list) => {
        if (!alive) return;
        setSubagents(list.filter((s) => s.status === 'active'));
        setFinishedSubagents(
          list
            .filter((s) => s.status !== 'active')
            .sort((a, b) => (b.lastActiveAt ?? '').localeCompare(a.lastActiveAt ?? '')),
        );
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [agent.id, isAcp, driver]);

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
        .sessionActive(agent.id, driver)
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
    // R7：interactive 前台会话不经 hub 管理，无发送通道（UI 已禁用，此处兜底）
    if (isInteractive) return;
    const text = draft.trim();
    // r71 实测（tmux 靶子）：@绝对路径 文本注入后 CLI 自动 attach 图片（多模态直读），
    // 比纯文本路径提示（依赖模型自发 Read 工具）可靠——引用置于消息尾，正文保持用户原文
    const imageRefs = pendingImages.map((p) => `@${p.path}`).join(' ');
    const finalText = imageRefs ? (text ? `${text}\n${imageRefs}` : imageRefs) : text;
    // 反馈轮 24-C：sending（POST 在途）仍挡防双击；awaitingReply 不再挡——
    // agent 处理中再发消息 = 进入 CLI 队列（working 期排队实测支持）
    if (!finalText || sending) return;
    // 空 id 守卫（2026-10-03 两连 ①）：新建会话未进清单时 selected 快照可能为
    // 空——禁止发出空 sessionId 请求（dsh 网关对空串报 resume failed）
    if (!agent.id.trim()) {
      // pi-busy-send S3：发送错误单处呈现（流内错误卡），不再双写底部 err 行
      setSendErrorText(`${t('chat.sendFailed')}${t('chat.emptySessionId')}`);
      return;
    }
    // 受理即显排队气泡（乐观语义）：以 agent 组判断（不看 sending 瞬时值）
    const busyFamily = agent.group === 'working' || agent.group === 'needs_input';
    setSending(true);
    setErr('');
    // 乐观 UI（反馈 7-1）：立即清空输入框 + 本地气泡，不等 PTY 回流；
    // 真实用户行落盘后由下方 effect 对账销账
    setDraft('');
    setPendingImages([]);
    setOptimisticText(finalText);
    setSendErrorText(null);
    if (busyFamily) setQueuedHint(true);
    try {
      if (isAcp) {
        // ACP（批2 任务8）：prompt POST 在途 = 整轮处理；chunk 由 WS 房间流式到达
        await api.sendAcpPrompt(agent.id, finalText);
      } else {
        await api.sendMessage(agent.id, finalText, driver);
      }
      sentRef.current = messages.length;
      setAwaitingReply(true);
    } catch (e) {
      // 失败回滚：还原文本、撤气泡；错误单处呈现（pi-busy-send S3）——流内
      // 错误卡（sendErrorText）为唯一通道，不再双写底部 err 行（双渲染根因）
      const raw = String(e);
      setOptimisticText(null);
      setQueuedHint(false);
      setDraft(text);
      const friendly = friendlySendError(driver, raw);
      setSendErrorText(`${t('chat.sendFailed')}${friendly}`);
    } finally {
      setSending(false);
    }
  };

  // 乐观气泡对账：真实用户行（同文本）经 tail/refetch 进入消息流即销账，防重复气泡。
  // pi-busy-send S4 加固：① 文本归一化匹配（trim+折叠空白——PTY/CLI 回写行与
  // 发送文本可能差行尾/连续空白，精确匹配失手即「真实行 + 尾部乐观气泡」双渲染）；
  // ② 渲染期守卫（下方 optimisticItem 条件同用此判定）——对账 effect 落在渲染后，
  // 单靠销账有一帧共 render 窗口。
  const optimisticDismissed =
    optimisticText != null &&
    messages.some(
      (m) => m.kind === 'user' && m.text != null && normWs(m.text) === normWs(optimisticText),
    );
  useEffect(() => {
    if (optimisticText == null) return;
    if (optimisticDismissed) {
      setOptimisticText(null);
      setQueuedHint(false); // 排队消息已被 CLI 处理落盘（反馈轮 24-C）
    }
  }, [messages, optimisticText, optimisticDismissed]);

  // 排队气泡清理：agent 生命周期组离开工作态（回复完成/中断/退出）时撤销。
  // 不能用 working 瞬时值判（sending 在途时 working 恒 false，会误清刚受理的气泡）
  useEffect(() => {
    if (agent.group !== 'working' && agent.group !== 'needs_input') setQueuedHint(false);
  }, [agent.group]);

  const interruptNow = async (): Promise<void> => {
    try {
      // ACP（批2 任务8）：cancel 端点（批3 任务12 完善按钮语义）；claude 走原链路
      if (isAcp) await api.cancelAcpSession(agent.id);
      else await api.interruptAgent(agent.id, driver);
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
        title: t('chat.sendAnywayTitle'),
        message: t('chat.sendAnywayMessage'),
        banner: t('chat.sendAnywayBanner'),
        variant: 'warn',
        confirmLabel: t('chat.sendAnyway'),
        action: () => send(),
      });
      return;
    }
    void send();
  };

  // assistant-resume A5：Hub 助手手动新建会话（pi RPC new_session 切内存态；
  // 发首条消息后即成为下次打开的默认会话，零消息重启回退上一会话——实测边界）。
  // 成功后重拉历史（空态）+ 清乐观残留；旧对话文件保留
  // 在 pi session-dir（不删），仅视图切走。
  const startNewSession = (): void => {
    setConfirmReq({
      title: t('chat.newSessionTitle'),
      message: t('chat.newSessionMessage'),
      banner: t('chat.newSessionBanner'),
      variant: 'warn',
      confirmLabel: t('chat.newSessionConfirm'),
      action: () => {
        api
          .newPiSession()
          .then(() => {
            setOptimisticText(null);
            setSendErrorText(null);
            setQueuedHint(false);
            setErr('');
            refetch();
          })
          .catch((e: unknown) => setErr(String(e)));
      },
    });
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
            throw new Error(t('chat.imageUndecodable'));
          }
          return compressed;
        }
        return { base64: await fileToBase64(file), name: file.name };
      })();
      if (base64.length > 7 * 1024 * 1024) {
        throw new Error(t('chat.imageTooLarge'));
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

  // C4：`/` 开头时弹斜杠补全（清单按当前实例路由）。agent-hub-slash-skills：
  // 按 agent cwd 请求（S2，切不同 cwd 的 agent 清单随之变化），来源扩
  // builtin/custom/skill/plugin。
  const slashQuery = draft.startsWith('/') ? draft.slice(1).toLowerCase() : null;
  const pluginCmds = usePluginCommands();
  const slashMatches = useSlashMatches(slashQuery, api, agent.cwd ?? null, pluginCmds);
  // S5 可用性裁量：分组展示 + 总候选上限 50——前缀过滤先行（输入越窄候选越少），
  // 空查询百条级清单按来源序截前 50 条（组头不计入），菜单沿用既有 max-height
  // 滚动；不做键盘导航（现状本无，保持改动面），长尾靠前缀过滤触达。
  const slashGroups = useMemo(() => {
    const labels: Record<string, string> = {
      builtin: t('chat.slashGroupBuiltin'),
      custom: t('chat.slashGroupCustom'),
      skill: t('chat.slashGroupSkill'),
      plugin: t('chat.slashGroupPlugin'),
      'hub-plugin': t('chat.slashGroupHubPlugin'),
    };
    const groups: { source: string; label: string; items: typeof slashMatches }[] = [];
    let count = 0;
    for (const source of ['builtin', 'custom', 'skill', 'plugin', 'hub-plugin']) {
      const items = slashMatches.filter((c) => c.source === source);
      if (items.length === 0) continue;
      const take = items.slice(0, Math.max(0, SLASH_GROUP_CAP - count));
      if (take.length === 0) break;
      count += take.length;
      groups.push({ source, label: labels[source], items: take });
    }
    return groups;
  }, [slashMatches, t]);
  // round 3 键控（r3 退格幽灵队列根因修复）：菜单打开态 Enter 此前直接落发送——
  // 用户按 TUI 习惯以 Enter「选中候选」，实际把未完成的中间态 draft（如已想退格
  // 修正的 `/claude-d/xxx`）提交入队，再修正重发即两条。现对齐 @ 菜单语义：
  // Enter=选中高亮候选（仅替换 draft，不发送；尾空格使菜单自然关闭），↓↑ 移动
  // 高亮，Esc 关闭菜单回落直接发送；菜单关闭态 Enter 行为不变。
  const slashRows = useMemo(() => {
    let acc = 0;
    const groups = slashGroups.map((g) => {
      const start = acc;
      acc += g.items.length;
      return { ...g, start };
    });
    return { groups, total: acc };
  }, [slashGroups]);
  const slashFlat = useMemo(() => slashRows.groups.flatMap((g) => g.items), [slashRows]);
  const [slashIdx, setSlashIdx] = useState(0);
  const [slashDismissed, setSlashDismissed] = useState(false);
  useEffect(() => {
    setSlashIdx(0);
    setSlashDismissed(false);
  }, [slashQuery]);
  const slashOpen = slashQuery != null && !slashDismissed && slashFlat.length > 0;
  // 插件工具执行回执（B2 批4 tool 型 command）：输入区上方一行短通知，4s 自清
  const [pluginNotice, setPluginNotice] = useState<string | null>(null);
  useEffect(() => {
    if (pluginNotice == null) return;
    const timer = setTimeout(() => setPluginNotice(null), 4_000);
    return () => clearTimeout(timer);
  }, [pluginNotice]);
  const pickSlash = (name: string): void => {
    // hub-plugin command：hub 侧语义（打开视图 / 触发只读工具），不插文本给 CLI
    const pc = pluginCmds.find((c) => c.name === name);
    if (pc != null && pc.targetView != null) {
      onOpenPluginView?.(pc.pluginId, pc.targetView);
      setDraft('');
      return;
    }
    if (pc != null && pc.targetTool != null) {
      plugins
        .invokeTool(pc.pluginId, pc.targetTool, pc.targetArgs ?? {})
        .then((res) => {
          setPluginNotice(
            res.ok
              ? t('chat.pluginToolDone')
              : `${t('chat.pluginToolFail')}: ${res.message ?? ''}`,
          );
        })
        .catch((e: unknown) => {
          setPluginNotice(
            `${t('chat.pluginToolFail')}: ${e instanceof Error ? e.message : String(e)}`,
          );
        });
      setDraft('');
      return;
    }
    setDraft(`/${name} `);
    inputRef.current?.focus();
  };

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
            aria-label={t('shell.expandSidebar')}
            onClick={onOpenSidebar}
          >
            ☰
          </button>
        ) : null}
        <div className="chat-topbar-info">
          <span className="chat-topbar-name">
            <DriverBadge driver={agent.driver} />
            {agent.name ?? agent.id.slice(0, 8)}
          </span>
          <span className="chat-topbar-meta">{agent.cwd ?? ''}</span>
          {agent.context_tokens != null ? (
            <span
              className="chat-topbar-meta chat-ctx-badge"
              title={t('chat.ctxTitle')}
            >
              CTX {fmtCtx(agent.context_tokens)}
            </span>
          ) : (
            <span className="chat-topbar-meta">tokens: {agent.tokens ?? '—'}</span>
          )}
          {modelChipLabel != null ? (
            <button
              type="button"
              className="chat-chip chat-chip--model"
              title={t('model.title')}
              onClick={() => setModelPanelOpen(true)}
            >
              ⚡ {modelChipLabel}
            </button>
          ) : null}
        </div>
        <div className="chat-topbar-actions">
          {/* 会话级操作全部移入侧栏 ⋯ 菜单（2026-09-22 用户定案）；顶栏右侧 =
              视图切换单键（Agent⇄文档）+ 工程便签切换（原 ⌘K 搜索钮位，⌘K 快捷键保留侧栏搜索）+ 关闭。
              2026-09-30 用户定案：单 SVG 图标按钮、状态徽章删除、撤回抽屉版 */}
          {/* assistant-resume A5：助手新建会话入口（仅 pi 主会话视图；working 中
              禁用——pi streaming 中切会话语义未定义，不让用户踩） */}
          {driver === 'pi' && focusSubagent == null ? (
            <button
              type="button"
              className="btn-topbar-newsession"
              aria-label={t('chat.newSessionTitle')}
              title={t('chat.newSessionTitle')}
              disabled={working}
              onClick={startNewSession}
            >
              ✚
            </button>
          ) : null}
          {onOpenTerminal ? (
            <button
              type="button"
              className="btn-topbar-terminal"
              aria-label={t('chat.openTerminal')}
              title={t('chat.openTerminal')}
              onClick={onOpenTerminal}
            >
              &gt;_
            </button>
          ) : null}
          {onToggleNotes ? (
            <button
              type="button"
              className={`btn-topbar-notes ${notesOpen ? 'btn-topbar-notes--open' : ''}`}
              aria-label={t('chat.notes')}
              aria-pressed={notesOpen}
              title={t('chat.notesTitle')}
              onClick={onToggleNotes}
            >
              📋
            </button>
          ) : null}
          {onClose ? (
            <button
              type="button"
              onClick={onClose}
              aria-label={t('chat.close')}
              title={t('chat.close')}
            >
              ×
            </button>
          ) : null}
        </div>
      </div>

      {/* agent-hub-files-panel：文档 Tab 退役——文件树进右面板（FilesTree）、
          内容进主区文件页签（FileTabPanel），渲染工厂全量复用能力不回退 */}
      {<>
          {focusSubagent ? (
            <div className="chat-readonly-hint" role="status">
              <span>
                {t('chat.viewingSubagent', {
                  name:
                    subagents.find((s) => s.agentId === focusSubagent)?.name ??
                    finishedSubagents.find((s) => s.agentId === focusSubagent)?.name ??
                    focusSubagent,
                })}
              </span>
              <button type="button" onClick={() => setFocusSubagent(null)}>
                {t('chat.backToMain')}
              </button>
            </div>
          ) : null}
          {!focusSubagent && pendingAsk ? (
            <div className="chat-pending-ask" role="status">
              {t('chat.pendingAskBanner')}
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
                {loadingOlder ? t('chat.loading') : t('chat.loadOlder')}
              </button>
            ) : null}
            {noMore ? <p className="chat-no-more">{t('chat.noMore')}</p> : null}
            {isLoading && items.length === 0 ? (
              <div className="chat-session-loading" role="status">
                <span className="session-spinner" aria-hidden /> {t('chat.sessionLoading')}
              </div>
            ) : error && items.length === 0 ? (
              <div className="chat-session-error" role="alert">
                {t('chat.sessionFailed')}
                <button type="button" onClick={refetch}>
                  {t('chat.retry')}
                </button>
              </div>
            ) : items.length === 0 ? (
              <p className="chat-empty">
                {focusSubagent ? t('chat.emptySubagent') : t('chat.empty')}
              </p>
            ) : null}
            {/* 会话侧能力可发现面（B2 批5）：折叠条，零插件不渲染（零回归） */}
            {focusSubagent == null ? <CapabilityFace /> : null}
            <ChatMessageRegion
              items={items}
              streaming={streaming}
              focusSubagent={focusSubagent}
              // idle-stuck：agentWorking 是 claude 的「jsonl 静默 = 主 LLM 等 subagent」
              // 文案分流信号——pi 无此语义（working 即真实输出/工具执行），恒 false
              agentWorking={agent.group === 'working' && driver !== 'pi'}
              remoteActive={remoteActive}
              isAcp={isAcp}
              queuedHint={queuedHint}
              optimisticItem={optimisticItem}
              optimisticDismissed={optimisticDismissed}
          sendErrorItem={sendErrorItem}
              subagentWorkingLabel={t('chat.subagentWorking')}
              outputtingLabel={t('chat.outputting')}
              queuedLabel={t('chat.queued')}
              agentId={agent.id}
              driver={driver}
              pendingAskToolId={pendingAsk?.toolUseId ?? null}
            />
            <div ref={bottomRef} />
          </div>
          {/* subagent 折叠条 + 任务清单：底部并列同构（反馈轮 6 形态调整）；
              两者相邻时并排一行省高度（预览反馈），窄容器回落纵向 */}
          <div className="chat-bars-row">
            <SubagentBar
              subagents={subagents}
              finished={finishedSubagents}
              activeId={focusSubagent}
              onSelect={(id) => setFocusSubagent((prev) => (prev === id ? null : id))}
            />
            {!focusSubagent && (taskList?.length ?? 0) > 0 ? (
              <TaskListBar tasks={taskList ?? []} />
            ) : null}
          </div>
          {focusSubagent == null ? (
            <>
              {err ? <p className="dialog-error chat-error">{err}</p> : null}
              {uploading ? (
                <div className="chat-uploading" role="status">
                  {t('chat.uploading')}
                </div>
              ) : null}
              {pendingImages.length > 0 ? (
                <div className="image-chips">
                  {pendingImages.map((p, i) => (
                    <span key={p.path} className="image-chip">
                      [image#{i + 1}]
                      <button
                        type="button"
                        aria-label={t('chat.removeImage', { n: i + 1 })}
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
                        <span className="slash-desc">{atDir || t('chat.workingDir')}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              ) : null}
              {slashOpen ? (
                <ul className="slash-menu" role="listbox" aria-label="slash commands">
                  {slashRows.groups.map((g) => (
                    <Fragment key={g.source}>
                      <li className="slash-group" role="presentation">
                        {g.label}
                      </li>
                      {g.items.map((c, i) => {
                        const flatIdx = g.start + i;
                        const active = flatIdx === slashIdx;
                        return (
                          <li key={`${c.source}:${c.name}`}>
                            <button
                              type="button"
                              aria-selected={active}
                              className={active ? 'slash-active' : undefined}
                              onMouseEnter={() => setSlashIdx(flatIdx)}
                              onClick={() => {
                                pickSlash(c.name);
                              }}
                            >
                              <span className="slash-name">/{c.name}</span>
                              <span className="slash-src">{g.label}</span>
                              <span className="slash-desc">{c.description ?? ''}</span>
                            </button>
                          </li>
                        );
                      })}
                    </Fragment>
                  ))}
                </ul>
              ) : null}
          </> ) : null}
          {pluginNotice != null ? (
            <div className="chat-plugin-notice" role="status">{pluginNotice}</div>
          ) : null}
          {focusSubagent == null && driver === 'omp' ? (
            <p className="omp-takeover-hint">{t('chat.ompTakeoverHint')}</p>
          ) : null}
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
            {/* 参考图（Gemini composer）定式：左 +（附件）· 中文本 · 右发送 */}
            <button
              type="button"
              className="chat-image-btn"
              onClick={() => fileRef.current?.click()}
              title={t('chat.attachImage')}
            >
              🖼
            </button>
            <textarea
              ref={inputRef}
              value={draft}
              disabled={isInteractive}
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
                isInteractive
                  ? t('chat.interactiveReadonly')
                  : pendingAsk
                    ? t('chat.inputPendingAsk')
                    : t('chat.inputPlaceholder')
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
                // slash 菜单键位（round 3）：菜单打开时 Enter=选中候选（仅替换
                // draft 不发送——尾空格使菜单自然关闭，二次 Enter 才发送），↓↑
                // 移动高亮，Esc 关闭菜单（回落直接发送原文）；关闭态 Enter 不受影响
                if (slashOpen) {
                  if (e.key === 'ArrowDown') {
                    e.preventDefault();
                    setSlashIdx((i) => Math.min(i + 1, slashFlat.length - 1));
                    return;
                  }
                  if (e.key === 'ArrowUp') {
                    e.preventDefault();
                    setSlashIdx((i) => Math.max(i - 1, 0));
                    return;
                  }
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    const pick = slashFlat[slashIdx];
                    if (pick) pickSlash(pick.name);
                    return;
                  }
                  if (e.key === 'Escape') {
                    e.preventDefault();
                    setSlashDismissed(true);
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
              {isInteractive ? (
                /* R7：interactive 前台会话无中断/发送通道——占位提示代替按钮 */
                <span className="chat-interactive-badge" title={t('chat.interactiveReadonly')}>
                  {t('chat.interactiveBadge')}
                </span>
              ) : working || sending ? (
                draft.trim() ? (
                  /* 反馈轮 28-B：工作态按钮两态——非空=排队发送（与 Enter 等效，
                     r24-C 队列），空=中断（r17 语义）；同一按钮不再打架 */
                  <button
                    type="button"
                    className="chat-send-btn"
                    title={t('chat.queueSendTitle')}
                    aria-label={t('chat.queueSend')}
                    onClick={sendWithCheck}
                  >
                    ↑
                  </button>
                ) : (
                  <button
                    type="button"
                    className="chat-send-btn"
                    title={t('chat.interruptTitle')}
                    aria-label={t('chat.interrupt')}
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
                  title={t('chat.send')}
                  aria-label={t('chat.send')}
                >
                  ↑
                </button>
              )}
            </div>
          </form>
          ) : null}
        </>
      }
      {confirmReq ? <ConfirmDialog request={confirmReq} onClose={() => setConfirmReq(null)} /> : null}
      {modelPanelOpen ? (
        <ModelPanel
          sessionId={agent.id}
          driver={driver}
          api={api}
          onSelect={() => {
            // 设置落驱动后芯片回读驱动事实（失效重取 + 侧栏 agents 快照同步）
            void modelQc.invalidateQueries({ queryKey: ['model-settings', agent.id, agent.instanceId ?? null] });
          }}
          onClose={() => setModelPanelOpen(false)}
        />
      ) : null}
      {activePerm ? (
        <AcpPermissionDialog driver={agent.driver}
          agentId={agent.id}
          request={activePerm}
          api={api}
          onClose={() => setPermDismissed(activePerm.toolUseId)}
        />
      ) : null}
    </div>
  );
}

/// S5 分组展示总候选上限（空查询百条级清单截前 N 条，组头不计）。
const SLASH_GROUP_CAP = 50;

/// C4：/ 补全候选（内置/自定义/技能/插件，前缀过滤）。agent-hub-slash-skills：
/// 清单按当前实例 api + agent cwd 拉取（S2，cwd 变化即重取），上限截断在渲染侧
/// 分组执行（SLASH_GROUP_CAP）。B2 批4：合并 hub-plugin 组（插件 command 贡献物，
/// 独立来源；commands.rs 消费端不动，前端分组渲染加法——设计 §12.6）。
function useSlashMatches(
  query: string | null,
  api: Api,
  cwd: string | null,
  pluginCmds: PluginCommandItem[],
): { name: string; source: string; description: string | null }[] {
  const [all, setAll] = useState<{ name: string; source: string; description: string | null }[]>([]);
  useEffect(() => {
    let alive = true;
    api
      .listCommands(cwd ?? undefined)
      .then((list) => {
        if (alive) setAll(list);
      })
      .catch(() => {
        if (alive) setAll([]);
      });
    return () => {
      alive = false;
    };
  }, [api, cwd]);
  if (query == null) return [];
  const merged = [
    ...all,
    ...pluginCmds.map((c) => ({
      name: c.name,
      source: 'hub-plugin' as const,
      description: c.description ?? c.label,
    })),
  ];
  // round 2 人审（S5）：前缀匹配改模糊查找——大小写不敏感子序列，连续/前缀/
  // 词首优先排序、短名优先，空查询回落全量原序（分组截 50 在渲染侧不变）
  return fuzzyFilter(merged, query);
}

/// hub-plugin command 清单（B2 批4）：hub 本地概念（与实例无关），挂载拉一次 +
/// 30s 轮询（启停插件即生效，BR-03；失败静默回空——补全零插件组即零回归）。
function usePluginCommands(): PluginCommandItem[] {
  const [cmds, setCmds] = useState<PluginCommandItem[]>([]);
  useEffect(() => {
    let alive = true;
    const fetch = (): void => {
      plugins
        .commands()
        .then((list) => {
          if (alive) setCmds(list);
        })
        .catch(() => {
          if (alive) setCmds([]);
        });
    };
    fetch();
    const timer = setInterval(fetch, 30_000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);
  return cmds;
}
