// 节点管理区（gateway-node-p3 M5）：配对列表 / 待审批卡片 / 改名 / token 轮换 /
// 撤销——P1 审批 API + P3 rotate/rename 端点的 UI 面。颜色全走 --c-* token 体系
//（Tailwind 类经 bui/theme.css 桥接），三态主题自动成立。
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, type ReactElement } from 'react';
import { api, RotateRolledBackError } from '../../api';
import type { NodeView } from '../../types';
import { useI18n } from '../../i18n';

/// host stats presence 的一行摘要（os/arch · version · uptime 人性化）；
/// null = 旧版本节点未上报（N-1 窗口如实缺失）。
function statsLine(stats: NodeView['stats']): string {
  if (!stats) return '';
  const days = Math.floor(stats.uptime_secs / 86400);
  const hours = Math.floor((stats.uptime_secs % 86400) / 3600);
  const uptime = days > 0 ? `${days}d ${hours}h` : `${hours}h`;
  return `${stats.os}/${stats.arch} · v${stats.version} · up ${uptime}`;
}

function UptimeCell({ node }: { node: NodeView }): ReactElement {
  return (
    <span className="text-xs text-text/60" data-testid={`node-stats-${node.nodeId}`}>
      {node.stats ? statsLine(node.stats) : '—'}
    </span>
  );
}

