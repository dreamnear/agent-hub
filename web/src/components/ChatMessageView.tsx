import { useEffect, useState, type ReactElement } from 'react';
import type { ChatMessage } from '../types';
import { combineToolCalls, type RenderItem, type ToolCallCard } from '../hooks/combineToolCalls';
import { parseAnsi } from '../lib/ansi';
import { parseCrossSessionMessage } from '../lib/parseCrossSessionMessage';
import { splitImageRefs, imageUrlOf, type ImageRefSegment } from '../lib/image';
import { splitSystemBlocks, type SystemBlockSegment } from '../lib/systemBlocks';
import { useI18n } from '../i18n';
import MarkdownView from './MarkdownView';
import CrossSessionCard from './CrossSessionCard';
import BuiThinking from './bui/Thinking';
import BuiToolChips, {
  type ToolDetailLine,
  type ToolDiff,
  type ToolDiffLine,
  type ToolStep,
} from './bui/ToolChips';
import './ChatMessageView.css';

/// 单条消息渲染入口：支持原始消息与 C6 工具调用收敛卡片两种输入。
export default function ChatMessageView({ item }: { item: RenderItem }): ReactElement {
  if (item.kind === 'tool_call') {
    return <ToolCallCardView card={item} />;
  }
  return <PlainMessageView msg={item.msg} />;
}

/// 图片段：内嵌缩略图（点击灯箱看大图，ESC/点遮罩关闭）；端点取图失败回落
/// 占位 pill，不白屏。原路径仅入 title 不上屏。
function ImageThumb({ seg }: { seg: ImageRefSegment }): ReactElement {
  const [open, setOpen] = useState(false);
  const [failed, setFailed] = useState(false);
  const src = imageUrlOf(seg.path ?? '');
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);
  if (src == null || failed) {
    return (
      <span className="img-pill" title={seg.path}>
        {seg.text}
      </span>
    );
  }
  return (
    <>
      <a
        className="img-thumb"
        title={seg.path}
        href={src}
        onClick={(e) => {
          e.preventDefault();
          setOpen(true);
        }}
      >
        <img src={src} alt={seg.text} loading="lazy" onError={() => setFailed(true)} />
      </a>
      {open ? (
        <div
          className="img-lightbox"
          role="dialog"
          aria-label={seg.text}
          onClick={() => setOpen(false)}
        >
          <img src={src} alt={seg.text} />
        </div>
      ) : null}
    </>
  );
}

