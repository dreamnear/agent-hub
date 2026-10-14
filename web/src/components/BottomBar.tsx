import { useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import './BottomBar.css';

/// 底部工具栏（Zed 式面板显隐 toggle）：左列三个面板开关（会话/工程/Git），
/// 右侧被动状态区（会话/实例计数，取自 useAgents 现有数据，零新增请求）。
/// ≤768px 不渲染（移动端维持 ☰ 抽屉交互）；显隐状态持久化 hub_panels（App 持有）。

/// 持久化键（跟随 hub_* 键模式）：App 初始化 loadPanelVis 读，变更后写回。
export const PANELS_KEY = 'hub_panels';

export interface PanelVis {
  sessions: boolean;
  projects: boolean;
  git: boolean;
  /** agent-hub-files-panel F4：文件组开关（右面板 files 树 + 主区内容） */
  files: boolean;
}

export type PanelKey = keyof PanelVis;

const DEFAULT_VIS: PanelVis = { sessions: true, projects: true, git: true, files: true };

/** 读持久化的面板显隐：坏 JSON/缺项/类型不符逐项回退默认（全显）。 */
export function loadPanelVis(): PanelVis {
  let raw: string | null = null;
  try {
    raw = typeof window === 'undefined' ? null : window.localStorage.getItem(PANELS_KEY);
  } catch {
    // 存储不可用（隐私模式等）：按默认
  }
  if (raw == null) return { ...DEFAULT_VIS };
  try {
    const v = JSON.parse(raw) as Partial<PanelVis>;
    return {
      sessions: typeof v.sessions === 'boolean' ? v.sessions : DEFAULT_VIS.sessions,
      projects: typeof v.projects === 'boolean' ? v.projects : DEFAULT_VIS.projects,
      git: typeof v.git === 'boolean' ? v.git : DEFAULT_VIS.git,
      files: typeof v.files === 'boolean' ? v.files : DEFAULT_VIS.files,
    };
  } catch {
    return { ...DEFAULT_VIS };
  }
}

interface Props {
  vis: PanelVis;
  onToggle: (k: PanelKey) => void;
  sessionCount: number;
  instanceCount: number;
}

/** 会话侧栏图标（feather sidebar 风格，stroke 随 currentColor） */
function IconSessions(): ReactElement {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <line x1="9" y1="3" x2="9" y2="21" />
    </svg>
  );
}

/** 工程面板图标（feather 右栏风格） */
function IconProjects(): ReactElement {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <line x1="15" y1="3" x2="15" y2="21" />
    </svg>
  );
}

/** Git 分支图标（feather git-branch） */
function IconGit(): ReactElement {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <line x1="6" y1="3" x2="6" y2="15" />
      <circle cx="18" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M18 9a9 9 0 0 1-9 9" />
    </svg>
  );
}

/** 文件图标（feather file，agent-hub-files-panel F4） */
function IconFiles(): ReactElement {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="15 2 15 8 21 8" />
    </svg>
  );
}

export default function BottomBar({
  vis,
  onToggle,
  sessionCount,
  instanceCount,
}: Props): ReactElement | null {
  const t = useI18n();
  // ≤768px 不渲染（挂载期判定；活体拉伸的显隐兜底由 CSS media query 承担）
  const [mobile] = useState(
    () => typeof window !== 'undefined' && window.matchMedia('(max-width: 768px)').matches,
  );
  if (mobile) return null;

  const items: { key: PanelKey; label: string; icon: ReactElement }[] = [
    { key: 'sessions', label: t('bar.sessions'), icon: <IconSessions /> },
    { key: 'projects', label: t('bar.projects'), icon: <IconProjects /> },
    // Git 独立竖栏：与工程栏互不联动（工程隐藏时 Git 仍可单独显示未选中占位）
    { key: 'git', label: t('bar.git'), icon: <IconGit /> },
    // 文件组开关（agent-hub-files-panel F4）：右面板文件树 + 主区内容渲染
    { key: 'files', label: t('bar.files'), icon: <IconFiles /> },
  ];

  return (
    <footer className="bottom-bar" aria-label={t('bar.label')}>
      {/* 左右分组（用户预览反馈）：左=会话开关，右=右面板三开关贴右边；行为/持久化不变 */}
      <div className="bottom-bar-toggles">
        {items
          .filter((it) => it.key === 'sessions')
          .map((it) => (
            <button
              key={it.key}
              type="button"
              className={`bottom-bar-toggle ${vis[it.key] ? 'is-on' : ''}`}
              aria-pressed={vis[it.key]}
              title={it.label}
              onClick={() => onToggle(it.key)}
            >
              {it.icon}
              <span>{it.label}</span>
            </button>
          ))}
      </div>
      <div className="bottom-bar-status">
        {t('bar.status', { sessions: sessionCount, instances: instanceCount })}
      </div>
      <div className="bottom-bar-toggles bottom-bar-toggles--right">
        {items
          .filter((it) => it.key !== 'sessions')
          .map((it) => (
            <button
              key={it.key}
              type="button"
              className={`bottom-bar-toggle ${vis[it.key] ? 'is-on' : ''}`}
              aria-pressed={vis[it.key]}
              title={it.label}
              onClick={() => onToggle(it.key)}
            >
              {it.icon}
              <span>{it.label}</span>
            </button>
          ))}
      </div>
    </footer>
  );
}