export default function NodesSection(): ReactElement {
  const t = useI18n();
  const qc = useQueryClient();
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState('');
  const [enrollCmd, setEnrollCmd] = useState<string | null>(null);
  // sweep S1：操作结果提示（轮换回滚等）——单条内联，下一次操作即清除
  const [notice, setNotice] = useState<{ kind: 'error'; text: string } | null>(null);

  const { data, isLoading } = useQuery({
    queryKey: ['nodes'],
    queryFn: api.listNodes,
    refetchInterval: 15_000,
  });

  const invalidate = (): void => {
    void qc.invalidateQueries({ queryKey: ['nodes'] });
    // approve/rotate/reject 都会改变实例表（挂载/改名/卸载）
    void qc.invalidateQueries({ queryKey: ['instances'] });
  };

  const act = async (run: () => Promise<unknown>): Promise<void> => {
    try {
      await run();
    } finally {
      invalidate();
    }
  };

  // sweep S1：轮换失败已回滚 → 专用文案；其余错误透传服务端详情
  const rotate = async (nodeId: string): Promise<void> => {
    setNotice(null);
    try {
      await act(() => api.rotateNodeToken(nodeId));
    } catch (e: unknown) {
      setNotice({
        kind: 'error',
        text:
          e instanceof RotateRolledBackError
            ? t('nodes.rotateRolledBack')
            : e instanceof Error
              ? e.message
              : String(e),
      });
    }
  };

  const copyEnroll = async (): Promise<void> => {
    try {
      const res = await fetch('/api/nodes/enroll-code', { method: 'POST' });
      const body = (await res.json()) as { command?: string };
      if (body.command) setEnrollCmd(body.command);
    } catch {
      setEnrollCmd(null);
    }
  };

  const nodeCard = (node: NodeView, pending: boolean): ReactElement => (
    <div
      key={node.nodeId}
      data-testid={`node-card-${node.nodeId}`}
      className="rounded-lg border border-line bg-panel p-3 flex flex-col gap-2"
    >
      <div className="flex items-center justify-between gap-2">
        {renaming === node.nodeId ? (
          <form
            className="flex gap-1 items-center"
            onSubmit={(e) => {
              e.preventDefault();
              const name = renameDraft.trim();
              if (name) void act(() => api.renameNode(node.nodeId, name));
              setRenaming(null);
            }}
          >
            <input
              autoFocus
              className="bg-bg border border-line rounded px-2 py-0.5 text-sm text-text"
              value={renameDraft}
              onChange={(e) => setRenameDraft(e.target.value)}
              data-testid={`rename-input-${node.nodeId}`}
            />
            <button
              type="submit"
              className="text-sm px-2 py-0.5 rounded bg-accent/15 text-accent hover:bg-accent/25"
            >
              {t('nodes.save')}
            </button>
          </form>
        ) : (
          <span className="text-sm font-medium text-text">
            {node.nodeName}
            {pending && (
              <span className="ml-2 text-xs px-1.5 py-0.5 rounded bg-warn/15 text-warn">
                {t('nodes.pending')}
              </span>
            )}
          </span>
        )}
        <span className="text-xs text-text/50 truncate max-w-[45%]" title={node.nodeId}>
          {node.nodeId}
        </span>
      </div>
      <div className="flex flex-col gap-0.5 text-xs text-text/70">
        <span>
          {node.platform} · {node.drivers.join(', ')}
        </span>
        <UptimeCell node={node} />
      </div>
      <div className="flex gap-1.5 flex-wrap">
        {pending ? (
          <>
            <button
              type="button"
              className="text-sm px-2.5 py-1 rounded bg-accent/15 text-accent hover:bg-accent/25"
              onClick={() => void act(() => api.approveNode(node.nodeId))}
              data-testid={`approve-${node.nodeId}`}
            >
              {t('nodes.approve')}
            </button>
            <button
              type="button"
              className="text-sm px-2.5 py-1 rounded bg-danger/15 text-danger hover:bg-danger/25"
              onClick={() => void act(() => api.rejectNode(node.nodeId))}
              data-testid={`reject-${node.nodeId}`}
            >
              {t('nodes.reject')}
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              className="text-sm px-2.5 py-1 rounded bg-accent/10 text-accent hover:bg-accent/20"
              data-testid={`rename-${node.nodeId}`}
              onClick={() => {
                setRenaming(node.nodeId);
                setRenameDraft(node.nodeName);
              }}
            >
              {t('nodes.rename')}
            </button>
            <button
              type="button"
              className="text-sm px-2.5 py-1 rounded bg-accent/10 text-accent hover:bg-accent/20"
              onClick={() => void rotate(node.nodeId)}
              data-testid={`rotate-${node.nodeId}`}
              title={t('nodes.rotateHint')}
            >
              {t('nodes.rotate')}
            </button>
            <button
              type="button"
              className="text-sm px-2.5 py-1 rounded bg-danger/10 text-danger hover:bg-danger/20"
              onClick={() => void act(() => api.rejectNode(node.nodeId))}
              data-testid={`revoke-${node.nodeId}`}
            >
              {t('nodes.revoke')}
            </button>
          </>
        )}
      </div>
    </div>
  );

  return (
    <div className="flex flex-col gap-4" data-testid="nodes-section">
      <p className="text-xs text-text/60">{t('nodes.hint')}</p>
      <button
        type="button"
        className="self-start text-sm px-3 py-1.5 rounded bg-accent/15 text-accent hover:bg-accent/25"
        onClick={() => void copyEnroll()}
        data-testid="enroll-code"
      >
        {t('nodes.enrollCode')}
      </button>
      {enrollCmd && (
        <pre
          className="text-xs bg-bg border border-line rounded p-2 whitespace-pre-wrap break-all text-text"
          data-testid="enroll-command"
        >
          {enrollCmd}
        </pre>
      )}
      {isLoading && <p className="text-sm text-text/60">{t('nodes.loading')}</p>}
      {notice && (
        <p
          className="text-sm px-2.5 py-1.5 rounded bg-danger/10 text-danger"
          data-testid="nodes-notice"
          role="alert"
        >
          {notice.text}
        </p>
      )}
      {data && (
        <>
          {data.pending.length > 0 && (
            <section className="flex flex-col gap-2">
              <h3 className="text-sm font-semibold text-text">{t('nodes.pendingList')}</h3>
              <div className="flex flex-col gap-2">{data.pending.map((n) => nodeCard(n, true))}</div>
            </section>
          )}
          <section className="flex flex-col gap-2">
            <h3 className="text-sm font-semibold text-text">{t('nodes.approvedList')}</h3>
            {data.approved.length === 0 ? (
              <p className="text-sm text-text/50">{t('nodes.none')}</p>
            ) : (
              <div className="flex flex-col gap-2">
                {data.approved.map((n) => nodeCard(n, false))}
              </div>
            )}
          </section>
        </>
      )}
    </div>
  );
}
