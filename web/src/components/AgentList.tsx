import { useState, type ReactElement } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useAgents, filterAgents, familyAnchors, type InstanceCtx, type GroupedAgents } from '../hooks/useAgents';
import { useTheme } from '../hooks/useTheme';
import { t, useI18n } from '../i18n';
import type { AgentSummary } from '../types';
import AgentRow, { type SessionAction } from './AgentRow';
import ConfirmDialog, { type ConfirmRequest } from './ConfirmDialog';
import type { SettingsSectionKey } from './SettingsPage';
import './AgentList.css';

interface Props {
  checked: ReadonlySet<string>;
  /** 清空工程过滤勾选（反馈轮 24-A 提示条「全部显示」用） */
  onClearFilter: () => void;
  onSelect: (a: AgentSummary) => void;
  selectedId: string | null;
  onOpenSearch: () => void;
  onCollapse: () => void;
  onOpenProjects: () => void;
  onOpenConfig: () => void;
  onStart: () => void;
  /** 打开设置页指定分区（agent-hub-settings B1/B3：底部「设置」项 + 顶栏主题标记跳转） */
  onOpenSettings: (section: SettingsSectionKey) => void;
  /** 移除会话后回抛被移除 agent 的 id（App 按复合键清选中） */
  onRemoved: (id: string) => void;
  /** 工程便签（agent-hub-notes）：⋯ 菜单入口上抛 App（悬浮卡 + 钉住自动展开归 App 管） */
  onOpenNotes: (a: AgentSummary) => void;
}

/// 组桶标签（C2 i18n：label 存 key，渲染期翻译——buildInstanceGroups 输出随 locale）
export const GROUP_LABELS: { key: string; labelKey: string }[] = [
  { key: 'working', labelKey: 'group.working' },
  { key: 'other', labelKey: 'group.other' },
  { key: 'needsInput', labelKey: 'group.needsInput' },
  { key: 'completed', labelKey: 'group.completed' },
];

/// 单个实例分组头可折叠状态（任务6）：键 = `${instanceId}:${groupKey}`，实例间互不影响。
export function groupKeyOf(instanceId: string | null, key: string): string {
  return `${instanceId ?? ''}:${key}`;
}

/// 按实例 + 组桶渲染的分组（任务6：侧栏分组头（本机/实例名）+ 组内既有状态桶）。
/// 各桶先按 instanceId 分流，再套既有语义：活跃桶（working/needs_input）永不被工程过滤
/// 隐藏；空闲/已完成受本机族过滤管（仅本机套族锚——远程 cwd 不落本机族锚整组保留）。
export function buildInstanceGroups(
  instances: InstanceCtx[],
  grouped: GroupedAgents,
  checked: ReadonlySet<string>,
  anchors: string[],
): { instId: string | null; instName: string; groups: { key: string; label: string; agents: AgentSummary[] }[] }[] {
  const ACTIVE_KEYS: ReadonlySet<string> = new Set(['working', 'needsInput']);
  return instances.map((inst) => {
    // 未标 instanceId（undefined）视同本机（null）——向后兼容现状数据
    const byInst = (bucket: AgentSummary[]) => bucket.filter((a) => (a.instanceId ?? null) === inst.id);
    const remoteIds = inst.id != null ? new Set([inst.id]) : new Set<string | null>();
    const groups = GROUP_LABELS.map(({ key, labelKey }) => {
      const bucket = grouped[key as keyof GroupedAgents] as AgentSummary[];
      return {
        key,
        label: t(labelKey),
        agents: ACTIVE_KEYS.has(key)
          ? byInst(bucket)
          : filterAgents(byInst(bucket), checked, anchors, remoteIds),
      };
    });
    return { instId: inst.id, instName: inst.name, groups };
  });
}

