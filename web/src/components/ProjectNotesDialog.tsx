import { useEffect, useRef, useState, type ReactElement } from 'react';
import { api, type Api } from '../api';
import './ProjectNotesDialog.css';

/// 工程便签悬浮卡（agent-hub-notes 增强）：绑定归一化 cwd 的共享 Markdown 便签——
/// 同一工程所有会话（Claude Code 与 ACP）打开同一份内容。
/// 多实例（任务8）：便签读写按 agent 所属实例路由（api per-instance），同 cwd 不同
/// 实例各自独立（各实例的 notes.json 天然隔离）；本地 localStorage 记忆键也带实例维。
/// PC：fixed 悬浮卡，头部按住拖动，位置按工程记忆（localStorage，重开还原）；
/// ≤768px 移动端：不拖动不记位，保持居中全宽面板。
/// 钉住（pin）按工程记忆：钉住后打开该工程任一会话自动展开（App 层消费 isNotePinned）。
/// 内容可能含端口/测试账密——安全红线：不进日志；仅本地 localStorage 存位置/钉态。

const PIN_PREFIX = 'notes_card_pin:';
const POS_PREFIX = 'notes_card_pos:';
const SIZE_PREFIX = 'notes_card_size:';
const EDGE = 8; // 拖出视口边缘的 clamp 边距

/// 记忆键（任务8）：带实例段——同 cwd 不同实例互不串（本机段为空保持现状键）。
function noteKey(prefix: string, cwd: string, instanceId: string | null): string {
  return `${prefix}${instanceId ?? ''}:${cwd}`;
}

/// 卡片最小尺寸（防拖没）
export const NOTE_MIN_SIZE = { w: 240, h: 180 } as const;

export interface NoteSize {
  w: number;
  h: number;
}

export function clampNoteSize(s: NoteSize): NoteSize {
  return {
    w: Math.max(NOTE_MIN_SIZE.w, Math.min(s.w, window.innerWidth - 2 * EDGE)),
    h: Math.max(NOTE_MIN_SIZE.h, Math.min(s.h, window.innerHeight - 2 * EDGE)),
  };
}

/// 读记忆尺寸（clamp 到最小与视口内）；坏数据 → null 走 CSS 默认
export function loadNoteSize(cwd: string, instanceId: string | null = null): NoteSize | null {
  try {
    const raw = localStorage.getItem(noteKey(SIZE_PREFIX, cwd, instanceId));
    if (!raw) return null;
    const s = JSON.parse(raw) as NoteSize;
    if (typeof s.w !== 'number' || typeof s.h !== 'number') return null;
    return clampNoteSize(s);
  } catch {
    return null;
  }
}

export function saveNoteSize(cwd: string, size: NoteSize, instanceId: string | null = null): void {
  try {
    localStorage.setItem(noteKey(SIZE_PREFIX, cwd, instanceId), JSON.stringify(size));
  } catch {
    // ignore：尺寸记忆属增强体验
  }
}

/// 移动端判定（挂载时一次；resize 切换不做——刷新即按当前视口取态）
export function isMobileViewport(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(max-width: 768px)').matches;
}

export function isNotePinned(cwd: string, instanceId: string | null = null): boolean {
  try {
    return localStorage.getItem(noteKey(PIN_PREFIX, cwd, instanceId)) === '1';
  } catch {
    return false;
  }
}

export function setNotePinned(cwd: string, pinned: boolean, instanceId: string | null = null): void {
  try {
    if (pinned) localStorage.setItem(noteKey(PIN_PREFIX, cwd, instanceId), '1');
    else localStorage.removeItem(noteKey(PIN_PREFIX, cwd, instanceId));
  } catch {
    // 隐私模式等 localStorage 不可用：钉态退化为不记忆
  }
}

export interface NotePos {
  x: number;
  y: number;
}

/// 读记忆位置并 clamp 回当前视口（视口变小后防卡片出界）
export function loadNotePos(cwd: string, cardW: number, cardH: number, instanceId: string | null = null): NotePos | null {
  try {
    const raw = localStorage.getItem(noteKey(POS_PREFIX, cwd, instanceId));
    if (!raw) return null;
    const p = JSON.parse(raw) as NotePos;
    if (typeof p.x !== 'number' || typeof p.y !== 'number') return null;
    return clampNotePos(p, cardW, cardH);
  } catch {
    return null;
  }
}

export function clampNotePos(p: NotePos, cardW: number, cardH: number): NotePos {
  return {
    x: Math.min(Math.max(p.x, EDGE), Math.max(EDGE, window.innerWidth - cardW - EDGE)),
    y: Math.min(Math.max(p.y, EDGE), Math.max(EDGE, window.innerHeight - cardH - EDGE)),
  };
}

