import { useState, type ReactElement } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import type { AgentSummary } from '../types';
import ChatTab from './ChatTab';
import ConfirmDialog, { type ConfirmRequest } from './ConfirmDialog';
import './DetailPanel.css';

export default function DetailPanel({
  agent,
  onClose,
}: {
  agent: AgentSummary;
  onClose: () => void;
}): ReactElement {
  const qc = useQueryClient();
  const [tab, setTab] = useState<'overview' | 'chat'>('overview');
  const [logs, setLogs] = useState('');
  const [showLogs, setShowLogs] = useState(false);
  const [err, setErr] = useState('');
  const [confirmReq, setConfirmReq] = useState<ConfirmRequest | null>(null);

  const loadLogs = async (): Promise<void> => {
    try {
      const res = await api.logs(agent.id);
      setLogs(res.logs);
      setShowLogs(true);
    } catch (e) {
      setErr(String(e));
    }
  };

  const stop = (): void => {
    // 误点即停代价高，与 rm 同级加确认（review-r1 BLOCKER-2）；统一走 ConfirmDialog
    setConfirmReq({
      title: '停止会话 · Stop agent',
      message: `即将停止「${agent.name ?? agent.id}」。`,
      banner: 'Stop 后会话退出运行状态，之后可用 Respawn 重启',
      variant: 'danger',
      confirmLabel: 'Stop',
      action: () =>
        api
          .stopAgent(agent.id)
          .then(() => qc.invalidateQueries({ queryKey: ['agents'] }))
          .catch((e: unknown) => setErr(String(e))),
    });
  };

  const respawn = (): void => {
    // respawn 会重启会话，加确认（对齐 stop/rm 语义）
    setConfirmReq({
      title: '重启会话 · Respawn',
      message: `即将重启「${agent.name ?? agent.id}」的会话进程。`,
      banner: 'Respawn 会结束当前会话并以同一 cwd 重新启动，未落盘的上下文会丢失',
      variant: 'warn',
      confirmLabel: 'Respawn',
      action: () =>
        api
          .respawnAgent(agent.id)
          .then(() => qc.invalidateQueries({ queryKey: ['agents'] }))
          .catch((e: unknown) => setErr(String(e))),
    });
  };

  const rm = (): void => {
    setConfirmReq({
      title: '移除会话 · Remove agent',
      message: `即将从列表移除「${agent.name ?? agent.id}」。`,
      banner: 'Remove 后该 agent 停止并被移出管理列表，操作不可撤销',
      variant: 'danger',
      confirmLabel: 'Remove',
      action: () =>
        api
          .removeAgent(agent.id)
          .then(() => {
            qc.invalidateQueries({ queryKey: ['agents'] });
            onClose();
          })
          .catch((e: unknown) => setErr(String(e))),
    });
  };

  return (
    <aside
      className={`detail-panel ${agent.group === 'needs_input' ? 'needs-input' : ''}`}
      aria-label="Agent detail"
    >
      <div className="detail-head">
        <button type="button" className="btn-ghost" onClick={onClose} aria-label="Close detail">
          Close
        </button>
        <div className="detail-tabs" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'overview'}
            onClick={() => setTab('overview')}
          >
            概览
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'chat'}
            onClick={() => setTab('chat')}
            disabled={agent.sessionId == null}
            title={agent.sessionId == null ? '该 agent 无关联会话' : undefined}
          >
            对话
          </button>
        </div>
      </div>

      {tab === 'chat' ? (
        <ChatTab agent={agent} />
      ) : (
        <>
          <dl className="detail-field">
            <dt>State</dt>
            <dd>{agent.rawState ?? '—'}</dd>
            <dt>Detail</dt>
            <dd>{agent.detail ?? '—'}</dd>
            <dt>CWD</dt>
            <dd>{agent.cwd ?? '—'}</dd>
            <dt>Model</dt>
            <dd>default</dd>
            <dt>Started</dt>
            <dd>
              {agent.startedAt ? new Date(agent.startedAt * 1000).toLocaleString() : '—'}
            </dd>
            <dt>Tokens</dt>
            <dd>{agent.tokens != null ? String(agent.tokens) : '—'}</dd>
          </dl>
          {err ? <p className="dialog-error">{err}</p> : null}
          <div className="detail-actions">
            <button type="button" className="btn-ghost" onClick={() => void loadLogs()}>
              Logs
            </button>
            <button type="button" className="btn-danger" onClick={stop}>
              Stop
            </button>
            <button type="button" className="btn-danger" onClick={rm}>
              Remove
            </button>
            <button
              type="button"
              className="btn-primary"
              onClick={respawn}
              disabled={agent.rawState === 'working'}
              title={
                agent.rawState === 'working'
                  ? '运行中的会话无需重启；respawn 面向已完成/退出的会话'
                  : undefined
              }
            >
              Respawn
            </button>
          </div>
          {showLogs ? <pre className="logs-pre">{logs}</pre> : null}
        </>
      )}
      {confirmReq ? <ConfirmDialog request={confirmReq} onClose={() => setConfirmReq(null)} /> : null}
    </aside>
  );
}
