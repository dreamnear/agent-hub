import { useRef, useState, type ReactElement } from 'react';
import type { AgentSummary } from '../types';
import './AgentRow.css';

/// 相对时间（行 title 提示用）。
function relativeTime(startedAt: number | null): string {
  if (startedAt == null) return '';
  const diff = Date.now() / 1000 - startedAt;
  if (diff < 60) return '刚刚';
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  return `${Math.floor(diff / 86400)} 天前`;
}

/// cwd 尾段作为工程标识（…/dec/.worktree/dec-staging → …/.worktree/dec-staging）。
function cwdTail(cwd: string | null): string {
  if (!cwd) return '';
  const parts = cwd.split('/').filter(Boolean);
  return `…/${parts.slice(-2).join('/')}`;
}

/// 单行会话行（只显示标题 13px 单行省略；名字前 8px 状态圆点按 group 着色——
/// 2026-09-23 回归恢复（f000b73 Sidebar-v2 曾按设计稿移除）；选中态 = 提亮底；
/// 右侧 ⋯ 弹浮层菜单——会话操作 + 删除会话，浮层 fixed 定位防被滚动裁剪）。
/// 会话级操作（中断/Stop/Respawn/Remove/Logs/工程便签）由父级 AgentList 统一接线（含确认弹窗），
/// 本组件只发射动作；interactive 会话无中断/Stop/Respawn/Remove（非 bg 任务）。
export type SessionAction = 'interrupt' | 'stop' | 'respawn' | 'remove' | 'logs' | 'notes';

export default function AgentRow({
  agent,
  onSelect,
  onAction,
  selected = false,
}: {
  agent: AgentSummary;
  onSelect: (a: AgentSummary) => void;
  onAction: (action: SessionAction, a: AgentSummary) => void;
  selected?: boolean;
}): ReactElement {
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuPos, setMenuPos] = useState<{ top: number; left: number } | null>(null);
  const menuBtnRef = useRef<HTMLButtonElement | null>(null);
  const name = agent.name ?? agent.id.slice(0, 8);
  const detail = agent.detail ?? agent.rawState ?? '';
  const hint = [detail, cwdTail(agent.cwd), relativeTime(agent.startedAt)]
    .filter(Boolean)
    .join(' · ');
  const interactive = agent.kind === 'interactive';
  // ocr-review 中：Logs/Stop/Respawn/删除是 claude 专属端点（/api/agents/claude/...），
  // ACP 行隐藏（中断在 AgentList.handleAction 分流 cancelAcpSession）
  const isClaude = agent.driver !== 'acp';

  const run = (action: SessionAction): void => {
    setMenuOpen(false);
    onAction(action, agent);
  };

  const toggleMenu = (): void => {
    if (menuOpen) {
      setMenuOpen(false);
      return;
    }
    const r = menuBtnRef.current?.getBoundingClientRect();
    if (r) {
      // 贴视口右/下缘内收，防溢出（菜单 6 项 ≈ 200px 高）
      setMenuPos({
        top: Math.min(r.bottom + 4, window.innerHeight - 210),
        left: Math.max(4, Math.min(r.left, window.innerWidth - 160)),
      });
    }
    setMenuOpen(true);
  };

  return (
    <div className={`agent-row-wrap ${selected ? 'agent-row-wrap--sel' : ''}`}>
      <button
        type="button"
        className="agent-row"
        onClick={() => onSelect(agent)}
        title={hint ? `${name}\n${hint}` : name}
      >
        <span className={`agent-dot agent-dot--${agent.group}`} aria-hidden="true" />
        <span className="agent-name">{name}</span>
      </button>
      <button
        type="button"
        ref={menuBtnRef}
        className="agent-row-menu-btn"
        aria-label={`${name} 更多操作`}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={toggleMenu}
      >
        ⋯
      </button>
      {menuOpen ? (
        <>
          <div className="agent-row-menu-backdrop" onClick={() => setMenuOpen(false)} />
          <div className="agent-row-menu" role="menu" style={menuPos ?? undefined}>
            {/* 非危险操作：Logs（claude 专属）/ 工程便签（agent-hub-notes）/ 中断 */}
            {isClaude ? (
              <button type="button" role="menuitem" onClick={() => run('logs')}>
                Logs
              </button>
            ) : null}
            <button type="button" role="menuitem" onClick={() => run('notes')}>
              工程便签
            </button>
            {!interactive ? (
              <button type="button" role="menuitem" onClick={() => run('interrupt')}>
                中断
              </button>
            ) : null}
            {/* 分组分隔 + 危险操作底部（Stop/Respawn/Remove 全桌聚焦改动避误触；claude 专属） */}
            {!interactive && isClaude ? <div className="agent-row-menu-sep" role="separator" /> : null}
            {!interactive && isClaude ? (
              <button
                type="button"
                role="menuitem"
                className="agent-row-menu-danger"
                onClick={() => run('stop')}
              >
                Stop
              </button>
            ) : null}
            {!interactive && isClaude ? (
              <button
                type="button"
                role="menuitem"
                className="agent-row-menu-respawn"
                onClick={() => run('respawn')}
              >
                Respawn
              </button>
            ) : null}
            {!interactive && isClaude ? (
              <button type="button" role="menuitem" className="agent-row-menu-danger" onClick={() => run('remove')}>
                删除会话
              </button>
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
