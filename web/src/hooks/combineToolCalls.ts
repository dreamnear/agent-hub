import type { ChatMessage } from '../types';

/// 工具调用收敛卡片（P5 C6）：tool_use 与对应 tool_result 合并为单条，
/// 状态点 = result 未到（运行中，灰）/ 到达 error（红）/ 到达无 error（绿）。
export interface ToolCallCard {
  kind: 'tool_call';
  key: string;
  toolUseId: string;
  toolName: string | null;
  input: unknown;
  result: ChatMessage | null;
  isError: boolean;
}

/// 任务清单（preview 反馈）：TaskCreate/TaskUpdate 聚合为清单，状态随 Update 演进。
/// taskId 语义（review-ui-r2 MINOR 实测）：harness 全局递增（非会话内从 1 起），
/// 真实值从 TaskCreate 的 tool_result 提取；提取不到（result 缺失/格式未识别）回退位置序号。
export interface TaskItem {
  taskId: string;
  subject: string;
  status: 'pending' | 'in_progress' | 'completed';
}

export type RenderItem =
  | { kind: 'message'; key: string; msg: ChatMessage }
  | ToolCallCard;

const TASK_TOOLS = new Set(['TaskCreate', 'TaskUpdate', 'TaskList', 'TaskGet']);

/// 从 TaskCreate 的 result 提取 harness 分配的真实 taskId。
/// 覆盖三种形态：result 对象带 taskId 字段 / JSON 文本 "taskId": 33 / 文本 Created task #33。
function extractCreatedTaskId(m: ChatMessage): string | null {
  const r: unknown = m.result;
  if (r != null && typeof r === 'object') {
    const tid = (r as { taskId?: unknown }).taskId;
    if (tid != null && /^\d+$/.test(String(tid))) return String(tid);
  }
  const texts = [m.text ?? '', r != null && typeof r !== 'object' ? String(r) : ''];
  for (const s of texts) {
    const hit =
      s.match(/"taskId"\s*:\s*"?(\d+)/) ?? s.match(/taskId\s*[=:]\s*#?(\d+)/i) ?? s.match(/#(\d+)/);
    if (hit) return hit[1];
  }
  return null;
}

/// 抽取会话任务清单（固定栏渲染用，preview 反馈：不随消息流滚动）。
export function extractTaskList(messages: ChatMessage[]): TaskItem[] {
  interface Draft {
    subject: string;
    status: TaskItem['status'];
    realId: string | null;
    duplicate?: boolean;
    deleted?: boolean;
  }
  const drafts: Draft[] = [];
  const byUseId = new Map<string, Draft>();
  // 第一遍：TaskCreate 按出现顺序占位
  for (const m of messages) {
    if (m.kind !== 'tool_use' || m.toolName !== 'TaskCreate') continue;
    const input = (m.input ?? {}) as Record<string, unknown>;
    const d: Draft = {
      subject: typeof input.subject === 'string' ? input.subject : '(untitled)',
      status: 'pending',
      realId: null,
    };
    drafts.push(d);
    if (m.toolUseId) byUseId.set(m.toolUseId, d);
  }
  // 第二遍：TaskCreate 的 result 回填真实 taskId；同 realId 去重（重放/compact 场景防御，
  // 后出现的同 id Create 合并进首个，不新增行）
  const seenRealIds = new Set<string>();
  for (const m of messages) {
    if (m.kind !== 'tool_result') continue;
    const d = m.toolUseId ? byUseId.get(m.toolUseId) : undefined;
    if (!d) continue;
    d.realId = extractCreatedTaskId(m);
    if (d.realId != null) {
      if (seenRealIds.has(d.realId)) {
        d.duplicate = true;
      } else {
        seenRealIds.add(d.realId);
      }
    }
  }
  // 第三遍：TaskUpdate 按 realId 精确匹配；未回填时按位置序号回退（旧行为兼容）
  for (const m of messages) {
    if (m.kind !== 'tool_use' || m.toolName !== 'TaskUpdate') continue;
    const input = (m.input ?? {}) as Record<string, unknown>;
    const tid =
      typeof input.taskId === 'string' || typeof input.taskId === 'number'
        ? String(input.taskId)
        : null;
    if (tid == null) continue;
    let d = drafts.find((x) => x.realId === tid);
    if (!d) {
      const idx = Number(tid) - 1;
      d = Number.isInteger(idx) && drafts[idx] ? drafts[idx] : undefined;
    }
    if (!d) continue;
    if (typeof input.subject === 'string' && input.subject) d.subject = input.subject;
    if (input.status === 'deleted') {
      // 反馈轮 9：deleted = 任务已从 harness 移除，聚合时剔除
      d.deleted = true;
    } else if (
      input.status === 'pending' ||
      input.status === 'in_progress' ||
      input.status === 'completed'
    ) {
      d.status = input.status;
    }
  }
  return drafts
    .filter((d) => !d.duplicate && !d.deleted)
    .map((d, i) => ({
      taskId: d.realId ?? String(i + 1),
      subject: d.subject,
      status: d.status,
    }));
}

/// 相邻 ACP 流式 chunk 合并（批2 任务8）：omp 逐词推 agent_message_chunk /
/// agent_thought_chunk，不合并会渲染成几十个碎片气泡。仅合并 rawType='acp_chunk'
/// 且同 kind 相邻的消息——claude jsonl 消息（rawType 为 jsonl type）零影响。
/// key 取首块（流式增长时 React 复用同一 DOM）。
export function mergeAcpChunks(messages: ChatMessage[]): ChatMessage[] {
  const out: ChatMessage[] = [];
  for (const m of messages) {
    const prev = out[out.length - 1];
    if (
      prev != null &&
      m.rawType === 'acp_chunk' &&
      prev.rawType === 'acp_chunk' &&
      m.kind === prev.kind &&
      (m.kind === 'assistant' || m.kind === 'thinking')
    ) {
      out[out.length - 1] = { ...prev, text: (prev.text ?? '') + (m.text ?? '') };
    } else {
      out.push(m);
    }
  }
  return out;
}

/// ACP plan → TaskItem（批2 任务10）：取**最后一条** plan update（原地刷新语义——
/// 每条新 plan 覆盖前值），entries 顺序即清单顺序。无 plan 返回 null（调用方回退）。
export function extractAcpPlan(messages: ChatMessage[]): TaskItem[] | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.rawType !== 'acp_plan') continue;
    const entries = (m.result as { entries?: unknown } | null)?.entries;
    if (!Array.isArray(entries)) return [];
    return entries.map((e, idx) => {
      const entry = (e ?? {}) as { content?: unknown; status?: unknown };
      const status =
        entry.status === 'completed'
          ? 'completed'
          : entry.status === 'in_progress'
            ? 'in_progress'
            : 'pending';
      return {
        taskId: String(idx + 1),
        subject: typeof entry.content === 'string' ? entry.content : '(untitled)',
        status,
      };
    });
  }
  return null;
}

