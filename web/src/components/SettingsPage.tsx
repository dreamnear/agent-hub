import { useEffect, useState, type ReactElement } from 'react';
import HarnessPanel from './HarnessPanel';
import SettingsAppearance from './SettingsAppearance';
import SettingsDocRender from './SettingsDocRender';
import SettingsLanguage from './SettingsLanguage';
import SettingsPiHub from './SettingsPiHub';
import InstancesSection from './settings/InstancesSection';
import NodesSection from './settings/NodesSection';
import TasksSection from './settings/TasksSection';
import ProjectSidebar from './ProjectSidebar';
import AgentsConfigPanel from './AgentsConfigPanel';
import StartDialog from './StartDialog';
import HistoryDialog from './HistoryDialog';
import type { AgentSummary } from '../types';
import { useI18n } from '../i18n';
import './SettingsPage.css';

export type SettingsSectionKey =
  | 'appearance'
  | 'language'
  | 'docs'
  | 'instances'
  | 'nodes'
  | 'piHub'
  | 'harness'
  | 'tasks'
  | 'about'
  // 侧栏收敛五项（agent-hub-settings-consolidation S1/S2）：原独立入口全部并入分区
  | 'projects'
  | 'config'
  | 'newAgent'
  | 'history'
  // Hub 助手（agent-hub-pi-hub-agent P3/P5）：pi provider/model + 助手生命周期
  | 'piHub';

/// 上次访问分区（S4：默认分区 = 上次访问，持久化合理即可）
const LAST_SECTION_KEY = 'hub_settings_section';

/// 合法分区键表（lastSettingsSection 脏值校验用；与 SECTIONS 注册表同步维护）
const SECTION_KEYS: SettingsSectionKey[] = [
  'appearance',
  'language',
  'docs',
  'projects',
  'config',
  'newAgent',
  'history',
  'piHub',
  'instances',
  'harness',
  'tasks',
  'about',
];

export function lastSettingsSection(): SettingsSectionKey {
  const v = localStorage.getItem(LAST_SECTION_KEY);
  // 防御（reviewer MINOR）：脏值/越界枚举一律回默认，不外泄非法 key
  return v != null && (SECTION_KEYS as string[]).includes(v) ? (v as SettingsSectionKey) : 'appearance';
}

interface Props {
  /** 打开时落哪个分区（侧栏入口缺省落上次访问分区，S4） */
  section?: SettingsSectionKey;
  onClose: () => void;
  /** Run 记录 sessionId → 打开该会话（定时任务段；App 关设置页 + 选中） */
  onOpenSession?: (agentId: string) => void;
  /** 分区渲染上下文（收敛五分区所需数据/回调，App 接线） */
  ctx?: SettingsRenderCtx;
}

/// 构建期注入（vite.config.ts define，零服务端改动）；vitest / 无 define 环境回落 dev
const HUB_VERSION = typeof __HUB_VERSION__ === 'string' ? __HUB_VERSION__ : 'dev';
const HUB_GIT_SHA = typeof __HUB_GIT_SHA__ === 'string' ? __HUB_GIT_SHA__ : 'dev';

function SettingsAbout(): ReactElement {
  const t = useI18n();
  return (
    <section className="set-group">
      <div className="set-row">
        <span className="set-label">{t('settings.about.product')}</span>
        <span className="set-value">agent-hub（claude-view）</span>
      </div>
      <div className="set-row">
        <span className="set-label">{t('settings.about.version')}</span>
        <span className="set-value">{HUB_VERSION}</span>
      </div>
      <div className="set-row">
        <span className="set-label">{t('settings.about.build')}</span>
        <span className="set-value">{HUB_GIT_SHA}</span>
      </div>
    </section>
  );
}

/// 分区渲染上下文：需要 App 交互回调的分区取用，其余忽略
export interface SettingsRenderCtx {
  onOpenSession?: (agentId: string) => void;
  /** 工程白名单勾选与族锚（工程列表/历史会话两分区共用，App 持有） */
  checked?: ReadonlySet<string>;
  anchors?: string[];
  /** 工程列表分区（S2/S3）：侧栏同款增删/勾选过滤，状态 App 持有（右面板工程树共用） */
  projects?: {
    agents: AgentSummary[];
    projects: string[];
    pending: string;
    onPendingChange: (v: string) => void;
    onToggle: (path: string) => void;
    onAdd: () => void;
  };
  /** 历史会话分区（S3）：条目选中 = 关设置页 + 打开 chat（App 接线） */
  onOpenHistory?: (a: AgentSummary) => void;
  /** 新建 Agent 分区（S3）：创建成功 = 关设置页 + 选中新会话（App 接线） */
  onCreatedAgent?: (driver: string, id: string) => void;
  /** 历史会话取数函数（测试注入用，透传 HistoryDialog） */
  fetchHistory?: () => Promise<AgentSummary[]>;
}

