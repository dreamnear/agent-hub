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
  /** 选中回抛（原子切换：三态都回抛目标 agent，由 App 切选中 + 主区跟随）；
   * info.sessionName = 该条会话的可读名（toast 文案用），
   * info.spawned = 是否新起了 bg 条目（true 时 App 立即失效 agents 查询，
   * 让新条目尽快进轮询清单——否则窗口内主区顶着快照名、侧栏无高亮） */
  onSelect: (a: AgentSummary, info: { sessionName: string; spawned: boolean }) => void;
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

/// 无摘要时的可辨识回退（R3 用户口径：实在取不到可读文本时给可辨识形式）：
/// 「日期 · 分支」（无分支则纯日期）。不用 sessionId 前缀——不可辨识。
function fallbackName(r: CliSessionEntry): string {
  const d = new Date(r.mtimeSecs * 1000);
  const date = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(
    d.getDate(),
  ).padStart(2, '0')}`;
  return r.branch ? `${date} · ${r.branch}` : date;
}

/// 条目可读名（行内展示与切换 toast 同源，防两处口径漂移）。
function displayName(r: CliSessionEntry): string {
  return r.summary ?? fallbackName(r);
}

/// 历史会话弹窗（agent-hub-session-history → agent-hub-acp-session-history）：
/// 某 agent cwd 对应工程的历史会话只读枚举 + 恢复。按驱动路由数据源（服务端
/// 同端点插拔）：claude → `~/.claude/projects/<slug>/*.jsonl`（`--bg --resume`
/// 恢复）；omp → sessions 目录扫描（ACP `session/load` 恢复，快照切 acp 驱动）。
/// 与全局 HistoryDialog 并存不合并——那边是「hub 已知非活跃会话」（hub 注册表），
/// 这边是「CLI 侧工程会话全量」（含 hub 从未接管的会话），数据源不同。
/// 行内口径对齐 CLI resume 选择器：相对时间 · 分支 · 体积 · worktree · 摘要
/// （omp 无分支/worktree 字段，如实留空）。
export default function SessionHistoryDialog({
  agent,
  agents,
  api,
  onSelect,
  onClose,
}: Props): ReactElement {
  const t = useI18n();
  // 弹窗标题带 agent 名（交互模型 5）：非选中行操作时有明确预期——「这是谁的
  // 历史会话」。缺名回退 id 前 8 位（AgentRow 同款口径）
  const agentName = agent.name ?? agent.id.slice(0, 8);
  const [filter, setFilter] = useState('');
  const [resuming, setResuming] = useState<string | null>(null);
  const [resumeError, setResumeError] = useState<string | null>(null);
  // omp 生态行型归一（R1 tester FAIL-1）：omp 文件行（driver='omp'）与 hub-spawn
  // 的 ACP 行（driver='acp' + name=acp agent 名）取数/恢复都按 'omp' 端点寻址
  // （ACP 行 driver 恒 'acp'，按原样寻址会打 /api/agents/acp/… → 400）
  const isOmpAcp = agent.driver === 'acp' && agent.name === 'omp';
  const endpointDriver = agent.driver === 'omp' || isOmpAcp ? 'omp' : agent.driver;
  const list = useQuery({
    queryKey: ['cli-sessions', agent.instanceId ?? null, agent.id],
    queryFn: () => api.cliSessions(agent.id, endpointDriver),
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
      const r = await api.resumeSession(agent.id, entry.sessionId, endpointDriver);
      // 三态统一收敛为「目标 agent」（agent-hub-pick-switch #18 原子切换）：
      // 服务端三态只回答「后端要不要动作」，不回答「前端视图要不要切」——它按
      // **入口行 agent** 比对 session_id，对主视图当前选中的是谁一无所知。把
      // «无需后端动作» 当成 «无需切换» 正是本单根因：非选中行开弹窗、选中该行
      // 自己的当前会话 → 服务端 current → 前端不回调 → 主区与侧栏纹丝不动。
      // 故此处判据一律落在前端：三种情形都必须回抛目标 agent，由 App 原子切换。
      const driver = endpointDriver === 'omp' ? 'acp' : agent.driver;
      const target: AgentSummary | null =
        r.status === 'current'
          ? // 该条 = 入口行 agent 自己的当前会话：后端无需动作，前端切到该行 agent
            agent
          : r.status === 'active' && r.agent
            ? // 已有实例在跑：复用该实例条目，不产生重复实例（功能点 3/6）。
              // 服务端返回的条目不带实例维（instanceId 是前端聚合层打标），
              // 远程实例上按实例路由必须由入口行的实例补上，否则选中后消息/
              // 发送会落到本机 api（跨实例取错数据）
              { ...r.agent, instanceId: r.agent.instanceId ?? agent.instanceId ?? null }
            : r.status === 'resumed' && r.id
              ? // 快照兜底（App historySel 机制）：新 bg 条目尚未进轮询清单时仍可选中。
                // claude：session_id = 原 CLI 会话 → 消息视图按既有分页端点呈现历史转录；
                // omp（两行型）：恢复 = ACP spawn + session/load，会话归 acp 驱动显示
                // （omp 文件清单排除注册表活会话），快照 driver 置 acp → 消息/发送路由
                // ACP 端点
                { ...agent, id: r.id, sessionId: entry.sessionId, driver, group: 'other' }
              : null;
      // 响应形状意外（三态皆不成立）当作失败：不静默关弹窗，错误可见（交互模型 2）
      if (target == null) throw new Error(t('sesshist.resumeFailed'));
      onSelect(target, {
        sessionName: displayName(entry),
        spawned: r.status === 'resumed',
      });
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
      aria-label={t('sesshist.titleNamed', { name: agentName })}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') onClose();
      }}
    >
      <div className="dialog-card sesshist-card">
        <div className="history-head">
          <h2 className="history-title">{t('sesshist.titleNamed', { name: agentName })}</h2>
          <span className="history-sub">
            {t('sesshist.subtitle')}
            {/* 计数仅取数成功后显示（reviewer MINOR 1）：错误/加载态挂 0 是陈旧值 */}
            {!list.isLoading && !list.isError
              ? ` · ${t('history.count', { n: filtered.length })}`
              : ''}
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
              // 名称：清洗后的可读摘要；无可读文本 → 「日期 · 分支」可辨识回退
              const name = displayName(r);
              return (
                <li key={r.sessionId}>
                  <button
                    type="button"
                    className="sesshist-item"
                    disabled={resuming != null}
                    onClick={() => void resume(r)}
                  >
                    <span className="sesshist-line1">
                      <span className="sesshist-summary" title={name}>
                        {name}
                      </span>
                      <span className="sesshist-time">{relativeTime(r.mtimeSecs)}</span>
                    </span>
                    <span className="sesshist-line2">
                      {/* 左组：分支 · worktree 名（R3 用户口径：行内容左对齐） */}
                      <span className="sesshist-l2-left">
                        {r.branch ? <span className="sesshist-branch">{r.branch}</span> : null}
                        {wt ? (
                          <span className="sesshist-wt" title={r.cwd ?? ''}>
                            {wt}
                          </span>
                        ) : null}
                      </span>
                      {/* 右组：状态徽标 + 体积（R3：大小向右对齐） */}
                      <span className="sesshist-l2-right">
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
                        <span className="sesshist-size">{fmtSize(r.sizeBytes)}</span>
                      </span>
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