export function combineToolCalls(allMessages: ChatMessage[]): RenderItem[] {
  // acp_plan 不进消息流（批2 任务10）：已在 TaskListBar 固定栏原地刷新，流内重复渲染为噪音
  const messages = mergeAcpChunks(allMessages).filter((m) => m.rawType !== 'acp_plan');
  const out: RenderItem[] = [];
  const cards = new Map<string, ToolCallCard>();

  // 第一遍：会话是否产出任务清单（决定 Task 系散卡是否吸收），并收集 Task 系调用 id
  const hasTaskCard = extractTaskList(messages).length > 0;
  const absorbedTaskUseIds = new Set<string>();
  for (const m of messages) {
    if (m.kind !== 'tool_use' || !m.toolName || !TASK_TOOLS.has(m.toolName)) continue;
    absorbedTaskUseIds.add(m.toolUseId ?? '');
  }

  for (const m of messages) {
    if (m.kind === 'tool_use') {
      if (m.toolName && TASK_TOOLS.has(m.toolName)) {
        if (!hasTaskCard && m.toolName !== 'TaskCreate' && m.toolName !== 'TaskUpdate') {
          // 无任务卡的会话：TaskList/TaskGet 按普通工具卡渲染（保留只读查询痕迹）
          const key = `call-${m.toolUseId ?? out.length}`;
          const card: ToolCallCard = {
            kind: 'tool_call',
            key,
            toolUseId: m.toolUseId ?? '',
            toolName: m.toolName,
            input: m.input,
            result: null,
            isError: false,
          };
          cards.set(card.toolUseId || key, card);
          out.push(card);
        }
        // 有任务卡时 Task 系 tool_use 连同其 result 一并吸收（preview 反馈：散卡噪音）
        continue;
      }
      const key = `call-${m.toolUseId ?? out.length}`;
      const card: ToolCallCard = {
        kind: 'tool_call',
        key,
        toolUseId: m.toolUseId ?? '',
        toolName: m.toolName,
        input: m.input,
        result: null,
        isError: false,
      };
      cards.set(card.toolUseId || key, card);
      out.push(card);
    } else if (m.kind === 'tool_result') {
      const card = m.toolUseId ? cards.get(m.toolUseId) : undefined;
      if (card) {
        card.result = m;
        card.isError = m.error === true;
      } else if (hasTaskCard && m.toolUseId != null && absorbedTaskUseIds.has(m.toolUseId)) {
        // 被吸收的只读 Task 调用的 result：不渲染
        continue;
      } else {
        // 孤儿 result（无前置 tool_use）原样渲染
        out.push({ kind: 'message', key: `r-${m.ts ?? ''}-${out.length}`, msg: m });
      }
    } else {
      out.push({ kind: 'message', key: `m-${m.ts ?? ''}-${out.length}`, msg: m });
    }
  }

  // 任务清单不进消息流（preview 反馈：固定于主对话区顶部，extractTaskList 单独取）
  return out;
}
