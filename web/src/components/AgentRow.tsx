import { useLayoutEffect, useRef, useState, type ReactElement } from 'react';
import { createPortal } from 'react-dom';
import { t } from '../i18n';
import DriverBadge from './DriverBadge';
import { useEscapeKey } from '../hooks/useEscapeKey';
import type { AgentSummary } from '../types';
import './AgentRow.css';

/// 相对时间（行 title 提示用）。
function relativeTime(startedAt: number | null): string {
  if (startedAt == null) return '';
  const diff = Date.now() / 1000 - startedAt;
  if (diff < 60) return t('row.justNow');
  if (diff < 3600) return t('row.minAgo', { n: Math.floor(diff / 60) });
  if (diff < 86400) return t('row.hourAgo', { n: Math.floor(diff / 3600) });
  return t('row.dayAgo', { n: Math.floor(diff / 86400) });
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
/// 会话级操作（中断/Stop/Respawn/Remove/Logs/工程便签/从列表移除）由父级 AgentList 统一接线（含确认弹窗），
/// 本组件只发射动作；interactive 会话无中断/Stop/Respawn/Remove（非 bg 任务），
/// 保留「从列表移除」（R8：仅隐藏条目，不动底层进程）。
export type SessionAction =
  | 'interrupt'
  | 'stop'
  | 'respawn'
  | 'remove'
  | 'logs'
  | 'notes'
  | 'sessions'
  | 'unlist';

export default function AgentRow({
  agent,
  onSelect,
  onAction,
  selected = false,
  nodeSuffix,
  muted = false,
}: {
  agent: AgentSummary;
  onSelect: (a: AgentSummary) => void;
  onAction: (action: SessionAction, a: AgentSummary) => void;
  selected?: boolean;
  /// agent-list-status-grouping G3：会话名节点后缀（「会话名 · 节点名」，
  /// 多实例视图统一形态；单本机省略）——区分同主会话
  nodeSuffix?: string;
  /// G4 离线不回归：所属实例离线 → 行置灰（原实例组头置灰语义下沉到行）
  muted?: boolean;
}): ReactElement {
  const [menuOpen, setMenuOpen] = useState(false);
  /// 打开瞬间的按钮锚点（翻转计算基准）+ 菜单初始粗定位（精确摆位交给
  /// useLayoutEffect 实测菜单尺寸后修正）
  const [menuAnchor, setMenuAnchor] = useState<{
    top: number;
    left: number;
    btnBottom: number;
    btnRight: number;
  } | null>(null);
  const [menuPos, setMenuPos] = useState<{ top: number; left: number } | null>(null);
  const menuBtnRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const name = agent.name ?? agent.id.slice(0, 8);
  const detail = agent.detail ?? agent.rawState ?? '';
  const hint = [detail, cwdTail(agent.cwd), relativeTime(agent.startedAt)]
    .filter(Boolean)
    .join(' · ');
  const interactive = agent.kind === 'interactive';
  // ocr-review 中：Logs/Stop/Respawn 是 claude 专属端点（/api/agents/claude/...），
  // ACP 行隐藏（中断在 AgentList.handleAction 分流 cancelAcpSession）
  const isClaude = agent.driver !== 'acp';
  const isClaudeDriver = agent.driver === 'claude';
  // #45 移除通道：acp 注册表行补「移除」项（/api/agents/acp/{id}/rm 已实装，
  // 删除注册表条目 + 同步清 acp_sessions.json 持久化）——重启复活的占位行用户
  // 可自行清掉。omp 文件行仍无此项（只读事实源，绝不删用户磁盘会话）
  const canRemove = !interactive && (isClaude || agent.driver === 'acp');
  // R4 追加（tester 观察 1 查证）：所有非 claude 驱动的 AgentCatalog::logs 均返回
  // unsupported（pi 1078/opencode 401/dsh 863/omp 627/acp 584 各处）——Logs 项只有
  // claude 行有意义
  const hasLogs = isClaudeDriver;
  // 历史会话入口（agent-hub-acp-session-history，用户拍板开放范围 = claude + omp）：
  // claude → CLI 转录目录；omp → sessions 目录 + session/load 恢复。omp 生态有两种
  // 行型——omp 文件扫描行（driver='omp'）与 hub-spawn 的 ACP 行（driver='acp'、
  // name=acp agent 名）：注册表活会话被 omp 清单排除，常规路径「hub 开 omp 会话 →
  // 恢复历史」入口落在 ACP 行上，须同放（R1 tester FAIL-1）。其余 ACP agent
  // （元数据薄弱）不开放——不出现点了信息残缺或必失败的入口
  const isOmpAcp = agent.driver === 'acp' && agent.name === 'omp';
  const hasSessions = isClaudeDriver || agent.driver === 'omp' || isOmpAcp;

  const run = (action: SessionAction): void => {
    setMenuOpen(false);
    onAction(action, agent);
  };

  // #20：菜单 Esc 统一关闭（document 级监听，焦点在 body 也生效）——关闭即
  // 连带卸载透明 backdrop，Esc 后的下一次点击落到下层真实元素
  useEscapeKey(menuOpen, () => setMenuOpen(false));

  const toggleMenu = (): void => {
    if (menuOpen) {
      setMenuOpen(false);
      return;
    }
    const r = menuBtnRef.current?.getBoundingClientRect();
    if (r) {
      // 粗定位：按钮下方 4px、左缘对齐按钮（精确翻转/贴边由 useLayoutEffect
      // 按菜单实测尺寸修正——菜单 7 项高约 260px，写死预算会随项数漂移）
      setMenuAnchor({ top: r.bottom + 4, left: r.left, btnBottom: r.bottom, btnRight: r.right });
      setMenuPos({
        top: Math.min(r.bottom + 4, window.innerHeight - 260),
        left: Math.max(8, Math.min(r.left, window.innerWidth - 176)),
      });
    }
    setMenuOpen(true);
  };

  /// R4 菜单可见性（用户截图：菜单被侧栏右缘裁成细竖条）：侧栏三层 overflow
  /// 祖先（wrap/sb-list/pane-left）+ WebKit 对 overflow 容器内 fixed 的处理缺陷，
  /// 都能让 fixed 菜单被裁或错位——根修 = Portal 到 body（脱离全部祖先上下文）
  /// + 渲染后实测菜单尺寸做边缘翻转（右缘超限向左翻、底部超限向上翻）。
  /// useLayoutEffect 在 paint 前完成摆位，首帧无跳动。
  useLayoutEffect(() => {
    if (!menuOpen || !menuRef.current || !menuAnchor) return;
    const mw = menuRef.current.offsetWidth;
    const mh = menuRef.current.offsetHeight;
    let { top, left } = menuPos ?? { top: menuAnchor.top, left: menuAnchor.left };
    // 贴视口右缘 → 向左翻转：菜单右缘对齐按钮右缘
    if (left + mw > window.innerWidth - 8) {
      left = Math.max(8, menuAnchor.btnRight - mw);
    }
    // 贴视口底部 → 向上翻转：菜单底缘在按钮顶缘上方；翻转后仍越界（极端矮视口）
    // → 贴视口下缘 8px 兜底，保证 bottom 恒不出视口（tester R4 观察项 2）
    if (top + mh > window.innerHeight - 8) {
      top = Math.max(8, Math.min(menuAnchor.btnBottom - mh - 4, window.innerHeight - mh - 8));
    }
    setMenuPos((cur) => (cur && cur.top === top && cur.left === left ? cur : { top, left }));
  }, [menuOpen, menuAnchor, menuPos]);

  return (
    <div className={`agent-row-wrap${selected ? ' agent-row-wrap--sel' : ''}${muted ? ' agent-row--muted' : ''}`}>
      <button
        type="button"
        className="agent-row"
        onClick={() => onSelect(agent)}
        title={hint ? `${name}\n${hint}` : name}
      >
        <span className={`agent-dot agent-dot--${agent.group}`} aria-hidden="true" />
        <DriverBadge driver={agent.driver} interactive={interactive} />
        <span className="agent-name">{name}</span>
        {nodeSuffix ? <span className="agent-node-suffix">· {nodeSuffix}</span> : null}
      </button>
      <button
        type="button"
        ref={menuBtnRef}
        className="agent-row-menu-btn"
        aria-label={t('row.moreActions', { name })}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={toggleMenu}
      >
        ⋯
      </button>
      {menuOpen
        ? createPortal(
            <>
              <div className="agent-row-menu-backdrop" onClick={() => setMenuOpen(false)} />
              <div className="agent-row-menu" ref={menuRef} role="menu" style={menuPos ?? undefined}>
                {/* 非危险操作：Logs（claude 专属，其余驱动 logs 端点均 unsupported）/
                    历史会话（agent-hub-acp-session-history，claude + omp 生态两行型开放）/
                    工程便签（agent-hub-notes）/ 中断 */}
                {hasLogs ? (
                  <button type="button" role="menuitem" onClick={() => run('logs')}>
                    Logs
                  </button>
                ) : null}
                {hasSessions ? (
                  <button type="button" role="menuitem" onClick={() => run('sessions')}>
                    {t('row.sessions')}
                  </button>
                ) : null}
                <button type="button" role="menuitem" onClick={() => run('notes')}>
                  {t('row.notes')}
                </button>
                {!interactive ? (
                  <button type="button" role="menuitem" onClick={() => run('interrupt')}>
                    {t('row.interrupt')}
                  </button>
                ) : null}
                {/* 分组分隔 + 危险操作底部（Stop/Respawn/Remove 全桌聚焦改动避误触；Stop/
                    Respawn claude 专属，Remove claude + acp（#45）；interactive 的 unlist
                    同属危险区，故 claude 行恒有分隔线） */}
                {isClaude || canRemove ? <div className="agent-row-menu-sep" role="separator" /> : null}
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
                {canRemove ? (
                  <button type="button" role="menuitem" className="agent-row-menu-danger" onClick={() => run('remove')}>
                    {t('row.remove')}
                  </button>
                ) : null}
                {/* R8：interactive 行的「从列表移除」——仅隐藏条目不动进程（Stop/
                    Respawn 等无进程句柄维持隐藏，不放假按钮） */}
                {interactive && isClaude ? (
                  <button type="button" role="menuitem" className="agent-row-menu-danger" onClick={() => run('unlist')}>
                    {t('row.unlist')}
                  </button>
                ) : null}
              </div>
            </>,
            document.body,
          )
        : null}
    </div>
  );
}
