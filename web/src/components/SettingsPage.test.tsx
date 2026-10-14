// @vitest-environment happy-dom
// 设置页外壳（agent-hub-settings B1）：五分区渲染与切换、关闭回调、Esc 关闭、
// 外观分区接主题/字体（B2/B3 的设置页入口）。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import SettingsPage, { lastSettingsSection } from './SettingsPage';
import { setLangChoice } from '../i18n';

vi.mock('../api', () => ({
  api: {
    listHarnesses: () => Promise.resolve({ harnesses: [] }),
    listInstances: () => Promise.resolve([]),
    tunnelStatus: () => Promise.resolve({ running: false, localPort: null, state: 'not_started', retries: 0 }),
    listAgentConfigs: () => Promise.resolve([]), // 收敛 S2：config 分区（AgentsConfigPanel）
    listDrivers: () => Promise.resolve(['claude']), // 收敛 S2：newAgent 分区（StartDialog）
    listAcpAgents: () => Promise.resolve({ agents: [] }),
  },
  storeToken: vi.fn(),
}));
vi.mock('../instances', () => ({ fetchInstances: () => Promise.resolve([]) }));

afterEach(() => {
  cleanup();
  setLangChoice('zh'); // 语言用例切过 en 后恢复 zh，防污染后续用例（模块态跨用例存续）
  window.localStorage.clear();
  delete document.documentElement.dataset.font;
  delete document.documentElement.dataset.theme;
  document.documentElement.style.removeProperty('--ui-scale');
});

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const renderPage = (props: Partial<Parameters<typeof SettingsPage>[0]> = {}) =>
  render(
    <SettingsPage
      section={props.section}
      onClose={props.onClose ?? vi.fn()}
      ctx={props.ctx}
    />,
    { wrapper },
  );

