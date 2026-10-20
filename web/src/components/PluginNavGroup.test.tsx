// @vitest-environment happy-dom
// 插件导航组（agent-hub-plugin-system B1）：零插件整组不渲染（BR-06 导航集合与
// 引入前一致）、故障徽标、入口点击回调携带完整条目。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import PluginNavGroup from './PluginNavGroup';
import type { PluginView } from '../plugins';

vi.mock('../plugins', async (importOriginal) => {
  const mod = await importOriginal<typeof import('../plugins')>();
  return {
    ...mod,
    plugins: { ...mod.plugins, list: vi.fn(async () => currentList) },
  };
});

let currentList: PluginView[] = [];

function pv(over: Partial<PluginView>): PluginView {
  return {
    id: 'demo-plugin',
    name: 'Demo',
    version: '0.1.0',
    source: { kind: 'drop-in', ref: null },
    enabled: true,
    state: 'ready',
    uiState: 'enabled',
    installedAt: '2026-10-19T00:00:00Z',
    contentHash: 'sha256:x',
    contributions: [],
    permissions: { api: [], data: [], credentials: [], highRisk: [] },
    views: [{ id: 'home', title: 'Home', order: 1 }],
    backend: { running: false, alwaysOn: false, idleTimeoutSecs: 300 },
    assetBytes: 0,
    ...over,
  };
}

function renderGroup(props: Partial<React.ComponentProps<typeof PluginNavGroup>> = {}): void {
  const qc = new QueryClient();
  qc.setQueryData(['plugins'], currentList);
  render(
    <QueryClientProvider client={qc}>
      <PluginNavGroup onOpenView={() => {}} onOpenManager={() => {}} {...props} />
    </QueryClientProvider>,
  );
}

afterEach(() => cleanup());

describe('PluginNavGroup', () => {
  it('零插件时整组不渲染（BR-06：导航集合与引入前一致）', () => {
    currentList = [];
    renderGroup();
    expect(document.querySelector('.plugin-nav')).toBeNull();
  });

  it('启用态插件出现入口；故障态保留入口 + 红色故障徽标（BR-07）', () => {
    currentList = [
      pv({ id: 'ok', name: 'OK', views: [{ id: 'h', title: '工作台', order: 1 }] }),
      pv({
        id: 'bad',
        name: 'Bad',
        uiState: 'fault',
        state: 'fault',
        views: [{ id: 'h', title: '故障视图', order: 2 }],
        lastFault: { at: '2026-10-19T00:00:00Z', kind: 'crash', message: '崩溃' },
      }),
    ];
    renderGroup();
    expect(screen.getByText('工作台')).toBeTruthy();
    expect(screen.getByText('故障视图')).toBeTruthy();
    expect(screen.getByText('故障')).toBeTruthy(); // 徽标文案
  });
});