export function saveNotePos(cwd: string, pos: NotePos, instanceId: string | null = null): void {
  try {
    localStorage.setItem(noteKey(POS_PREFIX, cwd, instanceId), JSON.stringify(pos));
  } catch {
    // ignore：位置记忆属增强体验
  }
}

export default function ProjectNotesDialog({
  cwd,
  title,
  api: instApi = api,
  instanceId = null,
  onClose,
}: {
  cwd: string;
  title: string;
  /** 便签读写按实例路由（任务8）：缺省=本机 api（现状零破坏） */
  api?: Api;
  instanceId?: string | null;
  onClose: () => void;
}): ReactElement {
  const [loaded, setLoaded] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [err, setErr] = useState('');
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [pos, setPos] = useState<NotePos | null>(null);
  const [size, setSize] = useState<NoteSize | null>(null);
  const [dragging, setDragging] = useState(false);
  const [resizing, setResizing] = useState(false);
  const [pinned, setPinned] = useState(() => isNotePinned(cwd, instanceId));
  const cardRef = useRef<HTMLDivElement | null>(null);
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);
  const mobile = isMobileViewport();

  // ocr-review 中：App 层渲染本组件未设 key，实例复用切工程时钉住态须随 cwd
  // 重载（否则 A 工程的钉态串台到 B 工程）；多实例：cwd+实例维同判
  useEffect(() => {
    setPinned(isNotePinned(cwd, instanceId));
  }, [cwd, instanceId]);

  useEffect(() => {
    let alive = true;
    instApi
      .getNote(cwd)
      .then((r) => {
        if (!alive) return;
        setLoaded(r.content ?? '');
        setDraft(r.content ?? '');
      })
      .catch((e: unknown) => {
        if (!alive) return;
        setErr(String(e));
        setLoaded('');
      });
    return () => {
      alive = false;
    };
  }, [cwd, instApi]);

  // 初始定位：记忆位置（clamp 后）> 首次顶部居中（量尺寸后落位）；尺寸记忆一并还原。
  // ocr-review 中：依赖 [cwd, mobile]——实例复用切工程时为新工程重载记忆位置/尺寸
  useEffect(() => {
    if (mobile) return;
    const el = cardRef.current;
    if (!el) return;
    const rememberedSize = loadNoteSize(cwd, instanceId);
    if (rememberedSize) setSize(rememberedSize);
    const r = el.getBoundingClientRect();
    const remembered = loadNotePos(cwd, r.width, r.height, instanceId);
    setPos(
      remembered ?? {
        x: Math.max(EDGE, (window.innerWidth - r.width) / 2),
        y: Math.max(EDGE, window.innerHeight * 0.12),
      },
    );
  }, [cwd, mobile]);

  // 视口缩放时把已落位卡片 clamp 回可视区（拖到右侧后缩窗防卡片丢失/溢出）
  useEffect(() => {
    const onResize = () => {
      const el = cardRef.current;
      if (!el) return;
      setPos((p) => (p ? clampNotePos(p, el.offsetWidth, el.offsetHeight) : p));
    };
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);

  const onHeadPointerDown = (e: React.PointerEvent<HTMLDivElement>): void => {
    if (mobile || !cardRef.current) return;
    // 头部内按钮（图钉/关闭）不启动拖动：pointer capture 会把 click 目标劫持到头部，
    // 按钮 onClick 被吞（实机 r2 复现：点图钉/关闭无响应）
    if ((e.target as HTMLElement).closest('button')) return;
    const r = cardRef.current.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - r.left, dy: e.clientY - r.top };
    setDragging(true);
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // happy-dom 等环境无 pointer capture：拖动仍走 React 合成事件
    }
  };

  const onHeadPointerMove = (e: React.PointerEvent<HTMLDivElement>): void => {
    const d = dragRef.current;
    const el = cardRef.current;
    if (!d || !el) return;
    setPos(
      clampNotePos({ x: e.clientX - d.dx, y: e.clientY - d.dy }, el.offsetWidth, el.offsetHeight),
    );
  };

  const onHeadPointerUp = (): void => {
    if (!dragRef.current) return;
    dragRef.current = null;
    setDragging(false);
    if (pos) saveNotePos(cwd, pos, instanceId);
  };

  // 右下角 resize（与拖动同款 pointer events；移动端不适用）：
  // 拖角 → 宽高随指针位移（clamp 最小 240×180 + 视口内），松开写 localStorage
  const resizeRef = useRef<{ sx: number; sy: number; w: number; h: number } | null>(null);

  const onResizePointerDown = (e: React.PointerEvent<HTMLDivElement>): void => {
    if (mobile || !cardRef.current) return;
    const r = cardRef.current.getBoundingClientRect();
    resizeRef.current = { sx: e.clientX, sy: e.clientY, w: r.width, h: r.height };
    setResizing(true);
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // 同拖动：无 capture 环境走 React 合成事件
    }
  };

  // 需额外跟踪最终尺寸（onUp 闭包里的 size 可能仍是上一次 state 快照）
  const resizeSizeRef = useRef<NoteSize | null>(null);

  const onResizePointerMove = (e: React.PointerEvent<HTMLDivElement>): void => {
    const d = resizeRef.current;
    if (!d) return;
    const next = clampNoteSize({ w: d.w + (e.clientX - d.sx), h: d.h + (e.clientY - d.sy) });
    resizeSizeRef.current = next;
    setSize(next);
    // 变大后可能推卡片出视口右/下缘——位置一并 clamp
    const el = cardRef.current;
    if (el) setPos((p) => (p ? clampNotePos(p, next.w, next.h) : p));
  };

  const onResizePointerUp = (): void => {
    if (!resizeRef.current) return;
    resizeRef.current = null;
    setResizing(false);
    if (resizeSizeRef.current) saveNoteSize(cwd, resizeSizeRef.current, instanceId);
  };

  const togglePin = (): void => {
    const next = !pinned;
    setPinned(next);
    setNotePinned(cwd, next, instanceId);
  };

  return (
    <div
      ref={cardRef}
      className={`notes-card ${pos && !mobile ? 'notes-card--float' : ''} ${dragging || resizing ? 'notes-card--dragging' : ''}`}
      role="dialog"
      aria-label="工程便签"
      style={
        pos && !mobile
          ? { left: pos.x, top: pos.y, ...(size ? { width: size.w, height: size.h } : {}) }
          : undefined
      }
      onKeyDown={(e) => {
        if (e.key === 'Escape' && !busy) onClose();
      }}
    >
      <div
        className="notes-head"
        onPointerDown={onHeadPointerDown}
        onPointerMove={onHeadPointerMove}
        onPointerUp={onHeadPointerUp}
        onPointerCancel={onHeadPointerUp}
      >
        <span className="notes-title" aria-hidden="true">
          📋 工程便签
        </span>
        <span className="notes-head-actions">
          <button
            type="button"
            className="notes-pin-btn"
            aria-pressed={pinned}
            aria-label={pinned ? '取消钉住' : '钉住：打开本工程会话时自动展开'}
            title={pinned ? '已钉住：本工程任一会话打开时自动展开' : '钉住后随本工程会话自动打开'}
            onClick={togglePin}
          >
            {pinned ? '📌' : '📍'}
          </button>
          <button type="button" className="notes-close-btn" aria-label="关闭便签" onClick={onClose}>
            ×
          </button>
        </span>
      </div>
      <p className="dialog-subtitle notes-subtitle">
        {title} · {cwd}（同一工程的所有会话共享）
      </p>
      {err ? <p className="dialog-error">{err}</p> : null}
      <textarea
        className="notes-textarea"
        value={draft}
        disabled={loaded == null}
        onChange={(e) => {
          setDraft(e.target.value);
          setSaved(false);
        }}
        rows={12}
        spellCheck={false}
        aria-label="便签内容"
        placeholder="服务端口、测试账号等备忘（自由文本，保存后同一工程所有会话可见）"
      />
      <div className="dialog-actions">
        {saved ? (
          <span className="notes-saved" role="status">
            已保存
          </span>
        ) : null}
        <button
          type="button"
          className="btn-primary"
          onClick={() => void save()}
          disabled={busy || loaded == null || draft === loaded}
        >
          保存
        </button>
      </div>
      {/* 右下角 resize 握把（桌面）：pointer events 拖拽改宽高，最小 240×180 */}
      {!mobile ? (
        <div
          className="notes-resize-handle"
          role="separator"
          aria-label="调整便签大小"
          onPointerDown={onResizePointerDown}
          onPointerMove={onResizePointerMove}
          onPointerUp={onResizePointerUp}
          onPointerCancel={onResizePointerUp}
        />
      ) : null}
    </div>
  );

  async function save(): Promise<void> {
    setErr('');
    setSaved(false);
    setBusy(true);
    try {
      await instApi.putNote(cwd, draft);
      setLoaded(draft);
      setSaved(true);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }
}
