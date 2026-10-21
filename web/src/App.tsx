import type { CSSProperties, ReactElement } from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useAgents, familyAnchors } from './hooks/useAgents';
import ProjectSidebar from './components/ProjectSidebar';
import MiniBar from './components/MiniBar';
import FilesTree from './components/FilesTree';
import FileTabPanel from './components/FileTabPanel';
import TerminalTab from './components/TerminalTab';
import PanelShell, {
  PANEL_FILES_W,
  PANEL_FILES_W_KEY,
  PANEL_GIT_W,
  PANEL_GIT_W_KEY,
  PANEL_W,
  PANEL_W_KEY,
  loadPanelPrefs,
  useDesktop,
} from './components/PanelShell';
import BottomBar, { PANELS_KEY, loadPanelVis, type PanelKey, type PanelVis } from './components/BottomBar';
import GitPanel from './components/GitPanel';
import GitDiffView from './components/GitDiffView';
import AgentList from './components/AgentList';
import ChatTab from './components/ChatTab';
import QuickChat from './components/QuickChat';
import SettingsPage, { lastSettingsSection, type SettingsSectionKey } from './components/SettingsPage';
import PluginNavGroup from './components/PluginNavGroup';
import PluginViewHost from './components/PluginViewHost';
import HighRiskApprovalDialog from './components/HighRiskApprovalDialog';
import { plugins, navItemsOf, type PluginNavItem, type PluginView } from './plugins';
import { usePluginRenderers } from './lib/usePluginRenderers';
import TokenGate from './components/TokenGate';
import ProjectNotesDialog, { isNotePinned } from './components/ProjectNotesDialog';
import SessionHistoryDialog from './components/SessionHistoryDialog';
import Toast from './components/Toast';
import { api, clearStoredToken, onUnauthorized } from './api';
import { useInplaceResumeWatch } from './hooks/useInplaceResumeWatch';
import { useI18n } from './i18n';
import type { AgentSummary } from './types';
import './App.css';
import './components/ProjectSidebar.css';

/// 右侧工具面板内容 id（2026-10-06 用户定模型：面板是一个壳，装不同内容而已）——
/// 一个壳层统一承载遮罩/互斥/桌面竖栏/移动抽屉，projects/git/files 是它装载的视图
/// （agent-hub-files-panel F1：files = 会话 cwd 文件树，面板-主功能区模型第三组）
type RightPanelId = 'projects' | 'git' | 'files';

/// 首帧右面板解析（单壳收敛）：桌面按 hub_panels 持久化恢复——files/git 优先级
/// 依次回落（多视图存储下最多恢复一个，互斥内生于单状态）；移动端恒 null（抽屉态，
/// 靠用户操作打开，首帧不写回，不冲桌面偏好）。
function initialRightPanel(): RightPanelId | null {
  if (typeof window === 'undefined' || !window.matchMedia('(min-width: 769px)').matches) {
    return null;
  }
  const stored = loadPanelVis();
  return stored.files ? 'files' : stored.git ? 'git' : stored.projects ? 'projects' : null;
}

import './components/BottomBar.css';

/// 单会话页签组（agent-hub-files-panel F2 补强：Tab 组按会话分组）
interface SessionTabGroup {
  files: { path: string; name: string }[];
  /** 组内聚焦页签：文件绝对路径 / 'terminal' / 'diff' / null = 聊天 */
  active: string | null;
  /** 终端页签（每会话单例）：cwd/instanceId 打开时捕获（隐藏保活、关页签回收 PTY） */
  terminal: { cwd: string; instanceId: string | null } | null;
  /** diff 页签（F4 v2：会话级单例，新 diff 替换内容不增页签；instanceId 按打开时捕获；
   * hash = 提交内文件（git-timeline-files 两级联动），缺省 = 工作区 diff */
  diff: { cwd: string; file: string; cached: boolean; hash?: string; instanceId: string | null } | null;
}

/// 复合选中 id：`${instanceId}:${agentId}`——跨实例 agentId 可能碰撞（本机与远程
/// 同 agent id），单一 id 无法定位（任务7）。
function selectKey(a: AgentSummary): string {
  return `${a.instanceId ?? ''}:${a.id}`;
}

