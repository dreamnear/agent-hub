// @vitest-environment happy-dom
// 插件管理页（agent-hub-plugin-system B1）：空态（BR-06 说明）、五态芯片、
// 停用卡操作、凭据按名零回显。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import PluginManagerPage from './PluginManagerPage';
import type { DiscoveredItem, PluginView } from '../plugins';

/// #46 git 直装 mock（供上行断言；实现按用例注入）
const installGitMock = vi.hoisted(() => vi.fn());

vi.mock('../plugins', async (importOriginal) => {
  const mod = await importOriginal<typeof import('../plugins')>();
  return {
    ...mod,
    plugins: {
      ...mod.plugins,
      list: vi.fn(async () => currentList),
      discovered: vi.fn(async () => ({ items: currentDiscovered, lastScanAt: '2026-10-19T00:00:00Z' })),
      scan: vi.fn(async () => ({ lastScanAt: '2026-10-19T00:00:00Z', discovered: 0 })),
      records: vi.fn(async () => ({ records: [] })),
      market: vi.fn(async () => ({ sources: [], entries: [] })),
      installGit: (...a: unknown[]) => installGitMock(...a),
    },
  };
});

let currentList: PluginView[] = [];
let currentDiscovered: DiscoveredItem[] = [];

function pv(over: Partial<PluginView>): PluginView {
  return {
    id: 'demo-plugin',
    name: 'Demo Workbench',
    version: '0.1.0',
    source: { kind: 'drop-in', ref: null },
    enabled: true,
    state: 'ready',
    uiState: 'enabled',
    installedAt: '2026-10-19T00:00:00Z',
    contentHash: 'sha256:x',
    contributions: [
      { kind: 'view', id: 'home', name: 'Home', risk: '-', path: 'view/index.html' },
    ],
    permissions: {
      api: [],
      data: ['projects:read'],
      credentials: [{ name: 'demo_pat', configured: false }],
      highRisk: [],
    },
    views: [{ id: 'home', title: 'Home', order: 1 }],
    backend: { running: false, alwaysOn: false, idleTimeoutSecs: 300 },
    assetBytes: 1234,
    ...over,
  };
}

