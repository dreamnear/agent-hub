import type { ReactElement } from 'react';
import './DriverBadge.css';

/** 驱动 → monogram 字母 + token 类（新驱动接入 = 加一条映射）。 */
const DRIVER_META: Record<string, { letter: string; token: string }> = {
  claude: { letter: 'C', token: 'claude' },
  opencode: { letter: 'O', token: 'opencode' },
  omp: { letter: 'P', token: 'omp' },
  dsh: { letter: 'D', token: 'dsh' }, // 注册表预留（omp 驱动未接入）
};

interface Props {
  driver: string;
  /** small=侧栏行/聊天头部（16px）；面板默认同 small（视觉规格统一 17px 盒） */
  size?: 'small';
  /** R8：interactive 前台会话标识——claude 显示双字母 IC（区别 bg 的 C） */
  interactive?: boolean;
}

/// 驱动 monogram 徽标：圆角小方块单字母，底/字色走 --c-driver-* 成对 token
///（亮暗两态 ≥4.5:1）。未知驱动不渲染（不虚构）。
export default function DriverBadge({ driver, size = 'small', interactive }: Props): ReactElement | null {
  const meta = DRIVER_META[driver];
  if (meta == null) return null;
  const letter = interactive && driver === 'claude' ? 'IC' : meta.letter;
  return (
    <span
      className={`driver-badge driver-badge--${meta.token} driver-badge--${size}${letter.length > 1 ? ' driver-badge--wide' : ''}`}
      aria-hidden="true"
    >
      {letter}
    </span>
  );
}
