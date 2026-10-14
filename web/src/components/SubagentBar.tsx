import './SubagentBar.css';
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import type { SubagentEntry } from '../types';

/// 测量不可用环境（happy-dom/零宽隐藏态）的兜底可见数（2026-10-01「前 3」定案沿用）。
const FALLBACK_PREVIEW = 3;

/// subagent 面板（agent-hub-subagent-strip 单行化）：活跃 chip 单行 chip 流——
/// 一行能排多少排多少（按容器实测宽度装箱），永不换行；溢出收进「+N」展开器，
/// 展开解除单行约束（wrap 全量列表），可再收起；容器变宽即时重排（ResizeObserver
/// + window resize 双通道）。History 入口与展开器宽度参与装箱预留，恒可达。
/// 宽度测量不可用（SSR/测试环境/零宽隐藏）回退前 3 兜底；无活跃条目时不渲染。
export default function SubagentBar({
  subagents,
  finished,
  activeId,
  onSelect,
}: {
  subagents: SubagentEntry[];
  finished: SubagentEntry[];
  activeId: string | null;
  onSelect: (agentId: string) => void;
}): ReactElement | null {
  const t = useI18n();
  const [expanded, setExpanded] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  /// 装箱结果：单行可见 chip 数；null = 未测量/测量不可用（走 FALLBACK_PREVIEW）
  const [fitCount, setFitCount] = useState<number | null>(null);
  const rowRef = useRef<HTMLDivElement | null>(null);
  const measureRef = useRef<HTMLDivElement | null>(null);

  const hasHistory = finished.length > 0;

  const chip = (s: SubagentEntry) => (
    <button
      key={s.agentId}
      type="button"
      role="tab"
      aria-selected={s.agentId === activeId}
      className={`subagent-chip ${s.agentId === activeId ? 'subagent-chip--active' : ''}`}
      title={[s.description, s.agentId].filter(Boolean).join(' · ')}
      onClick={() => onSelect(s.agentId)}
    >
      <span className={`subagent-dot subagent-dot--${s.status}`} />
      <span className="subagent-chip-name">{s.name}</span>
    </button>
  );

  /// 装箱测算：隐藏测量行持全量同构节点（chip/展开器/History 替身），按真实行
  /// clientWidth 逐个累加（gap 同实测）。全部放得下 → 全显无展开器；放不下 →
  /// 预留展开器+History 宽度后取最大前缀（至少露 1 个）。测量源零宽 → 保持现状
  /// （隐藏保活的 pane 里 clientWidth=0，不能把已算好的布局打回兜底）。
  const measure = useCallback(() => {
    if (expanded) return;
    const row = rowRef.current;
    const m = measureRef.current;
    if (row == null || m == null) return;
    const avail = row.clientWidth;
    if (avail <= 0) return;
    const chipEls = [...m.querySelectorAll<HTMLElement>('[data-measure="chip"]')];
    if (chipEls.length === 0 || chipEls.some((el) => el.offsetWidth <= 0)) {
      setFitCount(null); // 测量不可用（happy-dom 等）→ 兜底
      return;
    }
    const style = window.getComputedStyle(row);
    const gap = Number.parseFloat(style.columnGap || style.gap || '0') || 0;
    const widthOf = (sel: string): number =>
      m.querySelector<HTMLElement>(sel)?.offsetWidth ?? 0;
    const histW = hasHistory ? widthOf('[data-measure="history"]') + gap : 0;
    const sumAll = chipEls.reduce((acc, el) => acc + el.offsetWidth + gap, 0);
    if (sumAll + histW <= avail) {
      setFitCount(subagents.length); // 全部放得下：无展开器
      return;
    }
    const expW = widthOf('[data-measure="expand"]') + gap;
    let acc = histW + expW;
    let k = 0;
    for (const el of chipEls) {
      const next = acc + el.offsetWidth + (k > 0 ? gap : 0);
      if (next > avail) break;
      acc = next;
      k += 1;
    }
    setFitCount(Math.max(k, 1));
  }, [expanded, hasHistory, subagents.length]);

  // 布局后测量（条目数/展开态变化即重测）
  useLayoutEffect(() => {
    measure();
  }, [measure]);

  // 容器变宽即时重排（S3）：ResizeObserver 优先，window resize 兜底
  //（happy-dom 无 ResizeObserver；隐藏 pane clientWidth=0 时 measure 早退不打回兜底）
  useEffect(() => {
    const row = rowRef.current;
    if (row == null) return;
    let ro: ResizeObserver | null = null;
    if (typeof ResizeObserver !== 'undefined') {
      ro = new ResizeObserver(() => measure());
      ro.observe(row);
    }
    const onResize = (): void => measure();
    window.addEventListener('resize', onResize);
    return () => {
      ro?.disconnect();
      window.removeEventListener('resize', onResize);
    };
  }, [measure]);

  if (subagents.length === 0) return null; // 无活跃 = 入口不出现（含历史）
  const visibleCount = expanded
    ? subagents.length
    : (fitCount ?? Math.min(FALLBACK_PREVIEW, subagents.length));
  const overflowCount = subagents.length - visibleCount;
  const visible = subagents.slice(0, visibleCount);
  return (
    <div className="task-list-card subagent-card">
      <div className="subagent-summary-text">🤖 Subagent {subagents.length}</div>
      <div
        ref={rowRef}
        className={`subagent-chip-list ${expanded ? 'is-expanded' : 'is-collapsed'}`}
        role="tablist"
        aria-label={t('subagents.aria')}
      >
        {visible.map(chip)}
        {/* S2 溢出展开器：+N（展开后为收起钮）；无溢出不渲染 */}
        {(expanded || overflowCount > 0) && subagents.length > 1 && (
          <button
            type="button"
            className="subagent-toggle"
            aria-expanded={expanded}
            onClick={() => setExpanded(!expanded)}
          >
            {expanded ? 'Collapse' : `+${overflowCount}`}
          </button>
        )}
        {/* 历史入口恒挂活跃列表末尾（与展开态无关——不展开同样可达） */}
        {hasHistory && (
          <button
            type="button"
            className="subagent-toggle subagent-history-toggle"
            aria-expanded={historyOpen}
            onClick={() => setHistoryOpen(!historyOpen)}
          >
            History ({finished.length}) {historyOpen ? '▾' : '▸'}
          </button>
        )}
      </div>
      {/* 历史列表：活跃列表下方独立一行，弱化呈现（已结束条目只在此出现） */}
      {historyOpen && (
        <div className="subagent-chip-list subagent-history-list">{finished.map(chip)}</div>
      )}
      {/* 隐藏测量行（S1/S3 装箱数据源）：全量 chip + 展开器/History 替身，零视觉占位 */}
      <div ref={measureRef} className="subagent-chip-list subagent-measure" aria-hidden="true">
        {subagents.map((s) => (
          <span data-measure="chip" key={s.agentId} className="subagent-chip">
            <span className={`subagent-dot subagent-dot--${s.status}`} />
            <span className="subagent-chip-name">{s.name}</span>
          </span>
        ))}
        {subagents.length > 1 && (
          <button type="button" data-measure="expand" className="subagent-toggle" tabIndex={-1}>
            +{subagents.length}
          </button>
        )}
        {hasHistory && (
          <button
            type="button"
            data-measure="history"
            className="subagent-toggle subagent-history-toggle"
            tabIndex={-1}
          >
            History ({finished.length}) ▸
          </button>
        )}
      </div>
    </div>
  );
}