/// 文本按图片引用切段渲染：image 段缩略图、text 段原样（统一展示钩子，
/// r72：用户/乐观气泡纯文本、工具卡 detail、tool_result JSON dump 共用）。
/// 单段纯文本直返原文（零 DOM 开销）。
function TextWithImages({ text }: { text: string }): ReactElement {
  const segs = splitImageRefs(text);
  if (segs.length === 1 && segs[0].kind === 'text') return <>{text}</>;
  return (
    <>
      {segs.map((s, i) =>
        s.kind === 'image' ? (
          <ImageThumb key={i} seg={s} />
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

/// 工具名 → BUI 行图标（write/run/read/think，未知归 think）
function iconForTool(name: string | null): string {
  const n = name ?? '';
  if (/^(Write|Edit|MultiEdit|NotebookEdit)/i.test(n)) return 'write';
  if (/^(Bash|Run|Kill|TaskOutput)/i.test(n)) return 'run';
  if (/^(Read|Glob|Grep|WebFetch|WebSearch)/i.test(n)) return 'read';
  return 'think';
}

function ToolCallCardView({ card }: { card: ToolCallCard }): ReactElement {
  const t = useI18n();
  // ACP 分支（批2 任务9）：tool_call update 原样存在 input.sessionUpdate 里，
  // 走 ACP 视图（diff 喂 DiffTable chips）；claude 卡走原 JSON dump 路径
  const acpInput =
    card.input != null && typeof card.input === 'object'
      ? (card.input as Record<string, unknown>)
      : null;
  if (acpInput?.sessionUpdate === 'tool_call') {
    return <AcpToolCardView card={card} />;
  }
  // 状态语义保留：header 文本「工具 · 运行中/失败/完成」（BUI 行内无状态色概念，
  // r16-28 的运行中可见性经文字承载；结果 ANSI 保色经 detail ReactNode 直通）
  const stateLabel = card.result == null ? t('msg.state.running') : card.isError ? t('msg.state.failed') : t('msg.state.completed');
  const output = card.result?.text ?? (card.result?.result != null ? JSON.stringify(card.result.result) : null);
  const inputJson = card.input != null ? JSON.stringify(card.input, null, 2) : null;
  const detail: ToolDetailLine[] = [];
  // r72 图片可视化：输入 JSON（如 Read 的 uploads 图路径）与输出里的图片引用转缩略图
  if (inputJson != null) detail.push({ text: <TextWithImages text={inputJson} /> });
  if (output != null)
    detail.push({
      text: output.includes('\x1b[') ? <AnsiText text={output} /> : <ToolOutputText text={output} />,
    });
  // 输入摘要 chip：压缩 JSON 截断（展开后 detail 有全文）
  const chipRaw = card.input != null ? JSON.stringify(card.input) : '';
  const chip = chipRaw.length > 96 ? `${chipRaw.slice(0, 93)}…` : chipRaw || '—';
  const step: ToolStep = {
    icon: iconForTool(card.toolName),
    label: card.toolName ?? 'tool',
    chip,
    mono: true,
    detailMono: false,
    detail,
  };

  return (
    <div data-bui className="not-prose max-w-full [&>div]:!min-h-0 [&>div]:!max-w-full">
      <BuiToolChips
        static
        steps={[step]}
        diffs={[]}
        labels={{ header: `${card.toolName ?? 'tool'} · ${stateLabel}`, more: '' }}
      />
    </div>
  );
}

/// ACP 工具卡（批2 任务9）：tool_call + tool_call_update 合并卡 → BuiToolChips。
/// diff 内容块喂 diffs/diffLines（DiffTable chips + hover 预览）；content 文本块进
/// detail；omp 特有工具类型不硬编码——无 title 用 kind 兜底，全缺省即通用卡降级。
function acpContentBlocks(update: Record<string, unknown> | null): Record<string, unknown>[] {
  const raw = update?.content;
  return Array.isArray(raw) ? (raw as Record<string, unknown>[]) : [];
}

function acpToolView(card: ToolCallCard): {
  label: string;
  stateKey: string;
  icon: string;
  chip: string;
  detail: ToolDetailLine[];
  diffs: ToolDiff[];
  diffLines: Record<string, ToolDiffLine[]>;
} {
  const input = (card.input ?? {}) as Record<string, unknown>;
  const resultUpdate = (card.result?.result ?? null) as Record<string, unknown> | null;
  const status = typeof resultUpdate?.status === 'string' ? resultUpdate.status : null;
  // 状态文案渲染期翻译（C3 i18n）：此处只定 key
  const stateKey = status === 'failed' ? 'msg.state.failed' : status === 'completed' ? 'msg.state.completed' : 'msg.state.running';
  const kind = typeof input.kind === 'string' ? input.kind : '';
  const title = typeof input.title === 'string' && input.title ? input.title : kind || 'tool';
  // ACP kind → BUI 行图标（omp 特有类型不硬编码，未知归 think）
  const icon =
    kind === 'edit' || kind === 'delete' || kind === 'move'
      ? 'write'
      : kind === 'execute'
        ? 'run'
        : kind === 'read' || kind === 'search' || kind === 'fetch'
          ? 'read'
          : iconForTool(title);
  const detail: ToolDetailLine[] = [];
  const diffs: ToolDiff[] = [];
  const diffLines: Record<string, ToolDiffLine[]> = {};
  // ocr-review 中：同名不同目录/省略 path 的 diff 块撞名互覆——撞名追加 #n
  // 保唯一（显示与 diffLines 查键同字段，ToolChips 以 file 双向查）
  const seenFiles = new Set<string>();
  for (const block of [...acpContentBlocks(input), ...acpContentBlocks(resultUpdate)]) {
    if (block.type === 'diff') {
      const path = typeof block.path === 'string' ? block.path : 'file';
      const base = path.split('/').pop() || path;
      let file = base;
      let n = 2;
      while (seenFiles.has(file)) {
        file = `${base}#${n}`;
        n += 1;
      }
      seenFiles.add(file);
      const oldText = typeof block.oldText === 'string' ? block.oldText : '';
      const newText = typeof block.newText === 'string' ? block.newText : '';
      // ponytail: ACP diff 即 old/new 整块替换对——old 全部记 del、new 全部记 add，
      // 不做逐行 diff 算法；要行级对齐再引 diff 库
      const delLines = oldText ? oldText.replace(/\n$/, '').split('\n') : [];
      const addLines = newText ? newText.replace(/\n$/, '').split('\n') : [];
      diffs.push({ file, add: addLines.length, del: delLines.length });
      diffLines[file] = [
        ...delLines.map((text) => ({ text, tone: 'del' as const })),
        ...addLines.map((text) => ({ text, tone: 'add' as const })),
      ];
    } else if (block.type === 'content') {
      const inner = (block.content ?? {}) as Record<string, unknown>;
      if (typeof inner.text === 'string' && inner.text) {
        detail.push({ text: <TextWithImages text={inner.text} /> });
      }
    }
  }
  const chip =
    diffs[0]?.file ??
    ((typeof input.title === 'string' && input.title ? input.title : '') || kind || '—');
  return { label: title, stateKey, icon, chip, detail, diffs, diffLines };
}

function AcpToolCardView({ card }: { card: ToolCallCard }): ReactElement {
  const t = useI18n();
  const view = acpToolView(card);
  const step: ToolStep = {
    icon: view.icon,
    label: view.label,
    chip: view.chip,
    mono: true,
    detailMono: false,
    detail: view.detail,
  };
  return (
    <div data-bui className="not-prose max-w-full [&>div]:!min-h-0 [&>div]:!max-w-full">
      <BuiToolChips
        static
        steps={[step]}
        diffs={view.diffs}
        diffLines={view.diffLines}
        labels={{ header: `${view.label} · ${t(view.stateKey)}`, more: '' }}
      />
    </div>
  );
}

function PlainMessageView({ msg }: { msg: ChatMessage }): ReactElement | null {
  const t = useI18n();
  const when = msg.ts ? new Date(msg.ts).toLocaleTimeString() : '';

  if (msg.kind === 'thinking') {
    // BUI Thinking（Reasoning 变体，static 受控）：折叠默认收起，展开看全文；
    // 时间戳走 row.secondary；流式追加不闪跳（无自跑动画序列）。
    // 思考文本里内嵌系统块时按段交替：text 段各自一个折叠思考块、sys 段摘要条，
    // 顺序保持；无块走原渲染（splitSystemBlocks 快路径单段零变化）
    const { hasSys, parts } = visibleSegments(splitSystemBlocks(msg.text ?? ''));
    if (parts.length === 0) return null; // 整条只剩被隐藏的块 → 不留空行
    if (!hasSys) {
      return (
        <div data-bui className="max-w-full [&>div]:!min-h-0 [&>div]:!max-w-full">
          <BuiThinking
            mode="static"
            variant="Reasoning"
            active="thinking"
            done={t('msg.thinkingDone')}
            working={false}
            defaultExpanded={false}
            rows={[{ primary: msg.text ?? '…', secondary: when }]}
          />
        </div>
      );
    }
    return (
      <div data-bui className="max-w-full [&>div]:!min-h-0 [&>div]:!max-w-full">
        {parts.map((p, i) =>
          p.kind !== 'text' ? (
            <SystemBlockBar key={i} seg={p} />
          ) : p.text.trim() === '' ? null : (
            <BuiThinking
              key={i}
              mode="static"
              variant="Reasoning"
              active="thinking"
              done={t('msg.thinkingDone')}
              working={false}
              defaultExpanded={false}
              rows={[{ primary: p.text, secondary: when }]}
            />
          ),
        )}
      </div>
    );
  }

  if (msg.kind === 'tool_result') {
    return (
      <details className="chat-card" data-kind="tool_result">
        <summary>
          <span className="chat-badge">result</span>
          <span className="chat-detail">{msg.text ?? msg.toolUseId ?? 'result'}</span>
          <span className="chat-time">{when}</span>
        </summary>
        <pre className="chat-json">
          <TextWithImages text={JSON.stringify(msg.result, null, 2)} />
        </pre>
      </details>
    );
  }

  // user / assistant 消息（Penpot 规格：user 右对齐气泡内文字居中；
  // assistant 黑圆头像 + 左对齐无气泡正文）。分流顺序：
  // 1) 跨会话消息（user 文本含 <cross-session-message> 标签 → CrossSessionCard，早于其他分流，失败回退纯文本）
  // 2) 含 ANSI 转义 → 保色渲染；3) assistant 纯文本 → markdown；user → 纯文本。
  const text = msg.text ?? '';
  if (msg.kind === 'user') {
    const cross = parseCrossSessionMessage(text);
    if (cross) return <CrossSessionCard msg={cross} when={when} />;
  }
  const hasAnsi = text.includes('\x1b[');
  // 系统通知块（task-notification/system-reminder）折叠成摘要条：无块的普通消息
  // 走原渲染（splitSystemBlocks 快路径返回原文单段）；ANSI 文本同样切段——
  // sys 段出摘要条、text 段保持保色渲染（任何携带系统标签的路径都过折叠）
  const { hasSys, parts } = visibleSegments(splitSystemBlocks(text));
  if (parts.length === 0) return null; // 纯通知注入行 → 整条不渲染
  // 系统事件行：jsonl 里是 user 类型注入行（isMeta），语义上是 agent 的完成事件
  // ——切分后只剩系统段、没有任何正文时归 agent 侧左对齐（真实用户消息含正文，
  // 仍走右侧气泡）
  const isSystemEvent =
    msg.kind === 'user' && hasSys && parts.every((p) => p.kind !== 'text' || p.text.trim() === '');
  const onAgentSide = msg.kind === 'assistant' || isSystemEvent;
  const renderTextPart = (part: string): ReactElement => {
    if (hasAnsi) return <AnsiText text={part} />;
    if (onAgentSide) return <MarkdownView text={part} />;
    // user 纯文本：上传图片路径收敛为缩略图（加载失败回落 [Image #n] pill），
    // 原路径仅入 title 不上屏；历史消息与乐观气泡共用本渲染点
    return (
      <span className="chat-plain">
        <TextWithImages text={part} />
      </span>
    );
  };
  const content = hasSys ? (
    <>
      {parts.map((p, i) =>
        p.kind !== 'text' ? <SystemBlockBar key={i} seg={p} /> : renderTextPart(p.text),
      )}
    </>
  ) : (
    renderTextPart(text)
  );
  if (onAgentSide) {
    return (
      <div className="chat-msg chat-msg--assistant" data-kind={msg.kind}>
        <span className="assistant-avatar" aria-hidden="true">
          ◆
        </span>
        <div className="chat-assistant-body">{content}</div>
      </div>
    );
  }
  return (
    <div className="chat-msg chat-msg--user" data-kind={msg.kind}>
      <div className="chat-bubble chat-bubble--user">
        {content}
        <span className="chat-time">{when}</span>
      </div>
    </div>
  );
}

/// 渲染层过滤（2026-09-30 用户指示「task-notify 这个不需要显示出来」）：
/// task-notification 折叠条不渲染，只丢展示——splitSystemBlocks 的切分语义与
/// systemBlocks 单测零改动（数据层红线）。hasSys 取自过滤前的段：① 整条只剩被
/// 隐藏块时调用方整条返回 null，不留空头像行；② 有隐藏块又剩文本段时也不能回落
/// renderTextPart(text)，那会把隐藏块的原文又渲染出来。
function visibleSegments(raw: SystemBlockSegment[]): {
  hasSys: boolean;
  parts: SystemBlockSegment[];
} {
  const parts = raw.filter((p) => p.kind !== 'sys' || p.tag !== 'task-notification');
  return { hasSys: raw.some((p) => p.kind !== 'text'), parts };
}

/// 工具输出文本：与消息文本共用 splitSystemBlocks 通用识别——工具卡 detail 里的
/// 系统块同样折叠成摘要条（source="tool" 加「工具输出」badge 区分来源）；
/// 无块原文单段走 TextWithImages 零变化。取证：tool_result 里 XML 标签原文、
/// 换行为字面 \n（JSON 转义形态），indexOf/[\s\S] 识别天然兼容，不需反转义。
function ToolOutputText({ text }: { text: string }): ReactElement {
  const { hasSys, parts } = visibleSegments(splitSystemBlocks(text));
  if (!hasSys) return <TextWithImages text={text} />;
  return (
    <>
      {parts.map((p, i) =>
        p.kind !== 'text' ? (
          <SystemBlockBar key={i} seg={p} source="tool" />
        ) : (
          <TextWithImages key={i} text={p.text} />
        ),
      )}
    </>
  );
}

/// 系统通知块摘要条：一行弱化条（⚙ + 标签 + <summary> 文本），点击展开原始块
/// （等宽小字，内容不丢）；复用 chat-card 折叠盒与 chat-json 等宽样式。
/// source="tool"（工具卡 detail 内）加「工具输出」badge + 更紧凑样式区分来源。
function SystemBlockBar({ seg, source }: { seg: SystemBlockSegment; source?: 'msg' | 'tool' }): ReactElement {
  const t = useI18n();
  // 语义段（2026-10-02 通用检测）：command = 紧凑执行条（单行不折叠）；
  // stdout = 执行结果折叠卡（summary 为首行预览）；sys = 原折叠卡
  if (seg.kind === 'command') {
    return (
      <div className={`chat-card sys-cmd${source === 'tool' ? ' sys-block--tool' : ''}`} role="status">
        <span aria-hidden="true">⌘</span>
        <span className="chat-detail">{seg.label}</span>
      </div>
    );
  }
  if (seg.kind === 'stdout') {
    return (
      <details className={`chat-card sys-block sys-stdout${source === 'tool' ? ' sys-block--tool' : ''}`}>
        <summary>
          <span aria-hidden="true">↳</span>
          {source === 'tool' && <span className="chat-badge">{t('msg.toolOutput')}</span>}
          <span className="chat-badge">{t('sys.stdout')}</span>
          <span className="chat-detail">{seg.label}</span>
        </summary>
        <pre className="chat-json">{seg.text}</pre>
      </details>
    );
  }
  return (
    <details className={`chat-card sys-block${source === 'tool' ? ' sys-block--tool' : ''}`}>
      <summary>
        <span aria-hidden="true">⚙</span>
        {source === 'tool' && <span className="chat-badge">{t('msg.toolOutput')}</span>}
        <span className="chat-badge">{seg.tag}</span>
        <span className="chat-detail">{seg.label}</span>
      </summary>
      <pre className="chat-json">{seg.text}</pre>
    </details>
  );
}

function AnsiText({ text }: { text: string }): ReactElement {
  const segments = parseAnsi(text);
  return (
    <span className="ansi">
      {segments.map((s, i) => (
        <span
          key={i}
          style={{
            color: s.fg,
            background: s.bg,
            fontWeight: s.bold ? 700 : undefined,
          }}
        >
          {s.text}
        </span>
      ))}
    </span>
  );
}

/// 便捷导出：直接渲染原始消息数组（合并工具调用后）。
export function ChatMessageList({ messages }: { messages: ChatMessage[] }): ReactElement {
  return (
    <>
      {combineToolCalls(messages).map((item) => (
        <ChatMessageView key={item.key} item={item} />
      ))}
    </>
  );
}
