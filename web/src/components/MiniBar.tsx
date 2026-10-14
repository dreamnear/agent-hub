import type { ReactElement } from 'react';
import { useI18n } from '../i18n';
import './MiniBar.css';


interface Props {
  /** 开左侧会话抽屉 */
  onSessions: () => void;
  /** 开右抽屉并滚到 Git 槽 */
  onGit: () => void;
  /** 开文件面板（agent-hub-files-panel F4） */
  onFiles: () => void;
}

/// 手机迷你底栏（≤768px，agent-hub 工单）：会话/Git/文件三直达。桌面 ≥769px 用
/// 全功能 BottomBar，本组件由 App 按 !desktop 条件挂载（CSS 兜底双保险）。
export default function MiniBar({ onSessions, onGit, onFiles }: Props): ReactElement {
  const t = useI18n();
  return (
    <div className="mini-bar" aria-label={t('bar.label')}>
      <button type="button" className="mini-bar-btn" onClick={onSessions}>
        {t('bar.sessions')}
      </button>
      <button type="button" className="mini-bar-btn" onClick={onGit}>
        {t('bar.git')}
      </button>
      <button type="button" className="mini-bar-btn" onClick={onFiles}>
        {t('bar.files')}
      </button>
    </div>
  );
}