/// 左侧栏（Sidebar-v3，反馈轮 18，Penpot Sidebar-v3 板）：顶部品牌条 + 可折叠状态
/// 分组（▾工作中 ▸等待 ▾空闲 ▸已完成，工作中/空闲默认展开，等待/已完成折叠只显
/// 组头，点击组头切换；空组隐藏）+ 行状态圆点（agent-dot--group，2026-09-23 回归
/// 恢复）+ 底部功能菜单。多实例（任务6）：组头再套实例分组头（本机/实例名 + 计数 +
/// 离线标记），折叠状态按实例隔离。
export default function AgentList({
  checked,
  onClearFilter,
  onSelect,
  selectedId,
  onOpenSearch,
  onCollapse,
  onOpenProjects,
  onOpenConfig,
  onStart,
  onOpenSettings,
  onRemoved,
  onOpenNotes,
}: Props): ReactElement {
  const { grouped, isLoading, tree, instances, offline, authError, registry } = useAgents();
  const queryClient = useQueryClient();
  const t = useI18n(); // 屏蔽模块级 t：组件内随 locale 订阅重渲（C2）
  // 主题三态（反馈轮 26-A）：◐ 跟随系统 / ☾ 暗色 / ☀︎ 亮色。agent-hub-settings B3
  // 起唯一切换入口在设置页外观分区，此处只作状态标记（点击跳设置页）
  const theme = useTheme();
  // 工程族锚（反馈轮 23）：各树组主仓根路径，驱动 filterAgents 的 common-root 直查
  const anchors = familyAnchors(tree);
  const registryFor = (instanceId: string | null) => registry.apiFor(instanceId);
  // 行 ⋯ 会话操作统一确认弹窗（复用 ConfirmDialog）：危险操作 Confirm/Stop/Remove 走确认，
  // Logs/中断 直接执行。interactive 会话在 AgentRow 内已隐藏不可用项。
  const [confirmReq, setConfirmReq] = useState<ConfirmRequest | null>(null);
  // Sidebar-v3 折叠态（设计稿默认：工作中/空闲展开，等待/已完成折叠只显组头）
  // 多实例（任务6）：折叠键带实例维（`${instanceId}:${groupKey}`）——实例间互不影响。
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  const isCollapsed = (instanceId: string | null, key: string): boolean => {
    const v = collapsed[groupKeyOf(instanceId, key)];
    // 设计稿默认：工作中/空闲展开，等待/已完成折叠（多实例各实例同语义）
    return v != null ? v : key === 'needsInput' || key === 'completed';
  };

  const toggleCollapse = (instanceId: string | null, key: string): void => {
    const k = groupKeyOf(instanceId, key);
    setCollapsed((c) => ({ ...c, [k]: !(c[k] ?? (key === 'needsInput' || key === 'completed')) }));
  };

  const handleAction = (action: SessionAction, a: AgentSummary): void => {
    // 远程实例会话操作按实例路由（任务7 前置）：logs/中断/stop/respawn/remove 走
    // per-instance api —— ChatTab 已按 instanceId 路由播放；此处侧栏操作同样分流。
    const instApi = registryFor(a.instanceId ?? null);
    if (action === 'logs') {
      instApi
        .logs(a.id)
        .then((r) => alert(r.logs.slice(-2000)))
        .catch((e: unknown) => alert(String(e)));
      return;
    }
    if (action === 'notes') {
      onOpenNotes(a);
      return;
    }
    if (action === 'interrupt') {
      if (a.driver === 'acp') instApi.cancelAcpSession(a.id).catch((e: unknown) => alert(String(e)));
      else instApi.interruptAgent(a.id).catch((e: unknown) => alert(String(e)));
      return;
    }
    if (action === 'stop') {
      if (a.driver === 'acp') {
        instApi.cancelAcpSession(a.id).catch((e: unknown) => alert(String(e)));
        return;
      }
      setConfirmReq({
        title: t('confirm.stop.title'),
        message: t('confirm.stop.message', { name: a.name ?? a.id }),
        banner: t('confirm.stop.banner'),
        variant: 'danger',
        confirmLabel: t('confirm.stop.label'),
        action: () => instApi.stopAgent(a.id).catch((e: unknown) => alert(String(e))),
      });
      return;
    }
    if (action === 'respawn') {
      setConfirmReq({
        title: t('confirm.respawn.title'),
        message: t('confirm.respawn.message', { name: a.name ?? a.id }),
        banner: t('confirm.respawn.banner'),
        variant: 'warn',
        confirmLabel: t('confirm.respawn.label'),
        action: () => instApi.respawnAgent(a.id).catch((e: unknown) => alert(String(e))),
      });
      return;
    }
    // remove
    setConfirmReq({
      title: t('confirm.remove.title'),
      message: t('confirm.remove.message', { name: a.name ?? a.id }),
      banner: t('confirm.remove.banner'),
      variant: 'danger',
      confirmLabel: t('confirm.remove.label'),
      action: () =>
        instApi
          .removeAgent(a.id)
          .then(() => {
            queryClient.invalidateQueries({ queryKey: ['agents'] });
            onRemoved(a.id);
          })
          .catch((e: unknown) => alert(String(e))),
    });
  };

  if (isLoading) return <div role="status">{t('sidebar.loading')}</div>;

  // 多实例分组（任务6）：按实例分组头（本机/实例名）→ 组内沿用既有状态桶。
  // 过滤语义：活跃桶恒全显；空闲/已完成 —— 本机套族锚，远程整组保留（远程 cwd
  // 不落本机族锚是预期）。折叠态按实例隔离（collapsed[`${instId}:${groupKey}`]）。
  const instGroups = buildInstanceGroups(instances, grouped, checked, anchors);
  // 被工程过滤隐藏的会话数（仅受过滤桶统计）：逐实例逐桶聚合
  let hiddenCount = 0;
  for (const inst of instGroups) {
    for (const g of inst.groups) {
      hiddenCount += Math.max(
        (grouped[g.key as keyof GroupedAgents]?.filter(
          (a) => (a.instanceId ?? null) === inst.instId,
        ).length ?? 0) - g.agents.length,
        0,
      );
    }
  }

  return (
    <div className="agent-list">
      <div className="sb-topbar">
        <span className="sb-brand">agent-hub</span>
        <button
          type="button"
          className="sb-icon-btn"
          aria-label={t('sidebar.themeAria', {
            mode: theme.mode === 'auto' ? t('settings.theme.auto') : theme.mode === 'dark' ? t('settings.theme.dark') : t('settings.theme.light'),
          })}
          title={t('sidebar.themeTitle', {
            mode: theme.mode === 'auto' ? t('settings.theme.auto') : theme.mode === 'dark' ? t('settings.theme.dark') : t('settings.theme.light'),
          })}
          onClick={() => onOpenSettings('appearance')}
        >
          {theme.mode === 'auto' ? '◐' : theme.mode === 'dark' ? '☾' : '☀︎'}
        </button>
        <button
          type="button"
          className="sb-icon-btn"
          aria-label={t('sidebar.search')}
          title={t('sidebar.searchTitle')}
          onClick={onOpenSearch}
        >
          <svg width="15" height="15" viewBox="0 0 15 15" aria-hidden="true">
            <circle cx="6.5" cy="6.5" r="4.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
            <line x1="10" y1="10" x2="13.5" y2="13.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
          </svg>
        </button>
        <button
          type="button"
          className="sb-icon-btn"
          aria-label={t('sidebar.collapse')}
          title={t('sidebar.collapseTitle')}
          onClick={onCollapse}
        >
          <svg width="15" height="15" viewBox="0 0 15 15" aria-hidden="true">
            <rect x="3" y="2.5" width="3" height="10" rx="1" fill="currentColor" />
            <rect x="9" y="2.5" width="3" height="10" rx="1" fill="currentColor" opacity="0.45" />
          </svg>
        </button>
      </div>
      <div className="sb-list">
        {instGroups.every((inst) => inst.groups.every((g) => g.agents.length === 0)) &&
        !instGroups.some((inst) => offline[inst.instId ?? ''] || authError[inst.instId ?? '']) ? (
          <p className="sb-empty">{t('sidebar.empty')}</p>
        ) : (
          instGroups.map((inst) => {
            const visible = inst.groups.filter((g) => g.agents.length > 0);
            const instOffline = !!offline[inst.instId ?? ''];
            // 凭据失效优先展示（WS 会被 401 拒掉，离线/凭据失效可同时为真——
            // 展示语义上「token 失效」是更精确的降级原因，任务10）
            const instAuthErr = !!authError[inst.instId ?? ''];
            // 离线/凭据失效实例即使零会话也渲染组头（降级态可见，不静默消失）
            if (visible.length === 0 && !instOffline && !instAuthErr) return null;
            return (
              <div className="inst-group" key={inst.instId ?? 'local'}>
                <div className={`inst-group-head ${instOffline && !instAuthErr ? 'inst-group-head--offline' : ''}`}>
                  <span className="inst-group-name">
                    {inst.instId == null ? t('inst.local') : inst.instName}
                    {instOffline && !instAuthErr ? <span className="inst-group-offline">{t('sidebar.offline')}</span> : null}
                    {instAuthErr ? (
                      <button
                        type="button"
                        className="inst-group-autherr"
                        title={t('sidebar.authErrorTitle')}
                        onClick={() => onOpenSettings('instances')}
                      >
                        {t('sidebar.authError')}
                      </button>
                    ) : null}
                  </span>
                  <span className="inst-group-count">
                    {visible.reduce((n, g) => n + g.agents.length, 0)}
                  </span>
                </div>
                {visible.map(({ key, label, agents }) => (
                  <div className="agent-group" key={key}>
                    <button
                      type="button"
                      className={`agent-group-head ${instOffline ? 'agent-group-head--offline' : ''}`}
                      aria-expanded={!isCollapsed(inst.instId, key)}
                      onClick={() => toggleCollapse(inst.instId, key)}
                    >
                      <span className="agent-group-caret" aria-hidden="true">
                        {isCollapsed(inst.instId, key) ? '▸' : '▾'}
                      </span>
                      {label}
                      <span className="agent-group-count">{agents.length}</span>
                    </button>
                    {!isCollapsed(inst.instId, key)
                      ? agents.map((agent) => (
                          <AgentRow
                            key={`${agent.instanceId ?? ''}:${agent.driver}:${agent.id}`}
                            agent={agent}
                            onSelect={onSelect}
                            onAction={handleAction}
                            selected={agent.id === selectedId}
                          />
                        ))
                      : null}
                  </div>
                ))}
              </div>
            );
          })
        )}
      </div>
      {/* 过滤隐藏提示条（反馈轮 24-A，Penpot filter-hidden-hint）：仅在有会话被
          工程过滤隐藏时显示，点击清空勾选回到全显 */}
      {hiddenCount > 0 ? (
        <button type="button" className="filter-hidden-hint" onClick={onClearFilter}>
          {t('sidebar.hiddenHint', { n: hiddenCount })}
        </button>
      ) : null}
      <div className="sb-menu">
        <button type="button" className="sb-menu-item" onClick={onOpenProjects}>
          <span className="sb-menu-icon" aria-hidden="true">▤</span>
          {t('menu.projects')}
        </button>
        <button type="button" className="sb-menu-item" onClick={onOpenConfig}>
          <span className="sb-menu-icon" aria-hidden="true">⚙</span>
          {t('menu.config')}
        </button>
        {/* 实例管理入口（agent-hub-settings D1）：统一指向设置页实例分区，不再有独立弹层 */}
        <button
          type="button"
          className="sb-menu-item"
          onClick={() => onOpenSettings('instances')}
        >
          <span className="sb-menu-icon" aria-hidden="true">◫</span>
          {t('menu.instances')}
        </button>
        <button type="button" className="sb-menu-item" onClick={onStart}>
          <span className="sb-menu-icon" aria-hidden="true">＋</span>
          {t('menu.newAgent')}
        </button>
        {/* 设置页入口（agent-hub-settings B1）：默认落外观分区 */}
        <button
          type="button"
          className="sb-menu-item"
          onClick={() => onOpenSettings('appearance')}
        >
          <span className="sb-menu-icon" aria-hidden="true">⚒</span>
          {t('menu.settings')}
        </button>
      </div>
      {confirmReq ? <ConfirmDialog request={confirmReq} onClose={() => setConfirmReq(null)} /> : null}
    </div>
  );
}
