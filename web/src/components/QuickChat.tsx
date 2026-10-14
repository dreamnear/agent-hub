import { useEffect, useMemo, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import type { AgentSummary } from '../types';
import './QuickChat.css';

interface Props {
  agents: AgentSummary[];
  onPick: (a: AgentSummary) => void;
  onClose: () => void;
}

/// ⌘K 快速选择器（P5 第 9 项行为统一）：搜索 + 键盘导航选中 agent，
/// 选中即关闭面板并切换主区域到该会话对话（与左栏点选行为一致）。
/// P3 的浮层内直发表单已废弃（对话入口统一为主区域）。
export default function QuickChat({ agents, onPick, onClose }: Props): ReactElement {
  const t = useI18n();
  const [query, setQuery] = useState('');
  const [activeIdx, setActiveIdx] = useState(0);

  const candidates = useMemo(() => {
    const q = query.trim().toLowerCase();
    return agents
      .filter(
        (a) =>
          !q ||
          a.id.toLowerCase().includes(q) ||
          (a.name ?? '').toLowerCase().includes(q) ||
          (a.cwd ?? '').toLowerCase().includes(q),
      )
      .slice(0, 8);
  }, [agents, query]);

  useEffect(() => {
    setActiveIdx(0);
  }, [query]);

  const pick = (a: AgentSummary): void => {
    onPick(a);
    onClose();
  };

  return (
    <div
      className="quickchat-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="Quick agent switcher"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') onClose();
        if (e.key === 'k' && (e.metaKey || e.ctrlKey)) onClose();
      }}
    >
      <div className="quickchat-card">
        {/* Beautiful UI 批4 SearchList 输入行形态：搜索 icon + input + border-b */}
        <div className="quickchat-input-row">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--ink-3)" strokeWidth="2" strokeLinecap="round" className="quickchat-search-icon shrink-0" aria-hidden="true">
            <circle cx="11" cy="11" r="7" />
            <path d="M21 21l-4.3-4.3" />
          </svg>
          <input
            className="quickchat-search"
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown') {
                e.preventDefault();
                setActiveIdx((i) => Math.min(i + 1, candidates.length - 1));
              } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                setActiveIdx((i) => Math.max(i - 1, 0));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                const a = candidates[activeIdx];
                if (a) pick(a);
              }
            }}
            placeholder={t('qc.placeholder')}
            aria-label="search agent"
          />
        </div>
        <ul className="quickchat-list" role="listbox" aria-label="agents">
          {candidates.map((a, i) => (
            <li key={a.id}>
              <button
                type="button"
                role="option"
                aria-selected={i === activeIdx}
                className={i === activeIdx ? 'qc-opt qc-opt--sel' : 'qc-opt'}
                onClick={() => pick(a)}
                onMouseEnter={() => setActiveIdx(i)}
              >
                <span className={`agent-dot agent-dot--${a.group}`} aria-hidden="true" />
                <span className="qc-name">{a.name ?? a.id.slice(0, 8)}</span>
                <span className="qc-cwd">{a.cwd ?? ''}</span>
                <span className="qc-state">{a.rawState ?? ''}</span>
                {i === activeIdx ? <span className="qc-enter">{t('qc.open')}</span> : null}
              </button>
            </li>
          ))}
          {candidates.length === 0 ? (
            <li className="qc-empty">
              {/* Beautiful UI SearchList empty state 形态：圆底图标 + 标题 + 提示 */}
              <span className="qc-empty-icon" aria-hidden="true">
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
                  <circle cx="11" cy="11" r="7" />
                  <path d="M21 21l-4.3-4.3" />
                </svg>
              </span>
              <span className="qc-empty-title">{t('qc.emptyTitle')}</span>
              <span className="qc-empty-hint">{t('qc.emptyHint')}</span>
            </li>
          ) : null}
        </ul>
        <div className="quickchat-footer">{t('qc.footer')}</div>
      </div>
    </div>
  );
}
