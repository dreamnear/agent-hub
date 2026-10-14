import { useMemo, useState, type ReactElement } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { Api } from '../api';
import type { AgentSummary, CliSessionEntry } from '../types';
import { useI18n } from '../i18n';
import './SessionHistoryDialog.css';

interface Props {
  /** 入口 agent 行：cwd 定工程、id 供端点寻址、instanceId 定实例 */
  agent: AgentSummary;
  /** 活跃清单（App 轮询 data）：sessionId 命中 → 运行中徽标 + 恢复走复用 */
  agents: AgentSummary[];
  /** per-instance api（远程实例会话照常按实例路由） */
  api: Api;
  /** 恢复成功回抛（active = 既有实例条目；resumed = 快照，id 已切新 bg 条目） */
  onSelect: (a: AgentSummary) => void;
  onClose: () => void;
}

/// 相对时间（HistoryDialog 同口径：<1m / Xm / Xh / Xd，独立小实现避免耦合）。
function relativeTime(mtimeSecs: number): string {
  const diff = Date.now() / 1000 - mtimeSecs;
  if (diff < 60) return '<1m';
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

/// 人读体积（CLI 选择器口径：B/KB/MB）。
function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

/// cwd 尾段（worktree 会话的辨识名；AgentRow.cwdTail 同款思路取最后一截）。
function pathTail(p: string): string {
  const parts = p.split('/').filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/// CLI 历史会话弹窗（agent-hub-session-history）：某 agent cwd 对应工程的
/// `~/.claude/projects/<slug>/*.jsonl` 只读枚举 + `--bg --resume` 恢复。
/// 与全局 HistoryDialog 并存不合并——那边是「hub 已知非活跃会话」（hub 注册表），
/// 这边是「CLI 侧工程会话全量」（含 hub 从未接管的会话），数据源不同。
/// 行内口径对齐 CLI `--resume` 选择器：相对时间 · 分支 · 体积 · worktree · 摘要。
export default function SessionHistoryDialog({
  agent,
  agents,
  api,
  onSelect,
  onClose,
}: Props): ReactElement {
  const t = useI18n();
  const [filter, setFilter] = useState('');
  const [resuming, setResuming] = useState<string | null>(null);
  const [resumeError, setResumeError] = useState<string | null>(null);
  const list = useQuery({
    queryKey: ['cli-sessions', agent.instanceId ?? null, agent.id],
    queryFn: () => api.cliSessions(agent.id, agent.driver),
    staleTime: 5_000,
    retry: false,
  });

  // 运行中映射：活跃清单 sessionId → agent 条目（徽标 + 点击复用，防 CLI copy 副本）
  const activeBySid = useMemo(() => {
    const m = new Map<string, AgentSummary>();
    for (const a of agents) {
      if (a.sessionId) m.set(a.sessionId, a);
    }
    return m;
  }, [agents]);

  // 输入即过滤（对齐 CLI 的 Type to search）：摘要 / 分支 / 会话 id
  const filtered = useMemo(() => {
    const rows = list.data ?? [];
    const q = filter.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) =>
      [r.summary, r.branch, r.sessionId].some((s) => s?.toLowerCase().includes(q)),
    );
  }, [list.data, filter]);

  const resume = async (entry: CliSessionEntry): Promise<void> => {
    if (resuming != null) return;
    setResuming(entry.sessionId);
    setResumeError(null);
    try {
      const r = await api.resumeSession(agent.id, entry.sessionId, agent.driver);
      if (r.status === 'active' && r.agent) {
        // 已有实例在跑：直接选中，不产生重复实例（功能点 6）
        onSelect(r.agent);
      } else if (r.status === 'resumed' && r.id) {
        // 快照兜底（App historySel 机制）：新 bg 条目尚未进轮询清单时仍可选中；
        // session_id = 原 CLI 会话 → 消息视图按既有分页端点呈现历史转录
        onSelect({ ...agent, id: r.id, sessionId: entry.sessionId, group: 'other' });
      }
      // current：目标即当前会话，无需动作
      onClose();
    } catch (e) {
      setResumeError(e instanceof Error ? e.message : String(e));
    } finally {
      setResuming(null);
    }
  };

  return (
    <div
      className="dialog-backdrop confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t('sesshist.title')}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') onClose();
      }}
    >
      <div className="dialog-card sesshist-card">
        <div className="history-head">
          <h2 className="history-title">{t('sesshist.title')}</h2>
          <span className="history-sub">
            {t('sesshist.subtitle')} · {t('history.count', { n: filtered.length })}
          </span>
        </div>
        <input
          type="text"
          className="sesshist-search"
          aria-label={t('sesshist.searchLabel')}
          placeholder={t('sesshist.search')}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        {list.isLoading ? (
          <div className="history-empty">{t('sidebar.loading')}</div>
        ) : list.isError ? (
          <div className="history-empty">
            {String(list.error)}
            <button type="button" className="btn-ghost" onClick={() => void list.refetch()}>
              {t('history.retry')}
            </button>
          </div>
        ) : (list.data ?? []).length === 0 ? (
          <div className="history-empty">{t('sesshist.empty')}</div>
        ) : filtered.length === 0 ? (
          <div className="history-empty">{t('sesshist.filterEmpty')}</div>
        ) : (
          <ul className="sesshist-list">
            {filtered.map((r) => {
              const running = activeBySid.has(r.sessionId);
              const isCurrent = agent.sessionId === r.sessionId;
              // worktree 会话：转录 cwd 与工程 cwd 不同 → 显示尾段辨识名
              const wt = r.cwd && agent.cwd && r.cwd !== agent.cwd ? pathTail(r.cwd) : null;
              return (
                <li key={r.sessionId}>
                  <button
                    type="button"
                    className="sesshist-item"
                    disabled={resuming != null}
                    onClick={() => void resume(r)}
                  >
                    <span className="sesshist-line1">
                      <span className="sesshist-summary" title={r.summary ?? r.sessionId}>
                        {r.summary ?? r.sessionId.slice(0, 12)}
                      </span>
                      <span className="sesshist-time">{relativeTime(r.mtimeSecs)}</span>
                    </span>
                    <span className="sesshist-line2">
                      {r.branch ? <span className="sesshist-branch">{r.branch}</span> : null}
                      <span className="sesshist-size">{fmtSize(r.sizeBytes)}</span>
                      {wt ? <span className="sesshist-wt" title={r.cwd ?? ''}>{wt}</span> : null}
                      {resuming === r.sessionId ? (
                        <span className="sesshist-badge">{t('sesshist.resuming')}</span>
                      ) : null}
                      {isCurrent ? (
                        <span className="sesshist-badge sesshist-badge--current">
                          {t('sesshist.current')}
                        </span>
                      ) : running ? (
                        <span className="sesshist-badge sesshist-badge--running">
                          {t('sesshist.running')}
                        </span>
                      ) : null}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        {resumeError ? (
          <p className="dialog-error" role="alert">
            {t('sesshist.resumeFailed')}：{resumeError}
          </p>
        ) : null}
      </div>
    </div>
  );
}
