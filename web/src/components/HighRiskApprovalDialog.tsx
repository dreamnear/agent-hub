import { useCallback, useEffect, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import {
  plugins,
  type ApprovalRequest,
  type PluginView,
} from '../plugins';
import './HighRiskApprovalDialog.css';

/// 高危审批确认卡（B2 批3，ADR-0007 第 3 条 / 设计 §6.5 HighRiskConfirmDialog）：
/// 监听 hub 审批单表（4s 轮询——插件为 hub 本地概念，与实例 WS 无关），有待确认
/// 单时弹卡。拒绝在左、批准在右（设计规格），固定留痕说明（BR-15：双侧均入
/// 故障记录域）；工具描述来自插件（BR-14，非 hub 审定），确认与否不受其影响。
export default function HighRiskApprovalDialog(): ReactElement | null {
  const t = useI18n();
  const [pending, setPending] = useState<ApprovalRequest[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const refresh = useCallback((): void => {
    plugins
      .approvals()
      .then((r) => setPending(r.pending))
      .catch(() => setPending([]));
    plugins
      .list()
      .then((list: PluginView[]) => {
        const m: Record<string, string> = {};
        for (const p of list) m[p.id] = p.name;
        setNames(m);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, 4_000);
    return () => clearInterval(timer);
  }, [refresh]);

  if (pending.length === 0) return null;
  const req = pending[0];

  const decide = (decision: 'approve' | 'reject'): void => {
    setBusy(true);
    plugins
      .decideApproval(req.id, decision)
      .catch(() => {})
      .finally(() => {
        setBusy(false);
        refresh();
      });
  };

  return (
    <div
      className="dialog-backdrop apr-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t('apr.title')}
    >
      <div className="dialog-card apr-card">
        <h2 className="apr-title">{t('apr.title')}</h2>
        <p className="apr-desc">{t('apr.desc')}</p>
        <dl className="apr-facts">
          <div className="apr-fact">
            <dt>{t('apr.plugin')}</dt>
            <dd>{names[req.pluginId] ?? req.pluginId}</dd>
          </div>
          <div className="apr-fact">
            <dt>{t('apr.tool')}</dt>
            <dd className="apr-mono">{req.tool}</dd>
          </div>
          <div className="apr-fact">
            <dt>{t('apr.args')}</dt>
            <dd className="apr-mono apr-args">{req.argsDigest || '{}'}</dd>
          </div>
        </dl>
        <div className="confirm-banner confirm-banner--danger" role="note">
          ⚠ {t('apr.banner')}
        </div>
        <p className="apr-trace">{t('apr.trace')}</p>
        <p className="apr-untrusted">{t('apr.untrusted')}</p>
        <div className="dialog-actions">
          {/* 拒绝在左、批准在右（设计规格 HighRiskConfirmDialog） */}
          <button type="button" className="btn-ghost" onClick={() => decide('reject')} disabled={busy}>
            {t('apr.reject')}
          </button>
          <button type="button" className="btn-danger" onClick={() => decide('approve')} disabled={busy}>
            {t('apr.approve')}
          </button>
        </div>
      </div>
    </div>
  );
}