function renderPage(): void {
  const qc = new QueryClient();
  qc.setQueryData(['plugins'], currentList);
  qc.setQueryData(['plugins', 'discovered'], { items: currentDiscovered, lastScanAt: '2026-10-19T00:00:00Z' });
  render(
    <QueryClientProvider client={qc}>
      <PluginManagerPage />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('PluginManagerPage', () => {
  it('零插件时显示 BR-06 空态说明，无安装清单', async () => {
    currentList = [];
    currentDiscovered = [];
    renderPage();
    await waitFor(() => expect(screen.getByText(/没有已安装插件/)).toBeTruthy());
  });

  it('已安装卡：芯片 + 凭据按名（值零回显）+ 停用/卸载按钮', async () => {
    currentList = [pv({})];
    currentDiscovered = [];
    renderPage();
    await waitFor(() => expect(screen.getByText('Demo Workbench')).toBeTruthy());
    expect(screen.getByText('启用中')).toBeTruthy();
    expect(screen.getByText(/demo_pat/)).toBeTruthy();
    expect(screen.getByText(/未配置/)).toBeTruthy();
    expect(screen.getByText('停用')).toBeTruthy();
    expect(screen.getByText('卸载')).toBeTruthy();
    expect(screen.getByText('查看记录')).toBeTruthy();
    const html = document.body.textContent ?? '';
    expect(html).not.toContain('credential-value'); // 零回显纪律
  });

  it('故障卡：红字原因 + 重试加载（不再显示停用）', async () => {
    currentList = [
      pv({
        uiState: 'fault',
        state: 'fault',
        lastFault: { at: '2026-10-19T00:00:00Z', kind: 'crash', message: '崩溃：backend 退出' },
      }),
    ];
    renderPage();
    await waitFor(() => expect(screen.getByText('故障')).toBeTruthy());
    expect(screen.getByText('崩溃：backend 退出')).toBeTruthy();
    expect(screen.getByText('重试加载')).toBeTruthy();
  });

  it('审计失败卡：可定位错因 + 无安装按钮', async () => {
    currentList = [];
    currentDiscovered = [
      {
        id: 'broken',
        dirName: 'broken',
        path: '/tmp/plugins/broken',
        sourceKind: 'drop-in',
        status: 'audit_failed',
        report: {
          pluginId: 'broken',
          pluginDir: '/tmp/plugins/broken',
          contentHash: 'sha256:bad',
          contributions: [],
          reach: { api: [], data: [], credentials: [], highRisk: [], mcp: [] },
          errors: [{ field: 'id', rule: 'id-format', message: 'id 必须是 kebab-slug' }],
          warnings: [],
          assetBytes: 0,
          assetFiles: 0,
          installable: false,
        },
      },
    ];
    renderPage();
    await waitFor(() => expect(screen.getByText(/审计失败/)).toBeTruthy());
    expect(screen.getByText(/id 必须是 kebab-slug/)).toBeTruthy();
    expect(screen.queryByText('安装并启用')).toBeNull();
    expect(screen.getByText('重新扫描')).toBeTruthy();
  });

  // #46 git 直装自由 URL 入口：合法 https URL → installGit 上行 + 确认面提示
  it('#46 git 直装：合法 https URL 上行 installGit 并提示进入确认面', async () => {
    currentList = [];
    currentDiscovered = [];
    installGitMock.mockResolvedValue({
      id: 'demo-r',
      path: '/tmp/agent-hub/plugins/demo-r',
      status: 'new',
      report: {
        pluginId: 'demo-r',
        pluginDir: 'demo-r',
        contentHash: 'sha256:x',
        contributions: [],
        reach: { api: [], data: [], credentials: [], highRisk: [], mcp: [] },
        errors: [],
        warnings: [],
        assetBytes: 0,
        assetFiles: 0,
        installable: true,
      },
    });
    renderPage();
    fireEvent.change(await screen.findByLabelText(/git 直装/), {
      target: { value: 'https://example.com/demo.git' },
    });
    fireEvent.click(screen.getByRole('button', { name: '获取并安装' }));
    await waitFor(() =>
      expect(installGitMock).toHaveBeenCalledWith('https://example.com/demo.git'),
    );
    await waitFor(() => expect(screen.getByText(/已进入安装确认面/)).toBeTruthy());
  });

  // #46：非法形态前端预检拒绝（不上行），可读提示；本机绝对路径放行
  it('#46 git 直装：http:// 形态被拒且有可读提示（不上行）', async () => {
    currentList = [];
    currentDiscovered = [];
    renderPage();
    fireEvent.change(await screen.findByLabelText(/git 直装/), {
      target: { value: 'http://example.com/demo.git' },
    });
    fireEvent.click(screen.getByRole('button', { name: '获取并安装' }));
    await waitFor(() =>
      expect(screen.getByText(/仅支持 https:\/\/ 仓库或本机绝对路径/)).toBeTruthy(),
    );
    expect(installGitMock).not.toHaveBeenCalled();
  });

  it('#46 git 直装：本机绝对路径放行 installGit', async () => {
    currentList = [];
    currentDiscovered = [];
    installGitMock.mockResolvedValue({
      id: 'local-r',
      path: '/tmp/agent-hub/plugins/local-r',
      status: 'new',
      report: {
        pluginId: 'local-r',
        pluginDir: 'local-r',
        contentHash: 'sha256:x',
        contributions: [],
        reach: { api: [], data: [], credentials: [], highRisk: [], mcp: [] },
        errors: [],
        warnings: [],
        assetBytes: 0,
        assetFiles: 0,
        installable: true,
      },
    });
    renderPage();
    fireEvent.change(await screen.findByLabelText(/git 直装/), {
      target: { value: '/tmp/repos/demo-plugin' },
    });
    fireEvent.click(screen.getByRole('button', { name: '获取并安装' }));
    await waitFor(() => expect(installGitMock).toHaveBeenCalledWith('/tmp/repos/demo-plugin'));
  });
});
