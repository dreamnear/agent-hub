import type { ReactElement } from 'react';
import { useEffect, useMemo, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useAgents } from './hooks/useAgents';
import ProjectSidebar from './components/ProjectSidebar';
import StartDialog from './components/StartDialog';
import AgentList from './components/AgentList';
import ChatTab from './components/ChatTab';
import QuickChat from './components/QuickChat';
import AgentsConfigPanel from './components/AgentsConfigPanel';
import SettingsPage, { type SettingsSectionKey } from './components/SettingsPage';
import TokenGate from './components/TokenGate';
import ProjectNotesDialog, { isNotePinned } from './components/ProjectNotesDialog';
import { api, clearStoredToken, onUnauthorized } from './api';
import { useI18n } from './i18n';
import type { AgentSummary } from './types';
import './App.css';
import './components/ProjectSidebar.css';
import './components/StartDialog.css';
import './components/AgentsConfigPanel.css';

/// 复合选中 id：`${instanceId}:${agentId}`——跨实例 agentId 可能碰撞（本机与远程
/// 同 agent id），单一 id 无法定位（任务7）。
function selectKey(a: AgentSummary): string {
  return `${a.instanceId ?? ''}:${a.id}`;
}

export default function App(): ReactElement {
  const queryClient = useQueryClient();
  const t = useI18n();
  const { data, registry } = useAgents();
  // 选中只存复合键（`${instanceId}:${agentId}`，任务7：跨实例 agentId 碰撞）——
  // 渲染时按 key 从轮询列表实时取值（快照对象会让 group 停在选中瞬间，
  // spinner 复位等依赖 agent.group 的逻辑失效——tester-r12 FAIL 根因）
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const selected =
    selectedKey != null
      ? (data.find((a) => selectKey(a) === selectedKey) ?? null)
      : null;
  const selectedApi = useMemo(() => registry.apiFor(selected?.instanceId ?? null), [selected, registry]);
  const [showStart, setShowStart] = useState(false);
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
  // 抽屉开关（≤768px：左列表/右工程均为抽屉；桌面右栏可收起）
  const [leftOpen, setLeftOpen] = useState(false);
  const [rightOpen, setRightOpen] = useState(false);
  // Sidebar-v2：桌面左栏折叠态（SB-TopBar 折叠按钮 / ⌘B 切换，浮动 ☰ 展开）
  const [leftCollapsed, setLeftCollapsed] = useState(false);
  // 快速对话入口：⌘K 命令面板
  const [showQuickChat, setShowQuickChat] = useState(false);
  // agents 配置管理器入口
  const [showAgentsCfg, setShowAgentsCfg] = useState(false);
  // 设置页（agent-hub-settings B1）：null = 未打开，值 = 打开时落的分区。
  // 覆盖层渲染，主界面不卸载——关闭后选中会话/抽屉/折叠原样保留。
  const [settingsSection, setSettingsSection] = useState<SettingsSectionKey | null>(null);
  const openSettings = (section: SettingsSectionKey): void => setSettingsSection(section);
  const closeSettings = (): void => setSettingsSection(null);
  // 工程便签悬浮卡（agent-hub-notes）：⋯ 菜单/顶栏切换开；钉住的工程随会话自动展开
  const [notesFor, setNotesFor] = useState<AgentSummary | null>(null);
  // token 门禁：仅 allow_lan 模式遇 401 触发；localhost 不触发
  const [needToken, setNeedToken] = useState(false);

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
        setLeftCollapsed((v) => !v);
      }
      // 反馈轮 25-A：移动端抽屉打开时 Esc 关闭（与遮罩点击等价）；
      // 设置页打开时 Esc 只关最上层（设置页），抽屉不动（agent-hub-settings B1）
      if (e.key === 'Escape' && leftOpen && settingsSection === null) {
        setLeftOpen(false);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- leftOpen/settingsSection 参与 Esc 分层判定，防 stale closure
  }, [leftOpen, settingsSection]);

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
  };

  const closeSelected = (): void => setSelectedKey(null);

  return (
    <div
      className={`layout ${leftOpen ? 'left-open' : ''} ${rightOpen ? 'right-open' : ''} ${leftCollapsed ? 'left-collapsed' : ''} ${selected ? 'has-session' : ''}`}
    >
      {/* 侧栏不可见时（桌面折叠/手机抽屉关闭）的展开入口 */}
      <button
        type="button"
        className="sidebar-expand"
        aria-label={t('shell.expandSidebar')}
        onClick={() => {
          setLeftCollapsed(false);
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
      <div className="layout-body">
        <aside className="pane-left" aria-label="Agents">
          <AgentList
            checked={checked}
            onClearFilter={() => setChecked(new Set())}
            onSelect={selectAgent}
            selectedId={selected?.id ?? null}
            onOpenSearch={() => setShowQuickChat(true)}
            onCollapse={() => {
              setLeftOpen(false); // 手机：收起抽屉
              setLeftCollapsed((v) => !v); // 桌面：切换折叠
            }}
            onOpenProjects={() => setRightOpen(true)}
            onOpenConfig={() => setShowAgentsCfg(true)}
            onOpenSettings={openSettings}
            onStart={() => setShowStart(true)}
            onRemoved={(id) => {
              // 复合键与单实例 id 兼容：选中的正是被移除 agent 则清选中
              if (selectedKey === id || selectedKey?.endsWith(`:${id}`)) setSelectedKey(null);
            }}
            onOpenNotes={setNotesFor}
          />
        </aside>
        <main className="pane-main">
          {selected != null ? (
            <ChatTab
              key={`${selected.instanceId ?? ''}:${selected.id}`}
              agent={selected}
              api={selectedApi ?? undefined}
              onClose={closeSelected}
              onOpenSidebar={() => setLeftOpen(true)}
              notesOpen={notesFor?.cwd === selected.cwd}
              onToggleNotes={() =>
                setNotesFor((cur) => (cur && cur.cwd === selected.cwd ? null : selected))
              }
            />
          ) : (
            <div className="pane-placeholder">
              <p>{t('shell.placeholder')}</p>
            </div>
          )}
        </main>
        <aside className="pane-right" aria-label="Projects">
          <ProjectSidebar
            agents={data}
            projects={projects}
            checked={checked}
            pending={pending}
            onPendingChange={setPending}
            onToggle={toggleProject}
            onAdd={addProject}
            onCollapse={() => setRightOpen(false)}
          />
        </aside>
      </div>
      {showStart ? <StartDialog onClose={() => setShowStart(false)} /> : null}
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
      {showAgentsCfg ? <AgentsConfigPanel onClose={() => setShowAgentsCfg(false)} /> : null}
      {settingsSection ? (
        <SettingsPage section={settingsSection} onClose={closeSettings} />
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
      {needToken ? <TokenGate onUnlocked={unlock} /> : null}
    </div>
  );
}
