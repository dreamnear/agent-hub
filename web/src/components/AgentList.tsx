import { useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
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
  /** 打开设置页（agent-hub-settings-consolidation S1：底部唯一入口，缺省落上次
      访问分区；顶栏主题标记等定向跳转仍传具体分区） */
  onOpenSettings: (section?: SettingsSectionKey) => void;
  /** 移除会话后回抛被移除 agent 的 id（App 按复合键清选中） */
  onRemoved: (id: string) => void;
  /** 工程便签（agent-hub-notes）：⋯ 菜单入口上抛 App（悬浮卡 + 钉住自动展开归 App 管） */
  onOpenNotes: (a: AgentSummary) => void;
}

/// 组桶标签（C2 i18n：label 存 key，渲染期翻译——buildInstanceGroups 输出随 locale）。
/// R9（用户裁决 A：严格首位）：等待组排最前——待回复会话需第一时间可见，不再沉底手找。
export const GROUP_LABELS: { key: string; labelKey: string }[] = [
  { key: 'needsInput', labelKey: 'group.needsInput' },
  { key: 'working', labelKey: 'group.working' },
  { key: 'other', labelKey: 'group.other' },
  { key: 'completed', labelKey: 'group.completed' },
];

/// 单个实例分组头可折叠状态（任务6）：键 = `${instanceId}:${groupKey}`，实例间互不影响。
export function groupKeyOf(instanceId: string | null, key: string): string {
  return `${instanceId ?? ''}:${key}`;
}

/// 按状态分组（agent-hub-agent-list-status-grouping G1：去节点分组——跨实例合并，
/// 不再以实例/节点为分组头）。节点过滤（G2）只影响列表显示：'all' | 实例 id |
/// 'local'（本机哨兵，实例 id 可空故需哨兵）。既有语义保留：Hub 助手独立桶置顶；
/// 活跃桶（working/needsInput）永不被工程过滤隐藏；空闲/已完成受工程族过滤管
/// （远程会话整组豁免——remoteIds 收全部远程实例，与原逐实例语义等价）。
export function buildStatusGroups(
  instances: InstanceCtx[],
  grouped: GroupedAgents,
  checked: ReadonlySet<string>,
  anchors: string[],
  nodeFilter: string,
): { key: string; label: string; agents: AgentSummary[] }[] {
  const ACTIVE_KEYS: ReadonlySet<string> = new Set(['working', 'needsInput']);
  const instIdOf = (a: AgentSummary): string | null => a.instanceId ?? null;
  const matched = (bucket: AgentSummary[]) =>
    bucket.filter((a) => nodeFilter === 'all' || instIdOf(a) === (nodeFilter === 'local' ? null : nodeFilter));
  // P7：Hub 助手（driver=pi）独立分类置顶——功能维度桶，不随状态漂移四桶
  //（不参与工程族过滤：常驻入口不因过滤隐藏）
  const hub = matched([...grouped.needsInput, ...grouped.working, ...grouped.other, ...grouped.completed]).filter(
    (a) => a.driver === 'pi',
  );
  const notHub = (bucket: AgentSummary[]) => matched(bucket).filter((a) => a.driver !== 'pi');
  // 远程豁免族过滤：全部远程实例 id 入豁免集（与原「远程整组保留」逐实例语义等价）
  const remoteIds = new Set(instances.map((i) => i.id).filter((id): id is string => id != null));
  return [
    ...(hub.length > 0 ? [{ key: 'hubAssistant', label: t('group.hubAssistant'), agents: hub }] : []),
    ...GROUP_LABELS.map(({ key, labelKey }) => {
      const bucket = grouped[key as keyof GroupedAgents] as AgentSummary[];
      return {
        key,
        label: t(labelKey),
        agents: ACTIVE_KEYS.has(key)
          ? notHub(bucket)
          : filterAgents(notHub(bucket), checked, anchors, remoteIds),
      };
    }),
  ];
}