describe('SettingsPage 外壳（B1）', () => {
  it('默认落外观分区，全部分区导航可见（收敛批：7 原生 + 5 收编）', () => {
    renderPage({ onClose: vi.fn() });
    expect(screen.getByRole('button', { name: '关闭设置' })).toBeTruthy();
    for (const label of ['外观', '语言', '工程列表', 'Agent 配置', '新建 Agent', '历史会话', '实例', 'Harness', '关于']) {
      expect(screen.getByRole('button', { name: label })).toBeTruthy();
    }
    expect(screen.getByLabelText('外观设置')).toBeTruthy();
  });

  it('section 入参指定初始分区（侧栏入口定向跳转用）', () => {
    renderPage({ section: 'about', onClose: vi.fn() });
    expect(screen.getByText('agent-hub（claude-view）')).toBeTruthy();
    expect(screen.getByLabelText('关于')).toBeTruthy();
  });

  it('分区切换：点「关于」→ 关于内容出现，外观内容消失', () => {
    renderPage({ onClose: vi.fn() });
    fireEvent.click(screen.getByRole('button', { name: '关于' }));
    expect(screen.getByText('agent-hub（claude-view）')).toBeTruthy();
    expect(screen.queryByLabelText('外观设置')).toBeNull();
  });

  it('关闭按钮与 Esc 都回调 onClose', () => {
    const onClose = vi.fn();
    const { unmount } = renderPage({ onClose });
    fireEvent.click(screen.getByRole('button', { name: '关闭设置' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    unmount();
    renderPage({ onClose });
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it('实例分区已迁入（D1）：列表-表单管理界面可见；语言分区三选可见（C1）', () => {
    renderPage({ onClose: vi.fn() });
    fireEvent.click(screen.getByRole('button', { name: '实例' }));
    expect(screen.getByRole('button', { name: '＋ 新建实例' })).toBeTruthy();
    expect(screen.getByLabelText('实例管理')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '语言' }));
    expect(screen.getByRole('button', { name: '跟随系统' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '中文' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'English' })).toBeTruthy();
  });

  it('语言切换即时生效并持久化（C1）：切 English 后语言分区文案变英文，localStorage 落盘', () => {
    renderPage({ onClose: vi.fn() });
    fireEvent.click(screen.getByRole('button', { name: '语言' }));
    fireEvent.click(screen.getByRole('button', { name: 'English' }));
    expect(window.localStorage.getItem('hub_locale')).toBe('en');
    expect(screen.getByLabelText('Language settings')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Follow system' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '中文' }));
    expect(window.localStorage.getItem('hub_locale')).toBe('zh');
    expect(screen.getByRole('button', { name: '跟随系统' })).toBeTruthy();
  });
});

describe('SettingsPage 外观分区（B2/B3 入口）', () => {
  it('主题三态按钮落 useTheme（持久化 + data-theme 生效）', () => {
    renderPage({ onClose: vi.fn() });
    // 字形 span 为 aria-hidden，可达名称只含文字
    fireEvent.click(screen.getByRole('button', { name: '暗色' }));
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(window.localStorage.getItem('hub_theme')).toBe('dark');
    fireEvent.click(screen.getByRole('button', { name: '亮色' }));
    expect(document.documentElement.dataset.theme).toBe('light');
  });

  it('字号档位按钮即时落 --ui-scale（B2 全局缩放入口）', () => {
    renderPage({ onClose: vi.fn() });
    fireEvent.click(screen.getByRole('button', { name: '150%' }));
    expect(document.documentElement.style.getPropertyValue('--ui-scale')).toBe('1.5');
    expect(window.localStorage.getItem('hub_font_scale')).toBe('1.5');
  });

  it('字体族下拉即时落 data-font（B2）', () => {
    renderPage({ onClose: vi.fn() });
    fireEvent.change(screen.getByLabelText('界面字体'), { target: { value: 'mono' } });
    expect(document.documentElement.dataset.font).toBe('mono');
    expect(window.localStorage.getItem('hub_font_family')).toBe('mono');
  });
});

describe('lastSettingsSection 脏值防御（settings MINOR 回修）', () => {
  it.each([
    ['bogus-key', 'appearance'],
    ['about', 'about'],
    ['projects', 'projects'],
  ])('hub_settings_section=%s → %s', (stored, expected) => {
    window.localStorage.setItem('hub_settings_section', stored);
    expect(lastSettingsSection()).toBe(expected);
  });
});

describe('设置页收敛五分区（agent-hub-settings-consolidation S2/S3/S4）', () => {
  const projectsCtx = {
    checked: new Set(['/repo']),
    anchors: ['/repo'],
    projects: {
      agents: [],
      projects: ['/repo'],
      pending: '',
      onPendingChange: vi.fn(),
      onToggle: vi.fn(),
      onAdd: vi.fn(),
    },
  };

  it('S2 工程列表分区：ProjectSidebar 内嵌（树可见、勾选过滤在位）', () => {
    const { container } = renderPage({ section: 'projects', ctx: projectsCtx });
    expect(container.querySelector('nav.sidebar[aria-label="Projects"]')).not.toBeNull();
    // 勾选过滤状态直通（checked 传入渲染）
    expect(container.querySelector('.settings-content')).not.toBeNull();
  });

  it('S2/S3 Agent 配置分区：embedded 卡片直接渲染（空清单提示可见）', async () => {
    const { container } = renderPage({ section: 'config' });
    await Promise.resolve(); // listAgentConfigs 微任务冲刷
    expect(container.querySelector('.agentscfg-card--embedded')).not.toBeNull();
    expect(container.querySelector('.agentscfg-backdrop')).toBeNull(); // 无遮罩（分区内嵌）
  });

  it('S2/S3 新建 Agent 分区：表单内嵌（driver 切换与启动钮在位）', async () => {
    const { container } = renderPage({ section: 'newAgent' });
    await Promise.resolve();
    expect(container.querySelector('.start-card--embedded')).not.toBeNull();
    expect(container.querySelector('.dialog-backdrop')).toBeNull();
    expect(screen.getByRole('button', { name: '启动' })).toBeTruthy();
  });

  it('S2/S3 历史会话分区：列表内嵌（fetchAll 注入、条目点击回抛 onOpenHistory）', async () => {
    const onOpenHistory = vi.fn();
    const { container } = renderPage({
      section: 'history',
      ctx: {
        checked: new Set(),
        anchors: [],
        onOpenHistory,
        fetchHistory: () =>
          Promise.resolve([
            {
              id: 'h1',
              driver: 'claude',
              group: 'completed',
              name: null,
              cwd: '/repo/h',
              kind: null,
              rawState: null,
              detail: null,
              tokens: null,
              context_tokens: null,
              startedAt: 1,
              sessionId: null,
              instanceId: null,
            },
          ]),
      },
    });
    await screen.findByText('/repo/h');
    expect(container.querySelector('.history-card--embedded')).not.toBeNull();
    fireEvent.click(container.querySelector('.history-list button') as HTMLButtonElement);
    expect(onOpenHistory).toHaveBeenCalledTimes(1);
  });

  it('S4 分区切换持久化：点「关于」后 hub_settings_section 落盘', () => {
    renderPage({ onClose: vi.fn() });
    fireEvent.click(screen.getByRole('button', { name: '关于' }));
    expect(window.localStorage.getItem('hub_settings_section')).toBe('about');
  });
});
