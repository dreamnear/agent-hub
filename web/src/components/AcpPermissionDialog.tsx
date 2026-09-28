import { useState, type ReactElement } from 'react';
import { api as localApi, type Api } from '../api';
import { useI18n } from '../i18n';
import type { ChatMessage } from '../types';
import './AcpPermissionDialog.css';

interface PermOption {
  optionId: string;
  name?: string;
  kind?: string;
}

/// ACP 权限请求弹卡（批3 任务11）：呈现 agent 权限请求与选项，用户点选后回传
/// allow/reject；取消/关闭 = cancelled。红线：任何路径不默认放行——全部选项均需
/// 显式点击，关闭即取消。api 按 agent 所属实例路由（任务7 跨实例权限弹卡）。
export default function AcpPermissionDialog({
  agentId,
  request,
  onClose,
  api = localApi,
}: {
  agentId: string;
  request: ChatMessage;
  api?: Api;
  onClose: () => void;
}): ReactElement {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');
  const t = useI18n();
  const options: PermOption[] = Array.isArray(
    (request.input as { options?: PermOption[] } | null)?.options,
  )
    ? (request.input as { options: PermOption[] }).options
    : [];
  const title = request.toolName ?? t('perm.fallbackTitle');

  const answer = async (optionId: string | null): Promise<void> => {
    if (busy || request.toolUseId == null) return;
    setBusy(true);
    setErr('');
    try {
      await api.answerAcpPermission(agentId, request.toolUseId, optionId);
      onClose();
    } catch (e) {
      setErr(String(e));
      setBusy(false);
    }
  };

  return (
    <div
      className="dialog-backdrop confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t('perm.aria', { title })}
      onClick={(e) => {
        if (e.target === e.currentTarget) void answer(null);
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') void answer(null);
      }}
    >
      <div className="dialog-card confirm-card acp-perm-card">
        <h2 className="confirm-title">{t('perm.title')}</h2>
        <p className="confirm-desc">{title}</p>
        <div className="confirm-banner confirm-banner--warn" role="note">
          {t('perm.banner')}
        </div>
        <div className="dialog-actions acp-perm-actions">
          {options.map((o) => (
            <button
              key={o.optionId}
              type="button"
              disabled={busy}
              className={
                o.kind?.startsWith('reject')
                  ? 'btn-danger'
                  : o.kind?.startsWith('allow')
                    ? 'btn-primary'
                    : 'btn-ghost'
              }
              onClick={() => void answer(o.optionId)}
            >
              {o.name ?? o.optionId}
            </button>
          ))}
          <button type="button" className="btn-ghost" disabled={busy} onClick={() => void answer(null)}>
            {t('dialog.cancel')}
          </button>
        </div>
        {err ? <p className="dialog-error">{err}</p> : null}
      </div>
    </div>
  );
}