/// 左侧栏（Sidebar-v3，反馈轮 18，Penpot Sidebar-v3 板）：顶部品牌条 + 可折叠状态
/// 分组（▾工作中 ▸等待 ▾空闲 ▸已完成，工作中/空闲默认展开，等待/已完成折叠只显
/// 组头，点击组头切换；空组隐藏）+ 行状态圆点（agent-dot--group，2026-09-23 回归
/// 恢复）+ 底部功能菜单。多实例：agent-hub-agent-list-status-grouping 起去节点
/// 分组——状态组跨实例合并，节点经过滤器（全部/各节点，离线可辨可选）筛选，
/// 会话名多实例视图带「· 节点名」后缀，离线实例行置灰。
export default function AgentList({
  checked,
  onClearFilter,
  onSelect,
  selectedId,
  onOpenSearch,
  onCollapse,

  onOpenSettings,
  onRemoved,
  onOpenNotes,
}: Props): ReactElement {
  const { grouped, isLoading, tree, instances, offline, authError, registry } = useAgents();
  // 驱动级降级（急修第二刀）：dsh 收编带认证实例且未配 token → 侧栏「认证失效」banner
  const driverStatus = useQuery({
    queryKey: ['drivers-status'],
    queryFn: registry.apiFor(null).getDriverStatus,
    refetchInterval: 10_000,
  });
  const driverAuthErrors = Object.entries(driverStatus.data ?? {}).filter(([, v]) => v.authError);
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
  // Sidebar-v3 折叠态（R9 默认：工作中/等待/空闲展开，仅已完成折叠只显组头）
  // agent-list-status-grouping：去实例维后折叠键 = 组键本身（状态组全局唯一）
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  const isCollapsed = (key: string): boolean => {
    const v = collapsed[key];
    // R9：等待组默认展开（原与已完成同折叠，用户需手动找——回归单折叠 completed）
    return v != null ? v : key === 'completed';
  };

  const toggleCollapse = (key: string): void => {
    setCollapsed((c) => ({ ...c, [key]: !(c[key] ?? (key === 'completed')) }));
  };

  // 节点过滤器（G2）：'all' | 实例 id | 'local'——纯显示层过滤，不动数据刷新
  const [nodeFilter, setNodeFilter] = useState<string>('all');

  const handleAction = (action: SessionAction, a: AgentSummary): void => {
    // 远程实例会话操作按实例路由（任务7 前置）：logs/中断/stop/respawn/remove 走
    // per-instance api —— ChatTab 已按 instanceId 路由播放；此处侧栏操作同样分流。
    const instApi = registryFor(a.instanceId ?? null);
    if (action === 'logs') {
      instApi
        .logs(a.id, a.driver)
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
      else instApi.interruptAgent(a.id, a.driver).catch((e: unknown) => alert(String(e)));
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
        action: () => instApi.stopAgent(a.id, a.driver).catch((e: unknown) => alert(String(e))),
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
        action: () => instApi.respawnAgent(a.id, a.driver).catch((e: unknown) => alert(String(e))),
      });
      return;
    }
    // unlist（R8）
    if (action === 'unlist') {
      // R8：interactive 前台会话从列表移除——仅持久隐藏条目（按 sessionId），
      // 不动底层进程；进程退出后条目自然消失、忽略记录随之清理
      setConfirmReq({
        title: t('confirm.unlist.title'),
        message: t('confirm.unlist.message', { name: a.name ?? a.id }),
        banner: t('confirm.unlist.banner'),
        variant: 'danger',
        confirmLabel: t('confirm.unlist.label'),
        action: () =>
          instApi
            .unlistAgent(a.id, a.driver, a.sessionId)
            .then(() => {
              queryClient.invalidateQueries({ queryKey: ['agents'] });
              onRemoved(a.id);
            })
            .catch((e: unknown) => alert(String(e))),
      });
      return;
    }
    setConfirmReq({
      title: t('confirm.remove.title'),
      message: t('confirm.remove.message', { name: a.name ?? a.id }),
      banner: t('confirm.remove.banner'),
      variant: 'danger',
      confirmLabel: t('confirm.remove.label'),
      action: () =>
        instApi
          .removeAgent(a.id, a.driver)
          .then(() => {
            queryClient.invalidateQueries({ queryKey: ['agents'] });
            onRemoved(a.id);
          })
          .catch((e: unknown) => alert(String(e))),
    });
  };

  if (isLoading) return <div role="status">{t('sidebar.loading')}</div>;

  // 状态分组（G1 去节点分组）：跨实例合并 + 节点过滤（G2 纯显示层）
  const statusGroups = buildStatusGroups(instances, grouped, checked, anchors, nodeFilter);
  // 会话名节点后缀（G3）：多实例视图统一带「· 节点名」；单本机省略（无歧义不添噪）
  const multiNode = instances.length > 1;
  const instNameOf = (a: AgentSummary): string =>
    a.instanceId == null ? t('inst.local') : (instances.find((i) => i.id === a.instanceId)?.name ?? '');
  // 行置灰（G4 离线不回归）：所属实例离线 → 行级降透明（原实例组头置灰语义下沉到行）
  const isOfflineInst = (a: AgentSummary): boolean => !!offline[a.instanceId ?? ''];
  // 被工程过滤隐藏的会话数（仅空闲/已完成受滤桶统计；节点过滤不计入——那是主动筛选）
  let hiddenCount = 0;
  for (const g of statusGroups) {
    if (g.key === 'hubAssistant' || g.key === 'working' || g.key === 'needsInput') continue;
    hiddenCount += Math.max(
      (grouped[g.key as keyof GroupedAgents]?.filter(
        (a) => nodeFilter === 'all' || (a.instanceId ?? null) === (nodeFilter === 'local' ? null : nodeFilter),
      ).length ?? 0) - g.agents.length,
      0,
    );
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
        {/* 节点过滤器（G2）：全部 + 各实例/节点（离线可辨可选）——纯显示层过滤 */}
        <div className="node-filter" role="group" aria-label={t('sidebar.nodeFilterAria')}>
          <button
            type="button"
            className={`node-chip ${nodeFilter === 'all' ? 'node-chip--active' : ''}`}
            aria-pressed={nodeFilter === 'all'}
            onClick={() => setNodeFilter('all')}
          >
            {t('sidebar.filterAll')}
          </button>
          {instances.map((inst) => {
            const chipVal = inst.id ?? 'local';
            const off = !!offline[inst.id ?? ''];
            return (
              <button
                key={chipVal}
                type="button"
                className={`node-chip ${nodeFilter === chipVal ? 'node-chip--active' : ''}${off ? ' node-chip--offline' : ''}`}
                aria-pressed={nodeFilter === chipVal}
                title={off ? t('sidebar.offlineTitle') : undefined}
                onClick={() => setNodeFilter(chipVal)}
              >
                {inst.id == null ? t('inst.local') : inst.name}
                {off ? <span className="node-chip-offline">{t('sidebar.offline')}</span> : null}
              </button>
            );
          })}
        </div>
        {driverAuthErrors.map(([name]) => (
          <p className="sb-empty" key={`drv-auth-${name}`}>
            <button
              type="button"
              className="inst-group-autherr"
              title={t('sidebar.driverAuthErrorTitle')}
              onClick={() => onOpenSettings('instances')}
            >
              {name.toUpperCase()}
              {t('sidebar.driverAuthError')}
            </button>
          </p>
        ))}
        {/* 实例凭据失效横幅（任务10 能力保留）：去实例分组头后仍可辨可跳设置 */}
        {instances
          .filter((inst) => inst.id != null && authError[inst.id])
          .map((inst) => (
            <p className="sb-empty" key={`auth-${inst.id}`}>
              <button
                type="button"
                className="inst-group-autherr"
                title={t('sidebar.authErrorTitle')}
                onClick={() => onOpenSettings('instances')}
              >
                {inst.name}
                {t('sidebar.authError')}
              </button>
            </p>
          ))}
        {statusGroups.every((g) => g.agents.length === 0) ? (
          <p className="sb-empty">{t('sidebar.empty')}</p>
        ) : (
          statusGroups
            .filter((g) => g.agents.length > 0)
            .map(({ key, label, agents }) => (
              <div className="agent-group" key={key}>
                <button
                  type="button"
                  className="agent-group-head"
                  aria-expanded={!isCollapsed(key)}
                  onClick={() => toggleCollapse(key)}
                >
                  <span className="agent-group-caret" aria-hidden="true">
                    {isCollapsed(key) ? '▸' : '▾'}
                  </span>
                  {label}
                  <span className="agent-group-count">{agents.length}</span>
                </button>
                {!isCollapsed(key)
                  ? agents.map((agent) => (
                      <AgentRow
                        key={`${agent.instanceId ?? ''}:${agent.driver}:${agent.id}`}
                        agent={agent}
                        onSelect={onSelect}
                        onAction={handleAction}
                        selected={agent.id === selectedId}
                        // assistant-singleton 特例：助手每 gateway 唯一（聚合层已
                        // 去重），无节点歧义不添「· 节点名」后缀；一般 agent 不动
                        nodeSuffix={
                          multiNode && agent.driver !== 'pi' ? instNameOf(agent) : undefined
                        }
                        muted={isOfflineInst(agent)}
                      />
                    ))
                  : null}
              </div>
            ))
        )}
      </div>
      {/* 过滤隐藏提示条（反馈轮 24-A，Penpot filter-hidden-hint）：仅在有会话被
          工程过滤隐藏时显示，点击清空勾选回到全显 */}
      {hiddenCount > 0 ? (
        <button type="button" className="filter-hidden-hint" onClick={onClearFilter}>
          {t('sidebar.hiddenHint', { n: hiddenCount })}
        </button>
      ) : null}
      {/* 底部功能菜单收敛为唯一「设置」入口（agent-hub-settings-consolidation S1）：
          工程列表/Agent 配置/实例管理/新建 Agent/历史会话全部为设置页分区（S2） */}
      <div className="sb-menu">
        <button type="button" className="sb-menu-item" onClick={() => onOpenSettings()}>
          <span className="sb-menu-icon" aria-hidden="true">⚒</span>
          {t('menu.settings')}
        </button>
      </div>
      {confirmReq ? <ConfirmDialog request={confirmReq} onClose={() => setConfirmReq(null)} /> : null}
    </div>
  );
}
