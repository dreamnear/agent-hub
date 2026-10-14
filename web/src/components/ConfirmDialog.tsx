import { useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import './ConfirmDialog.css';

/// 确认请求描述（回调式：调用方 setConfirmReq({...})，确认后执行 action）。
export interface ConfirmRequest {
  title: string;
  message: string;
  banner: string;
  variant: 'danger' | 'warn';
  confirmLabel: string;
  action: () => unknown;
}

/// 统一确认弹窗（Penpot 规格第三节第 5 条）：危险型红色确认钮 + 红警示条；
/// 提醒型绿色确认钮 + 橙警示条。替换全部原生 confirm()。
export default function ConfirmDialog({
  request,
  onClose,
}: {
  request: ConfirmRequest;
  onClose: () => void;
}): ReactElement {
  const [busy, setBusy] = useState(false);
  const t = useI18n();
  const close = (): void => {
    if (!busy) onClose();
  };
  const run = (): void => {
    setBusy(true);
    Promise.resolve(request.action()).finally(() => {
      setBusy(false);
      onClose();
    });
  };

  return (
    <div
      className="dialog-backdrop confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={request.title}
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') close();
      }}
    >
      <div className="dialog-card confirm-card">
        <h2 className="confirm-title">{request.title}</h2>
        <p className="confirm-desc">{request.message}</p>
        <div className={`confirm-banner confirm-banner--${request.variant}`} role="note">
          ⚠ {request.banner}
        </div>
        <div className="dialog-actions">
          <button type="button" className="btn-ghost" onClick={close} disabled={busy}>
            {t('dialog.cancel')}
          </button>
          <button
            type="button"
            className={request.variant === 'danger' ? 'btn-danger' : 'btn-primary'}
            onClick={run}
            disabled={busy}
          >
            {request.confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
