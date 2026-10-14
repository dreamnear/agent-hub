import { useMemo, type ReactElement } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api';
import type { AgentSummary } from '../types';
import { filterAgents } from '../hooks/useAgents';
import { useI18n } from '../i18n';
import DriverBadge from './DriverBadge';
import './HistoryDialog.css';

interface Props {
  /** 选中历史条目（打开 chat 视图；发送即复活回侧栏） */
  onSelect: (a: AgentSummary) => void;
  /** 对话框形态关闭回调；embedded（设置页分区）下由外壳关设置页，可缺省 */
  onClose?: () => void;
  /** 工程白名单过滤（与侧栏同一族锚语义；修正4 双保险，全驱动适用） */
  checked: ReadonlySet<string>;
  anchors: string[];
  /** 取全量清单（含历史）的取数函数；缺省本机 api（测试注入用） */
  fetchAll?: () => Promise<AgentSummary[]>;
  /** embedded（agent-hub-settings-consolidation S2）：设置页分区内嵌形态——去遮罩，列表行为不变（S3） */
  embedded?: boolean;
}

/// 历史会话面板（2026-10-02 全驱动展示纪律）：默认侧栏只列活跃会话，本面板承担
/// 「回去的路」——收编所有驱动的非活跃会话（claude Done + opencode 历史），
/// 合并按最近活动倒序；条目带驱动小标（不写教学文案）。
/// 条目点击即打开 chat 视图（消息分页照常）：
/// - claude Done：send 内化 respawn 链路（drivers/claude/send.rs）→ 复活回侧栏 Working
/// - opencode：send → busy 置位 → 回侧栏 Working
/// 目录先过工程白名单族锚过滤（checked/anchors 由调用方传入），与侧栏同一语义。
/// ponytail ceiling：不做搜索/分页——会话量数千后在此加 cursor 或虚拟滚动。
export default function HistoryDialog({
  onSelect,
  onClose,
  checked,
  anchors,
  fetchAll,
  embedded = false,
}: Props): ReactElement {
  const t = useI18n();
  const list = useQuery({
    queryKey: ['agents-history'],
    queryFn: fetchAll ?? (() => api.listAgents(true)),
    staleTime: 30_000,
  });
  const history = useMemo(() => {
    const done = (list.data ?? []).filter((a) => a.group === 'completed');
    // 白名单族锚过滤与侧栏同一实现（filterAgents）；合并排序最近活动优先
    //（opencode 条目服务端已倒序，claude 混入后统一客户端重排）
    return filterAgents(done, checked, anchors).sort(
      (x, y) => (y.startedAt ?? 0) - (x.startedAt ?? 0),
    );
  }, [list.data, checked, anchors]);

  const pick = (a: AgentSummary): void => {
    onSelect(a);
    onClose?.();
  };

  const card = (
      <div className={`dialog-card history-card${embedded ? ' history-card--embedded' : ''}`}>
        <div className="history-head">
          <h2 className="history-title">{t('history.title')}</h2>
          <span className="history-sub">
            {t('history.latestFirst')} · {t('history.count', { n: history.length })}
          </span>
        </div>
        {list.isLoading ? (
          <div className="history-empty">{t('sidebar.loading')}</div>
        ) : list.isError ? (
          <div className="history-empty">
            {String(list.error)}
            <button type="button" className="btn-ghost" onClick={() => void list.refetch()}>
              {t('history.retry')}
            </button>
          </div>
        ) : history.length === 0 ? (
          <div className="history-empty">{t('history.empty')}</div>
        ) : (
          <ul className="history-list">
            {history.map((a) => (
              <li key={`${a.driver}:${a.id}`}>
                <button type="button" className="history-item" onClick={() => pick(a)}>
                  <span className="history-name">
                    <DriverBadge driver={a.driver} />
                    {a.name ?? a.id.slice(0, 12)}
                  </span>
                  <span className="history-cwd" title={a.cwd ?? ''}>
                    {a.cwd ?? ''}
                  </span>
                  <span className="history-time">{relativeTime(a.startedAt)}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
  );

  if (embedded) return card;
  return (
    <div
      className="dialog-backdrop confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t('history.title')}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose?.();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') onClose?.();
      }}
    >
      {card}
    </div>
  );
}

/// 相对时间（AgentRow.relativeTime 同口径，独立小实现避免耦合行组件）。
function relativeTime(startedAt: number | null): string {
  if (startedAt == null) return '';
  const diff = Date.now() / 1000 - startedAt;
  if (diff < 60) return '<1m';
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}