export default function App(): ReactElement {
  const queryClient = useQueryClient();
  const t = useI18n();
  usePluginRenderers();
  const { data: polled, registry, tree } = useAgents();
  // 插件视图（agent-hub-plugin-system B1）：与设置页同级的整页视图（ADR-0004 第 2 条
  // 「主内容区整页」），由 /api/plugins 数据动态驱动；null = 未打开（零插件时不存在
  // 任何入口，导航集合与引入前一致）
  const [pluginView, setPluginView] = useState<PluginNavItem | null>(null);
  // 当前打开视图的插件快照（查询数据驱动；卸载/停用后自动回落关闭）
  const pluginViewData = useQuery({
    queryKey: ['plugins'],
    queryFn: plugins.list,
    refetchInterval: 10_000,
  });
  const pluginViewPlugin: PluginView | null = useMemo(
    () =>
      pluginView != null
        ? (pluginViewData.data?.find((p) => p.id === pluginView.pluginId) ?? null)
        : null,
    [pluginView, pluginViewData.data],
  );
  // 停用/卸载正在查看的插件 → 视图自动关闭（BR-03 同现同隐，无死入口）。
  // 停用后条目仍在清单（disabled），必须显式判 enabled（tester 2 轮 FAIL-3）
  useEffect(() => {
    if (pluginView != null && (pluginViewPlugin == null || pluginViewPlugin.enabled === false)) {
      setPluginView(null);
    }
  }, [pluginView, pluginViewPlugin]);
  // 行名跟随（agent-hub-resume-inplace #21 功能点 5）：原地恢复后该行显示名改为
  // 被恢复会话的名（行 id/任务 id 不变，只覆写展示名）。在数据入口统一派生，
  // 侧栏行 / 主区标题 / 弹窗标题等全部下游自然跟随，无需逐组件传透
  const [nameOverrides, setNameOverrides] = useState<Record<string, string>>({});
  const data = useMemo(
    () =>
      Object.keys(nameOverrides).length === 0
        ? polled
        : polled.map((a) => {
            const name = nameOverrides[a.id];
            return name != null && a.name !== name ? { ...a, name } : a;
          }),
    [polled, nameOverrides],
  );
  // 工程族锚（历史会话分区过滤同款语义；推导同 AgentList）
  const anchors = familyAnchors(tree);
  // 选中只存复合键（`${instanceId}:${agentId}`，任务7：跨实例 agentId 碰撞）——
  // 渲染时按 key 从轮询列表实时取值（快照对象会让 group 停在选中瞬间，
  // spinner 复位等依赖 agent.group 的逻辑失效——tester-r12 FAIL 根因）。
  // 历史兜底（2026-10-02 全驱动展示纪律）：从历史面板点开的会话不在默认清单里
  // （Done/历史不进侧栏），用点开时的快照兜底解析——发送复活后轮询清单即命中
  // 快照退位，group 恢复实时口径；不复活则保持完成态快照（只读历史照常可看）。
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [historySel, setHistorySel] = useState<AgentSummary | null>(null);
  // 未发送草稿按会话键保留（agent-hub-pick-switch #18 交互模型 4）：ChatTab 是
  // 会话键重挂载组件（切会话 = 新实例），草稿若留组件本地态必丢。此处按复合键
  // 存一份、挂载时喂回，故「前 agent 打字 → 切走 → 切回」草稿仍在，且各 agent
  // 各存各的（切到 B 不会带出 A 的草稿）。
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  // 同值不写（返回原引用）：ChatTab 的草稿上行 effect 每次渲染都跑，无此守卫会
  // 造成「写状态 → App 重渲 → effect 再跑」的自激环
  const setDraft = useCallback((key: string, text: string): void => {
    setDrafts((d) => (d[key] === text ? d : { ...d, [key]: text }));
  }, []);
  const selected =
    selectedKey != null
      ? (data.find((a) => selectKey(a) === selectedKey) ??
        (historySel != null && selectKey(historySel) === selectedKey ? historySel : null))
      : null;
  // P4 默认视图（agent-hub-pi-hub-agent）：无选中持久化机制（selectedKey 仅内存），
  // 裁量 = 每次打开默认 Hub 助手：数据首次到位且用户尚未选择时选中 pi 条目；
  // 用户手动切走（selectedKey 非空）后不再干预，尊重用户选择。
  useEffect(() => {
    if (selectedKey != null) return;
    const hub = data.find((a) => a.driver === 'pi');
    if (hub != null) setSelectedKey(selectKey(hub));
  }, [data, selectedKey]);
  const selectedApi = useMemo(() => registry.apiFor(selected?.instanceId ?? null), [selected, registry]);
  // 工程过滤状态：勾选集合提升到 App，驱动 AgentList 过滤
  // 工程列表走 react-query（staleTime=∞ + 关 focus refetch：仅显式 invalidate 才重拉）
  const { data: projects = [] } = useQuery<string[]>({
    queryKey: ['projects'],
    queryFn: () => api.listProjects(),
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });
  const [checked, setChecked] = useState<ReadonlySet<string>>(new Set());
  const [pending, setPending] = useState('');
  // 抽屉开关（≤768px：左列表/右面板均为抽屉；桌面右栏可收起）
  const [leftOpen, setLeftOpen] = useState(false);
  // 右侧工具面板（2026-10-06 用户定模型：面板是一个壳，装不同内容而已）——
  // 单状态单源：'projects' | 'git' | null；互斥内生于状态，遮罩/桌面竖栏/移动抽屉
  // 全由壳层统一承载，不再有各自为政的开关标志。桌面首帧按持久化恢复；移动端恒关。
  const [rightPanel, setRightPanel] = useState<RightPanelId | null>(initialRightPanel);
  // 首次用户操作前不同步持久化（手机打开 app 不冲掉桌面的面板偏好）
  const [rightPanelTouched, setRightPanelTouched] = useState(false);
  const openRightPanel = (id: RightPanelId): void => {
    setRightPanelTouched(true);
    setRightPanel(id);
  };
  const toggleRightPanel = (id: RightPanelId): void => {
    setRightPanelTouched(true);
    setRightPanel((cur) => (cur === id ? null : id));
  };
  const closeRightPanel = (): void => {
    setRightPanelTouched(true);
    setRightPanel(null);
  };
  // Zed 式底部栏面板显隐（持久化 hub_panels；sessions 顶替原 leftCollapsed）——
  // projects/git/files 位是单状态的派生缓存（BottomBar 高亮数据源）：初始即按单状态
  // 收敛，旧版「双 flag 双事实源」的存储残留首帧归一
  const [panels, setPanels] = useState<PanelVis>(() => {
    const stored = loadPanelVis();
    const rp = initialRightPanel();
    return {
      sessions: stored.sessions,
      projects: rp === 'projects',
      git: rp === 'git',
      files: rp === 'files',
    };
  });
  // 持久化写回守卫（R4）：首帧收敛快照相同的 panels 不落盘——手机打开 app 时
  // 单状态收敛（rp=null → projects/git=false）与桌面存储的差异不得写回冲掉桌面
  // 偏好。守卫仅首帧未操作期生效：右面板一经用户操作（rightPanelTouched），
  // 当前值即用户意图——即便操作序列恰好回到首帧快照也必须写盘，否则中间值
  // 残留存储 → reload 回退中间面板（所见非所得，files-panel 走查 FAIL1 实证）。
  // 判定在值不在次数（StrictMode 双跑安全）。
  const initialPanelsRef = useRef<PanelVis | null>(null);
  if (initialPanelsRef.current === null) initialPanelsRef.current = panels;
  useEffect(() => {
    const init = initialPanelsRef.current;
    if (
      !rightPanelTouched &&
      init != null &&
      panels.sessions === init.sessions &&
      panels.projects === init.projects &&
      panels.git === init.git
    ) {
      return;
    }
    window.localStorage.setItem(PANELS_KEY, JSON.stringify(panels));
  }, [panels, rightPanelTouched]);
  // 用户操作后单状态 → panels 同步（高亮跟随 + 持久化写回；首帧不写，守卫见上）
  useEffect(() => {
    if (!rightPanelTouched) return;
    setPanels((p) => {
      const projects = rightPanel === 'projects';
      const git = rightPanel === 'git';
      const files = rightPanel === 'files';
      return p.projects === projects && p.git === git && p.files === files
        ? p
        : { ...p, projects, git, files };
    });
  }, [rightPanel, rightPanelTouched]);
  // 快速对话入口：⌘K 命令面板
  const [showQuickChat, setShowQuickChat] = useState(false);
  // agents 配置管理器入口
  // 设置页（agent-hub-settings B1）：null = 未打开，值 = 打开时落的分区。
  // 覆盖层渲染，主界面不卸载——关闭后选中会话/抽屉/折叠原样保留。
  const [settingsSection, setSettingsSection] = useState<SettingsSectionKey | null>(null);
  const openSettings = (section?: SettingsSectionKey): void =>
    setSettingsSection(section ?? lastSettingsSection());
  const closeSettings = (): void => setSettingsSection(null);
  // 工程便签悬浮卡（agent-hub-notes）：⋯ 菜单/顶栏切换开；钉住的工程随会话自动展开
  const [notesFor, setNotesFor] = useState<AgentSummary | null>(null);
  // CLI 历史会话弹窗（agent-hub-session-history）：⋯ 菜单开；恢复成功 onSelect 切选中
  const [sessionsFor, setSessionsFor] = useState<AgentSummary | null>(null);
  // 一次性提示条（agent-hub-pick-switch #18 交互模型 6）：切换成功提示「已切换到
  // <会话名>」，自动消失、不阻塞。计时器挂状态不挂 Toast 组件内——父级因轮询
  // 频繁重渲，组件内定时器会被反复重置而永不消失。
  const [toast, setToast] = useState<string | null>(null);
  useEffect(() => {
    if (toast == null) return;
    const timer = setTimeout(() => setToast(null), 2600);
    return () => clearTimeout(timer);
  }, [toast]);
  // 原地恢复装载跟随（agent-hub-resume-inplace #21 路由 2）：/resume 注入后
  // 有界等待 watcher 把选中行 sessionId 跟随到目标会话——命中静默收尾（视图
  // 自然跟随），超时明确报错（TUI 未装载，视图不动）。心跳失效 agents 查询
  // 加速拾取，不让用户干等轮询周期
  const refreshAgents = useCallback((): void => {
    void queryClient.invalidateQueries({ queryKey: ['agents'] });
  }, [queryClient]);
  const onInplaceTimeout = useCallback((): void => {
    setToast(t('sesshist.inplaceFailedToast'));
  }, [t]);
  const startResumeWatch = useInplaceResumeWatch({
    agents: data,
    onTimeout: onInplaceTimeout,
    refresh: refreshAgents,
  });
  // token 门禁：仅 allow_lan 模式遇 401 触发；localhost 不触发
  const [needToken, setNeedToken] = useState(false);
  // 面板宽度（工程 + Git + 文件三视图各自持久化 hub_panel_right_w /
  // hub_panel_git_w / hub_panel_files_w）；桌面经 --panel-w（projects）/ 内联
  // width（git/files）作用于 .pane-right / .pane-right--git / --files
  // （App.css 769+ 段消费，移动端抽屉不受影响）
  const [panelPref, setPanelPref] = useState(loadPanelPrefs);
  useEffect(() => {
    window.localStorage.setItem(PANEL_W_KEY, String(panelPref.width));
    window.localStorage.setItem(PANEL_GIT_W_KEY, String(panelPref.gitWidth));
    window.localStorage.setItem(PANEL_FILES_W_KEY, String(panelPref.filesWidth));
  }, [panelPref]);
  const desktop = useDesktop();
  // Git diff 主区覆盖视图（Git 面板三改 #1）：点击变更文件打开，ESC/关闭钮收回；
  // 仅活跃会话可触发（GitPanel 挂载前提），覆盖层卸载即回原视图，聊天状态不受影响。
  // cwd = diff 所属仓库层路径（submodule 钻取后与会话 cwd 不同，由 GitPanel 上报）
  // （diffFile 覆盖态已退役——F4 v2 diff 页签化：会话级单例进 tabGroups）
  // 主区多页签（agent-hub-files-panel F2 v2）：聊天 = 常驻页签（不可关），
  // 打开的文件各占一个可关页签（多文件并存互不冲掉），聚焦键 = 文件绝对路径
  // （'terminal' = 终端页签，agent-hub-terminal T1；null = 聚焦聊天）。
  // 切走仅 CSS 隐藏不卸载，聊天滚动/草稿与文件滚动均保留
  // Tab 组按会话分组（F2 补强，用户原话：「一个会话里的 tab 为一组……不要切了
  // 会话就把打开的 tab 全关掉」）：键 = 复合选中键（instanceId:id 跨实例唯一），
  // 切换会话整组切换、各组面板常挂（隐藏不卸载，滚动/终端后台态保留）；页签只随
  // 用户手动关闭而关闭。active = 该组聚焦页签（文件绝对路径 / 'terminal' / null=聊天）
  const [tabGroups, setTabGroups] = useState<Record<string, SessionTabGroup>>({});
  const curGroup: SessionTabGroup | undefined = selectedKey != null ? tabGroups[selectedKey] : undefined;
  const fileTabs = curGroup?.files ?? [];
  const activeTab = curGroup?.active ?? null;
  const terminalTab = curGroup?.terminal ?? null;

  const patchGroup = (key: string, fn: (g: SessionTabGroup) => SessionTabGroup): void => {
    setTabGroups((g) => ({
      ...g,
      [key]: fn(g[key] ?? { files: [], active: null, terminal: null, diff: null }),
    }));
  };

  /// 打开/替换 diff 页签（F4 v2）：GitPanel 文件点击 → 会话级单例（新 diff 替换
  /// 内容不增页签）并聚焦；instanceId 打开时捕获（隐藏保活按实例路由）
  const openDiffTab = (f: { cwd: string; file: string; cached: boolean; hash?: string }): void => {
    if (selectedKey == null) return;
    const instanceId = selected?.instanceId ?? null;
    patchGroup(selectedKey, (g) => ({ ...g, active: 'diff', diff: { ...f, instanceId } }));
  };
  /// 关 diff 页签（指定组——非聚焦组的隐藏页签也可被自身 × 关闭，语义一致）
  const closeDiffTab = (key: string): void => {
    patchGroup(key, (g) => ({
      ...g,
      diff: null,
      active: g.active === 'diff' ? null : g.active,
    }));
  };
  // ESC 关「当前聚焦组」的 diff 页签（F4 v2）：页签形态的 GitDiffView 不挂
  // window 监听（常挂全组若各自注册，单次 ESC 会批量误关所有会话 diff——
  // reviewer 终审 BLOCKER），故上移 App 层按聚焦组收敛；设置页打开时让位
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Escape' || settingsSection != null) return;
      if (activeTab !== 'diff' || selectedKey == null) return;
      if (tabGroups[selectedKey]?.diff == null) return;
      closeDiffTab(selectedKey);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [activeTab, selectedKey, tabGroups, settingsSection]);
  const setActiveTab = (tab: string | null): void => {
    if (selectedKey == null) return;
    patchGroup(selectedKey, (g) => ({ ...g, active: tab }));
  };

  /// 打开/聚焦终端页签（ChatTab 顶栏入口，agent-hub-terminal T1）：cwd/instanceId
  /// 打开时捕获进组（隐藏保活跨会话切换；关页签回收 PTY，T3）
  const openTerminalTab = (): void => {
    if (selectedKey == null || selected?.cwd == null) return;
    const cwd = selected.cwd;
    const instanceId = selected.instanceId ?? null;
    patchGroup(selectedKey, (g) => ({ ...g, active: 'terminal', terminal: { cwd, instanceId } }));
  };
  /// 关终端页签：回聊天 + 回收 PTY（T2/T3）
  const closeTerminalTab = (): void => {
    if (selectedKey == null) return;
    patchGroup(selectedKey, (g) => ({
      ...g,
      active: g.active === 'terminal' ? null : g.active,
      terminal: null,
    }));
  };

  /// 打开插件视图（B2 批4：hub-plugin view 型 command 的执行语义）——复用
  /// PluginNavGroup 同款 pluginView 态；清单实时拉（启停即生效，BR-03）
  const openPluginView = (pluginId: string, viewId: string): void => {
    void plugins
      .list()
      .then((list) => {
        const item = navItemsOf(list).find((n) => n.pluginId === pluginId && n.viewId === viewId);
        if (item != null) {
          setPluginView(item);
          setSettingsSection(null);
        }
      })
      .catch(() => {});
  };

  /// 打开/聚焦文件页签（面板树点击与 diff 跳源共用）：未开则新开并聚焦 + 右面板
  /// 切到文件树（F4 跳源同规）
  const openFileTab = (path: string, name: string): void => {
    if (selectedKey == null) return;
    patchGroup(selectedKey, (g) => ({
      ...g,
      files: g.files.some((tb) => tb.path === path) ? g.files : [...g.files, { path, name }],
      active: path,
    }));
    openRightPanel('files');
  };
  /// 关页签：关的是聚焦页签则回聊天（F2：当前 Tab 可关 = 回聊天）
  const closeFileTab = (path: string): void => {
    if (selectedKey == null) return;
    patchGroup(selectedKey, (g) => ({
      ...g,
      files: g.files.filter((tb) => tb.path !== path),
      active: g.active === path ? null : g.active,
    }));
  };
  // （F4 v2 后 diff 入页签组，随组按会话保留/恢复——切换会话不再需要清理动作；
  //  历史注释存档：diffFile 覆盖态时代此处清 diffFile 防跨会话残留）
  // 手机迷你底栏「Git」直达：Git 即面板内容（壳层唯一），无需再滚动定位
  // （原 gitSlotRef 滚动逻辑随「堆叠槽位」形态一并移除）

  useEffect(() => {
    // token 回填已上移 api.ts 模块加载期（先于 react-query 首查）；此处只挂 401 回调
    // 401 = 持久 token 失效：清两层存储再弹 Gate，防旧 token 自动重进死循环
    onUnauthorized(() => {
      clearStoredToken();
      setNeedToken(true);
    });
  }, []);

  // 反馈轮 24-A：过滤默认态改全不勾 = 显示全部——删除原"projectKey 变化全勾注册
  // 列表"的初始化 effect；用户主动勾选才启用过滤，刷新后默认全显

  const toggleProject = (path: string): void => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  // 右栏显隐统一入口（兼容旧调用点）：开 = 装载对应视图，关 = 壳收起（互斥内生于单状态）
  const setProjectsVisible = (v: boolean): void => (v ? openRightPanel('projects') : closeRightPanel());

  // 底部工具栏 toggle：sessions 左栏（left-collapsed 类）；右侧统一走面板壳
  const togglePanel = (k: PanelKey): void => {
    if (k === 'projects' || k === 'git' || k === 'files') {
      toggleRightPanel(k);
      return;
    }
    setPanels((p) => ({ ...p, [k]: !p[k] }));
  };

  const addProject = (): void => {
    const path = pending.trim();
    if (!path || projects.includes(path)) {
      setPending('');
      return;
    }
    const next = [...projects, path];
    setPending('');
    api
      .putProjects(next)
      .then(() => {
        queryClient.invalidateQueries({ queryKey: ['projects'] });
        setChecked((prev) => new Set(prev).add(path));
      })
      .catch((err) => alert(String(err)));
  };

  // 快捷键：⌘K 快速对话 / ⌘B 折叠/展开左栏（Sidebar-v2）
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'k' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setShowQuickChat((v) => !v);
      }
      if (e.key === 'b' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setPanels((p) => ({ ...p, sessions: !p.sessions }));
      }
      // 反馈轮 25-A：移动端抽屉打开时 Esc 关闭（与遮罩点击等价）；
      // 设置页打开时 Esc 只关最上层（设置页），抽屉不动（agent-hub-settings B1）
      if (e.key === 'Escape' && settingsSection === null) {
        if (leftOpen) setLeftOpen(false);
        // 右抽屉同规（单壳后移动端与左抽屉同权，R2 死锁的兜底第二路径）；
        // 桌面竖栏不经 Esc——关闭走 BottomBar toggle / 面板收起钮
        if (!desktop && rightPanel != null) closeRightPanel();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- leftOpen/rightPanel/settingsSection/desktop 参与 Esc 分层判定，防 stale closure
  }, [leftOpen, rightPanel, settingsSection, desktop]);

  // token 解锁：重拉全部数据
  const unlock = (): void => {
    setNeedToken(false);
    queryClient.invalidateQueries();
  };

  // 钉住自动展开（agent-hub-notes 增强）：钉住的工程，打开其任一会话便签自动弹出；
  // 未钉不自动开（从 ⋯ 菜单/顶栏手动开）。依赖 cwd 防同会话对象轮询重建反复触发。
  // 任务8：钉态带实例维（同 cwd 不同实例各自独立）；便签卡已开时切换到同 cwd
  // 另一实例的会话 → 卡跟随当前实例重绑（读写切到该实例的 notes.json）
  useEffect(() => {
    if (selected?.cwd && isNotePinned(selected.cwd, selected.instanceId ?? null)) {
      setNotesFor(selected);
    } else if (
      notesFor &&
      selected &&
      notesFor.cwd === selected.cwd &&
      (notesFor.instanceId ?? null) !== (selected.instanceId ?? null)
    ) {
      setNotesFor(selected);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- selected.id/cwd 变化即判定；对象引用不参与
  }, [selected?.id, selected?.cwd, selected?.instanceId]);

  const selectAgent = (a: AgentSummary): void => {
    setSelectedKey(selectKey(a));
    setLeftOpen(false); // 手机：选中后收起抽屉
    // 换会话回 Agent 视图：由 ChatTab 的 key（instanceId:id）重挂载天然保证，
    // 视图状态留在 ChatTab 本地，无需在此处理
  };

  const closeSelected = (): void => setSelectedKey(null);

  // Git 栏内容（桌面竖栏 / 移动抽屉堆叠共用）：跟随当前活跃会话的工作路径
  // （Git 面板三改 #2）——切会话即切 Git 状态；无会话/cwd 为空 → 占位。
  // 远端会话走实例代理（git 端点在 Api 工厂内按 baseUrl 前缀路由），无需占位降级
  const gitContent =
    selected?.cwd != null ? (
      <GitPanel
        cwd={selected.cwd}
        api={selectedApi ?? api}
        instanceId={selected.instanceId ?? null}
        onOpenDiff={openDiffTab}
      />
    ) : (
      <div className="git-panel">
        <div className="git-title">Git · —</div>
        <p className="git-empty">{t('proj.gitFollowSession')}</p>
      </div>
    );

  // 壳开合(right-open 类:移动抽屉 CSS 显隐依据;桌面直接条件渲染,类仅作状态镜像)
  const rightOpen = rightPanel != null;

  return (
    <div
      className={`layout ${leftOpen ? 'left-open' : ''} ${rightOpen ? 'right-open' : ''} ${panels.sessions ? '' : 'left-collapsed'} ${selected ? 'has-session' : ''}`}
    >
      {/* 侧栏不可见时（桌面折叠/手机抽屉关闭）的展开入口。
          sessions 翻转仅桌面（≥769px）——手机 ☰ 是纯开抽屉，写 sessions 会把
          桌面持久化语义污染进手机 localStorage（回归工单根因之一） */}
      <button
        type="button"
        className="sidebar-expand"
        aria-label={t('shell.expandSidebar')}
        onClick={() => {
          if (desktop) setPanels((p) => (p.sessions ? p : { ...p, sessions: true }));
          setLeftOpen(true);
        }}
      >
        ☰
      </button>
      {/* 反馈轮 25-A：移动端抽屉遮罩（≤768px 显示）——点击关闭抽屉；z-index 在
          抽屉（22）之下、页面内容之上 */}
      {leftOpen ? (
        <div className="drawer-overlay" aria-hidden="true" onClick={() => setLeftOpen(false)} />
      ) : null}
      {/* R2（本次工单核心）：右抽屉遮罩与左抽屉同款（.drawer-overlay ≤768px 显示，
          z 21 < 抽屉 22）——点击必关，与内容态无关（git 空态也有关闭路径，死锁不复
          存在）；桌面遮罩 CSS 恒隐藏，desktop 判定下连挂载都省去 */}
      {!desktop && rightPanel != null ? (
        <div className="drawer-overlay" aria-hidden="true" onClick={closeRightPanel} />
      ) : null}
      <div className="layout-body">
        {/* 左栏条件挂载：卸载语义仅桌面（会话隐藏即卸载，规格 C）；移动端恒挂载——
            抽屉是 CSS 显隐（left-open 类），AgentList 内嵌 useAgents（每实例 WS +
            staleTime 0 全量 agents），重挂即触发 WS 拆建 + 重取风暴（移动回归工单根因）。
            left-collapsed 类保留——浮动 ☰ 的桌面可见性依赖它 */}
        {panels.sessions || leftOpen || !desktop ? (
          <aside className="pane-left" aria-label="Agents">
            <AgentList
            checked={checked}
            onClearFilter={() => setChecked(new Set())}
            onSelect={selectAgent}
            selectedId={selected?.id ?? null}
            onOpenSearch={() => setShowQuickChat(true)}
            onCollapse={() => {
              setLeftOpen(false); // 手机：收起抽屉
              // 桌面：切换折叠（sessions 持久化）。移动端只关抽屉——折叠键若在手机上
              // 翻转 sessions 并落盘，此后每次开抽屉都全量重挂 AgentList（回归工单根因）
              if (desktop) setPanels((p) => ({ ...p, sessions: !p.sessions }));
            }}
            onOpenSettings={openSettings}
            onRemoved={(id) => {
              // 复合键与单实例 id 兼容：选中的正是被移除 agent 则清选中
              if (selectedKey === id || selectedKey?.endsWith(`:${id}`)) setSelectedKey(null);
              // agent 真删除（reviewer MINOR）：清其页签组，终端 PTY 随面板卸载自动
              // 回收（TerminalTab cleanup 已就绪）；closeSelected 仅取消选中不算删除，
              // 组保留（会话可再点选，Tab 组切回即恢复语义不变）
              setTabGroups((g) =>
                Object.fromEntries(Object.entries(g).filter(([k]) => !k.endsWith(`:${id}`))),
              );
            }}
            onOpenNotes={(a) => {
              setNotesFor(a);
              setLeftOpen(false);
            }}
            onOpenSessions={(a) => {
              setSessionsFor(a);
              setLeftOpen(false);
            }}
          />
          {/* 插件视图导航组（ADR-0004 第 2 条）：数据驱动、动态挂载；零插件不渲染 */}
          <PluginNavGroup
            onOpenView={(item) => {
              setPluginView(item);
              setSettingsSection(null);
              setLeftOpen(false); // 手机上开视图即收抽屉（并关已展开的设置页，防遮挡）
            }}
            onOpenManager={() => {
              setSettingsSection('plugins');
              setLeftOpen(false);
            }}
            activePluginId={pluginView?.pluginId ?? null}
          />
          </aside>
        ) : null}
        <main className="pane-main">
          {pluginView != null && pluginViewPlugin != null ? (
            <PluginViewHost
              plugin={pluginViewPlugin}
              view={pluginView}
              onExit={() => setPluginView(null)}
            />
          ) : selected != null ? (
            <>
              {/* 主区页签条（F2 v2）：仅有文件/终端页签时出现（纯聊天不占一行），
                  聊天为常驻首位页签；≤768px 横向滚动可用 */}
              {fileTabs.length > 0 || terminalTab != null || curGroup?.diff != null ? (
                <div className="main-tabs" role="tablist" aria-label={t('tabs.barAria')}>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={activeTab == null}
                    className={`main-tab${activeTab == null ? ' main-tab--active' : ''}`}
                    onClick={() => setActiveTab(null)}
                  >
                    <span className="main-tab-name">{t('tabs.chat')}</span>
                  </button>
                  {fileTabs.map((ft) => (
                    <span key={ft.path} className="main-tab-item">
                      <button
                        type="button"
                        role="tab"
                        title={ft.path}
                        aria-selected={activeTab === ft.path}
                        className={`main-tab${activeTab === ft.path ? ' main-tab--active' : ''}`}
                        onClick={() => setActiveTab(ft.path)}
                      >
                        <span className="main-tab-name">{ft.name}</span>
                      </button>
                      <button
                        type="button"
                        className="main-tab-close"
                        aria-label={`${t('tabs.close')} ${ft.name}`}
                        onClick={() => closeFileTab(ft.path)}
                      >
                        ×
                      </button>
                    </span>
                  ))}
                  {/* diff 页签（F4 v2）：会话级单例，新 diff 替换内容；标签=文件名 */}
                  {curGroup?.diff != null ? (
                    <span className="main-tab-item">
                      <button
                        type="button"
                        role="tab"
                        title={`${curGroup.diff.hash ? `${t('git.commitPrefix')} ${curGroup.diff.hash}` : curGroup.diff.cached ? t('git.staged') : t('git.worktree')} · ${curGroup.diff.cwd}/${curGroup.diff.file}`}
                        aria-selected={activeTab === 'diff'}
                        className={`main-tab${activeTab === 'diff' ? ' main-tab--active' : ''}`}
                        onClick={() => setActiveTab('diff')}
                      >
                        <span className="main-tab-name">{curGroup.diff.file}</span>
                      </button>
                      <button
                        type="button"
                        className="main-tab-close"
                        aria-label={`${t('tabs.close')} ${curGroup.diff.file}`}
                        onClick={() => closeDiffTab(selectedKey ?? '')}
                      >
                        ×
                      </button>
                    </span>
                  ) : null}
                  {/* 终端页签（agent-hub-terminal T1）：每会话单例，关闭回聊天（T2） */}
                  {terminalTab != null ? (
                    <span className="main-tab-item">
                      <button
                        type="button"
                        role="tab"
                        title={terminalTab.cwd}
                        aria-selected={activeTab === 'terminal'}
                        className={`main-tab${activeTab === 'terminal' ? ' main-tab--active' : ''}`}
                        onClick={() => setActiveTab('terminal')}
                      >
                        <span className="main-tab-name">{t('tabs.terminal')}</span>
                      </button>
                      <button
                        type="button"
                        className="main-tab-close"
                        aria-label={`${t('tabs.close')} ${t('tabs.terminal')}`}
                        onClick={closeTerminalTab}
                      >
                        ×
                      </button>
                    </span>
                  ) : null}
                </div>
              ) : null}
              {/* 聊天常驻页签槽：文件页签聚焦时仅 CSS 隐藏，消息流/输入草稿不丢 */}
              <div className={`pane-slot${activeTab != null ? ' pane-slot--hidden' : ''}`}>
                <ChatTab
                  key={`${selected.instanceId ?? ''}:${selected.id}`}
                  agent={selected}
                  api={selectedApi ?? undefined}
                  // 草稿按会话键保留（#18 交互模型 4）：key 换即重挂载，草稿从
                  // App 的 per-key 表喂回，切走再切回不丢、跨 agent 不串味
                  initialDraft={selectedKey != null ? (drafts[selectedKey] ?? '') : ''}
                  onDraftChange={(text) => setDraft(selectedKey ?? '', text)}
                  onClose={closeSelected}
                  onOpenSidebar={() => setLeftOpen(true)}
                  notesOpen={notesFor?.cwd === selected.cwd}
                  onToggleNotes={() =>
                    setNotesFor((cur) => (cur && cur.cwd === selected.cwd ? null : selected))
                  }
                  onOpenTerminal={selected.cwd != null ? openTerminalTab : undefined}
                  onOpenPluginView={openPluginView}
                />
              </div>
              {/* 文件页签面板：全会话组常挂（隐藏不卸载，滚动位置保留，Tab 组按会话
                  分组——切会话不销毁、切回即恢复），关页签才卸载 */}
              {Object.entries(tabGroups).flatMap(([key, grp]) =>
                grp.files.map((ft) => (
                  <FileTabPanel
                    key={`${key}:${ft.path}`}
                    path={ft.path}
                    active={key === selectedKey && grp.active === ft.path}
                  />
                )),
              )}
              {/* diff 页签面板（F4 v2）：全会话组常挂（隐藏保活，切会话整组保留），
                  关页签才卸载；跳源控件照常（激活/新开文件页签，diff 页签保留） */}
              {Object.entries(tabGroups).map(([key, grp]) =>
                grp.diff != null ? (
                  <div
                    key={`${key}:diff`}
                    className={`pane-slot${
                      key === selectedKey && grp.active === 'diff' ? '' : ' pane-slot--hidden'
                    }`}
                  >
                    <GitDiffView
                      cwd={grp.diff.cwd}
                      api={registry.apiFor(grp.diff.instanceId) ?? api}
                      file={grp.diff}
                      onClose={() => closeDiffTab(key)}
                      onOpenSource={(abs) => openFileTab(abs, abs.split('/').pop() ?? abs)}
                      embedded
                    />
                  </div>
                ) : null,
              )}
              {/* 终端页签面板：全会话组常挂（多会话多终端并存，PTY 隐藏保活同 T3——
                  切会话不回收，随各自页签关闭回收） */}
              {Object.entries(tabGroups).map(([key, grp]) =>
                grp.terminal != null ? (
                  <div
                    key={`${key}:terminal`}
                    className={`pane-slot${
                      key === selectedKey && grp.active === 'terminal'
                        ? ''
                        : ' pane-slot--hidden'
                    }`}
                  >
                    <TerminalTab
                      cwd={grp.terminal.cwd}
                      api={registry.apiFor(grp.terminal.instanceId) ?? api}
                      onClose={closeTerminalTab}
                    />
                  </div>
                ) : null,
              )}
            </>
          ) : (
            <div className="pane-placeholder">
              <p>{t('shell.placeholder')}</p>
            </div>
          )}
        </main>
        {/* 右侧工具面板单壳（2026-10-06 收敛）：一个容器装 projects / git / files 之一，
            由 rightPanel 驱动；null = 卸载（桌面竖栏收起 / 移动抽屉不挂，Zed 式
            toggle 规格：隐藏即卸载，宽度持久化值保留）。宽度三键各随各视图：
            projects 走 --panel-w（App.css 769+ 段消费），git/files 桌面直接内联 width
            （曾因 CSS 硬编码压掉 var 致拖宽失效，勿回退）——移动抽屉一律不内联
            （内联会压掉媒体查询固定宽） */}
        {rightPanel != null ? (
          <aside
            className={
              rightPanel === 'git'
                ? 'pane-right pane-right--git'
                : rightPanel === 'files'
                  ? 'pane-right pane-right--files'
                  : 'pane-right'
            }
            aria-label={
              rightPanel === 'git'
                ? t('bar.git')
                : rightPanel === 'files'
                  ? t('bar.files')
                  : 'Projects'
            }
            style={
              rightPanel === 'projects'
                ? ({ '--panel-w': `${panelPref.width}px` } as CSSProperties)
                : ({
                    width: desktop
                      ? rightPanel === 'git'
                        ? panelPref.gitWidth
                        : panelPref.filesWidth
                      : undefined,
                  } as CSSProperties)
            }
          >
            <PanelShell
              width={
                rightPanel === 'git'
                  ? panelPref.gitWidth
                  : rightPanel === 'files'
                    ? panelPref.filesWidth
                    : panelPref.width
              }
              onWidthChange={(w) =>
                setPanelPref((p) =>
                  rightPanel === 'git'
                    ? { ...p, gitWidth: w }
                    : rightPanel === 'files'
                      ? { ...p, filesWidth: w }
                      : { ...p, width: w },
                )
              }
              min={
                rightPanel === 'git'
                  ? PANEL_GIT_W.min
                  : rightPanel === 'files'
                    ? PANEL_FILES_W.min
                    : PANEL_W.min
              }
              max={
                rightPanel === 'git'
                  ? PANEL_GIT_W.max
                  : rightPanel === 'files'
                    ? PANEL_FILES_W.max
                    : PANEL_W.max
              }
              resetWidth={
                rightPanel === 'git'
                  ? PANEL_GIT_W.default
                  : rightPanel === 'files'
                    ? PANEL_FILES_W.default
                    : PANEL_W.default
              }
            >
              {rightPanel === 'git' ? (
                gitContent
              ) : rightPanel === 'files' ? (
                <FilesTree
                  root={selected?.cwd ?? null}
                  activePath={activeTab}
                  onOpenFile={openFileTab}
                />
              ) : (
                <ProjectSidebar
                  agents={data}
                  projects={projects}
                  checked={checked}
                  pending={pending}
                  onPendingChange={setPending}
                  onToggle={toggleProject}
                  onAdd={addProject}
                  onCollapse={() => setProjectsVisible(false)}
                />
              )}
            </PanelShell>
          </aside>
        ) : null}
      </div>
      <BottomBar
        vis={panels}
        onToggle={togglePanel}
        sessionCount={data.length}
        instanceCount={registry.instances.length}
      />
      {/* 手机迷你底栏（≤768px）：会话/Git 直达；桌面走上方全功能 BottomBar */}
      {!desktop ? (
        <MiniBar
          onSessions={() => setLeftOpen(true)}
          onGit={() => openRightPanel('git')}
          onFiles={() => openRightPanel('files')}
        />
      ) : null}
      {showQuickChat ? (
        <QuickChat
          agents={data}
          onPick={(a) => {
            setSelectedKey(selectKey(a));
            setShowQuickChat(false);
          }}
          onClose={() => setShowQuickChat(false)}
        />
      ) : null}
      {settingsSection ? (
        <SettingsPage
          section={settingsSection}
          onClose={closeSettings}
          ctx={{
            checked,
            anchors,
            projects: {
              agents: data,
              projects,
              pending,
              onPendingChange: setPending,
              onToggle: toggleProject,
              onAdd: addProject,
            },
            // 历史会话（S3）：点条目 = 关设置页 + 打开 chat（同原侧栏历史入口语义，
            // historySel 快照兜底保留——历史会话不在默认清单）
            onOpenHistory: (a) => {
              setHistorySel(a);
              selectAgent(a);
              setSettingsSection(null);
            },
            // 新建 Agent（S3）：创建成功 = 关设置页 + 选中新会话（原 StartDialog 语义）
            onCreatedAgent: (_driver, id) => {
              setSelectedKey(`:${id}`);
              setSettingsSection(null);
            },
          }}
          onOpenSession={(agentId) => {
            // Run 记录 sessionId 跳转：关设置页 + 选中该会话（agents 清单里找 id 命中项）
            setSettingsSection(null);
            const a = data.find((x) => x.id === agentId);
            if (a != null) setSelectedKey(selectKey(a));
          }}
        />
      ) : null}
      {/* 工程便签悬浮卡：无 cwd 会话不开启（claude/ACP 条目均带 cwd，防御性兜底） */}
      {notesFor && notesFor.cwd ? (
        <ProjectNotesDialog
          cwd={notesFor.cwd}
          title={notesFor.name ?? notesFor.id.slice(0, 8)}
          api={registry.apiFor(notesFor.instanceId ?? null)}
          instanceId={notesFor.instanceId ?? null}
          onClose={() => setNotesFor(null)}
        />
      ) : null}
      {/* CLI 历史会话弹窗（agent-hub-session-history）：选中 = 原子切换
          （agent-hub-pick-switch #18：选中 agent + 活跃会话 + 主区转录三者同切）——
          selectAgent 走复合键；新 bg 条目未进轮询清单时 historySel 快照承接，
          同 HistoryDialog 语义。spawned = 新条目标条目尚未在清单里，立即失效
          agents 查询把新条目尽快拉进来（否则受 WS 失效节流 5s 影响，窗口期内
          主区顶着快照名、侧栏无任何高亮，看着像「没切」）。
          恢复路由（agent-hub-resume-inplace #21）：inplace = 已向当前选中 TUI
          注入 /resume 原地装载——不切行、toast 说明 + 行名跟随会话名 + 启动
          装载跟随（watcher 刷新 sessionId 后消息视图自然跟随，超时明确报错）；
          spawn+busy = 选中行正忙没法注入，新行恢复并如实说明；其余沿用 #18 语义 */}
      {sessionsFor ? (
        <SessionHistoryDialog
          agent={sessionsFor}
          agents={data}
          selected={selected}
          api={registry.apiFor(sessionsFor.instanceId ?? null) ?? api}
          onSelect={(a, info) => {
            setHistorySel(a);
            selectAgent(a);
            if (info.mode === 'inplace') {
              setToast(t('sesshist.inplaceToast', { name: info.sessionName }));
              setNameOverrides((m) => ({ ...m, [a.id]: info.sessionName }));
              startResumeWatch({
                agentId: a.id,
                instanceId: a.instanceId ?? null,
                targetSessionId: info.targetSessionId ?? '',
              });
            } else if (info.spawnReason === 'busy') {
              setToast(t('sesshist.spawnBusyToast', { name: info.sessionName }));
            } else {
              setToast(t('sesshist.switchedToast', { name: info.sessionName }));
            }
            if (info.spawned) void queryClient.invalidateQueries({ queryKey: ['agents'] });
          }}
          onClose={() => setSessionsFor(null)}
        />
      ) : null}
      {toast != null ? <Toast text={toast} /> : null}
      {/* 高危审批确认卡（B2 批3）：自持轮询，无待确认单时渲染 null */}
      <HighRiskApprovalDialog />
      {needToken ? <TokenGate onUnlocked={unlock} /> : null}
    </div>
  );
}
