// @vitest-environment happy-dom
// 节点管理区测试（gateway-node-p3 M5）：配对列表渲染 + 待审批卡片动作 +
// 改名/轮换/撤销的 API 调用形态 + stats presence 如实显示（N-1 缺失 = —）。
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

import NodesSection from './NodesSection';


const pendingNode = {
  nodeId: 'hub-test-pending',
  nodeName: 'pending-node',
  status: 'pending',
  platform: 'linux-aarch64',
  drivers: ['claude'],
  createdAt: '1791600000',
  lastSeen: '1791600001',
  stats: null, // N-1：旧节点未上报
};
const approvedNode = {
  nodeId: 'hub-test-approved',
  nodeName: 'approved-node',
  status: 'approved',
  platform: 'darwin-aarch64',
  drivers: ['claude', 'dsh'],
  createdAt: '1791600000',
  lastSeen: '1791600002',
  stats: { os: 'darwin', arch: 'aarch64', version: '0.1.0', uptime_secs: 90061 }, // 1d 1h
};

let fetchMock: Mock;

function renderSection() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <NodesSection />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockImplementation((input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/api/nodes') && !url.includes('/')) {
      // 精确 /api/nodes（相对路径 fetch 会带 origin——用 includes 收敛）
    }
    if (/\/api\/nodes$/.test(url)) {
      return Promise.resolve(
        new Response(JSON.stringify({ pending: [pendingNode], approved: [approvedNode] }), {
          status: 200,
        }),
      );
    }
    return Promise.resolve(new Response('{}', { status: 200 }));
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('NodesSection', () => {
  it('renders pending and approved groups with stats presence', async () => {
    renderSection();
    expect(await screen.findByText('pending-node')).toBeTruthy();
    expect(screen.getByTestId('node-card-hub-test-pending')).toBeTruthy();
    expect(screen.getByTestId('node-card-hub-test-approved')).toBeTruthy();
    // stats presence：有 stats 的节点显示人化摘要（1d 1h）；旧节点（null）显示 —
    expect(screen.getByTestId('node-stats-hub-test-approved').textContent).toContain('1d 1h');
    expect(screen.getByTestId('node-stats-hub-test-pending').textContent).toBe('—');
  });

  it('approve posts to the node endpoint and refreshes', async () => {
    renderSection();
    const btn = await screen.findByTestId('approve-hub-test-pending');
    fireEvent.click(btn);
    await waitFor(() => {
      const calls = fetchMock.mock.calls.map((c) => String(c[0]));
      expect(calls.some((u) => u.endsWith('/api/nodes/hub-test-pending/approve'))).toBe(true);
    });
  });

  it('rename posts the new name', async () => {
    renderSection();
    const renameBtn = await screen.findByTestId('rename-hub-test-approved');
    fireEvent.click(renameBtn);
    const input = await screen.findByTestId('rename-input-hub-test-approved');
    fireEvent.change(input, { target: { value: '新名字' } });
    fireEvent.click(screen.getByText('保存'));
    await waitFor(() => {
      const posted = fetchMock.mock.calls.find(
        (c) => String(c[0]).endsWith('/rename') && (c[1]?.body as string)?.includes('新名字'),
      );
      expect(posted).toBeTruthy();
    });
  });

  it('rotate posts to the rotate endpoint', async () => {
    renderSection();
    const btn = await screen.findByTestId('rotate-hub-test-approved');
    fireEvent.click(btn);
    await waitFor(() => {
      const calls = fetchMock.mock.calls.map((c) => String(c[0]));
      expect(calls.some((u) => u.endsWith('/api/nodes/hub-test-approved/rotate'))).toBe(true);
    });
  });

  // sweep S1：轮换回滚（网关 {"error":"rotate_rolled_back"} 503）→ 专用 i18n
  // 文案（不透传服务端原文）；role=alert 供走查与可达性双用
  it('rotate rollback shows rolled-back notice', async () => {
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (/\/api\/nodes$/.test(url)) {
        return Promise.resolve(
          new Response(JSON.stringify({ pending: [], approved: [approvedNode] }), { status: 200 }),
        );
      }
      if (url.endsWith('/rotate')) {
        return Promise.resolve(
          new Response(JSON.stringify({ error: 'rotate_rolled_back' }), { status: 503 }),
        );
      }
      return Promise.resolve(new Response('{}', { status: 200 }));
    });
    renderSection();
    fireEvent.click(await screen.findByTestId('rotate-hub-test-approved'));
    const notice = await screen.findByTestId('nodes-notice');
    expect(notice.textContent).toContain('轮换失败已回滚，请重试');
  });
});