/// 分区注册表（agent-hub-settings B1）：后续分区（D1 实例、C1 语言）按数组加一行
/// 挂载，不改编排代码。render 只负责内容，导航与容器由外壳统一管。
/// label 存 i18n key（C1/C2），渲染时经 useI18n 翻译——语言切换即时生效。
const SECTIONS: {
  key: SettingsSectionKey;
  labelKey: string;
  render: (ctx: SettingsRenderCtx) => ReactElement;
}[] = [
  { key: 'appearance', labelKey: 'settings.section.appearance', render: () => <SettingsAppearance /> },
  { key: 'language', labelKey: 'settings.section.language', render: () => <SettingsLanguage /> },
  { key: 'docs', labelKey: 'settings.section.docs', render: () => <SettingsDocRender /> },
  {
    key: 'projects',
    labelKey: 'menu.projects',
    render: (ctx) =>
      ctx.projects != null ? (
        <div className="set-group set-projects-section">
          <ProjectSidebar
            agents={ctx.projects.agents}
            projects={ctx.projects.projects}
            checked={ctx.checked ?? new Set()}
            pending={ctx.projects.pending}
            onPendingChange={ctx.projects.onPendingChange}
            onToggle={ctx.projects.onToggle}
            onAdd={ctx.projects.onAdd}
          />
        </div>
      ) : (
        <section className="set-group" />
      ),
  },
  { key: 'config', labelKey: 'menu.config', render: () => <AgentsConfigPanel onClose={() => {}} embedded /> },
  {
    key: 'newAgent',
    labelKey: 'menu.newAgent',
    render: (ctx) => <StartDialog onClose={undefined} onCreated={ctx.onCreatedAgent} embedded />,
  },
  {
    key: 'history',
    labelKey: 'menu.history',
    render: (ctx) => (
      <HistoryDialog
        onSelect={ctx.onOpenHistory ?? (() => {})}
        checked={ctx.checked ?? new Set()}
        anchors={ctx.anchors ?? []}
        fetchAll={ctx.fetchHistory}
        embedded
      />
    ),
  },
  { key: 'piHub', labelKey: 'settings.section.piHub', render: () => <SettingsPiHub /> },
  { key: 'instances', labelKey: 'settings.section.instances', render: () => <InstancesSection /> },
  { key: 'nodes', labelKey: 'settings.section.nodes', render: () => <NodesSection /> },
  { key: 'harness', labelKey: 'settings.section.harness', render: () => <HarnessPanel /> },
  { key: 'tasks', labelKey: 'settings.section.tasks', render: (ctx) => <TasksSection onOpenSession={ctx.onOpenSession} /> },
  { key: 'about', labelKey: 'settings.section.about', render: () => <SettingsAbout /> },
];

/// 设置页外壳：全屏容器 + 分区导航（桌面左列 / ≤768px 顶部分段控件）+ 关闭返回。
/// 作为覆盖层渲染在主界面之上——主界面不卸载，关闭后选中会话/抽屉/折叠原样保留（S4）。
export default function SettingsPage({
  section,
  onClose,
  onOpenSession,
  ctx,
}: Props): ReactElement {
  const t = useI18n();
  const [active, setActiveState] = useState<SettingsSectionKey>(section ?? 'appearance');
  // S4：分区切换持久化（下次开设置落上次访问的分区）
  const setActive = (k: SettingsSectionKey): void => {
    setActiveState(k);
    localStorage.setItem(LAST_SECTION_KEY, k);
  };
  const current = SECTIONS.find((s) => s.key === active) ?? SECTIONS[0];

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="settings" role="dialog" aria-modal="true" aria-label={t('settings.title')}>
      <header className="settings-head">
        <h1 className="settings-title">{t('settings.title')}</h1>
        <button
          type="button"
          className="btn-ghost settings-close"
          onClick={onClose}
          aria-label={t('settings.close')}
        >
          ✕
        </button>
      </header>
      <div className="settings-body">
        <nav className="settings-nav" aria-label={t('settings.nav')}>
          {SECTIONS.map((s) => (
            <button
              key={s.key}
              type="button"
              className={`settings-tab${s.key === current.key ? ' is-active' : ''}`}
              aria-current={s.key === current.key}
              onClick={() => setActive(s.key)}
            >
              {t(s.labelKey)}
            </button>
          ))}
        </nav>
        <section className="settings-content" aria-label={t(current.labelKey)}>
          {current.render({ onOpenSession, ...ctx })}
        </section>
      </div>
    </div>
  );
}
