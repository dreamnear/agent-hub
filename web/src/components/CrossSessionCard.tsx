import type { ReactElement } from 'react';
import { useI18n } from '../i18n';
import type { CrossSessionMessage } from '../lib/parseCrossSessionMessage';
import MarkdownView from './MarkdownView';
import './CrossSessionCard.css';

/// 跨会话消息卡片：默认单行紧凑展示（对齐终端注入样式
/// `> Message from @source: <peek> (click to expand)`），
/// 点开走 details 展开完整 markdown。
export default function CrossSessionCard({
  msg,
  when,
}: {
  msg: CrossSessionMessage;
  when: string;
}): ReactElement {
  const t = useI18n();
  const source = msg.fromName ?? msg.from?.split('/').pop() ?? msg.from ?? 'agent';
  const peek =
    msg.body
      .split('\n')
      .map((l) => l.trim())
      .find((l) => l) ?? '';

  return (
    <details className="cross-card">
      <summary className="cross-head">
        <span className="cross-arrow">›</span>
        <span className="cross-label">
          Message from <span className="cross-from">@{source}</span>:
        </span>
        <span className="cross-peek" title={msg.body}>
          {peek}
        </span>
        <span className="cross-hint">{t('cross.expandHint', { when })}</span>
      </summary>
      <div className="cross-body">
        <MarkdownView text={msg.body} />
      </div>
      {msg.disclaimer ? (
        <details className="cross-disclaimer">
          <summary>{t('cross.disclaimerSummary')}</summary>
          <p>{msg.disclaimer}</p>
        </details>
      ) : null}
    </details>
  );
}
