// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AgentSummary } from '../types';
import HistoryDialog from './HistoryDialog';

function mk(
  id: string,
  driver: string,
  group: AgentSummary['group'],
  cwd = `/repo/${id}`,
  startedAt = 1790831543,
): AgentSummary {
  return {
    id,
    driver,
    group,
    name: `会话 ${id}`,
    cwd,
    kind: driver === 'opencode' ? 'opencode' : driver,
    rawState: group === 'working' ? 'working' : group === 'completed' ? 'idle' : 'working',
    detail: null,
    tokens: null,
  context_tokens: null,
    startedAt,
    sessionId: id,
    instanceId: null,
  };
}

const ANCHORS = ['/repo'];

function renderHistory(
  fetchAll: () => Promise<AgentSummary[]>,
  onSelect: (a: AgentSummary) => void = vi.fn(),
  onClose: () => void = vi.fn(),
  checked: ReadonlySet<string> = new Set(ANCHORS),
): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <HistoryDialog
        onSelect={onSelect}
        onClose={onClose}
        checked={checked}
        anchors={ANCHORS}
        fetchAll={fetchAll}
      />
    </QueryClientProvider>,
  );
}

function itemButtons(): HTMLElement[] {
  return screen
    .getAllByRole('button')
    .filter((b) => b.className === 'history-item');
}

afterEach(cleanup);

describe('HistoryDialog（2026-10-02 全驱动历史面板）', () => {
  it('收编全驱动非活跃会话（claude Done + opencode 历史），working 不进', async () => {
    const fetchAll = vi.fn().mockResolvedValue([
      mk('oc-hist', 'opencode', 'completed', '/repo/oc-hist', 1790831500),
      mk('claude-done', 'claude', 'completed', '/repo/claude-done', 1790831600),
      mk('live', 'opencode', 'working'), // 活跃 turn：在侧栏，不进历史
      mk('claude-live', 'claude', 'working'), // claude 活会话：侧栏承担
    ]);
    renderHistory(fetchAll);
    await waitFor(() => expect(itemButtons().length).toBe(2));
    expect(screen.getByText('会话 oc-hist')).toBeTruthy();
    expect(screen.getByText('会话 claude-done')).toBeTruthy();
    expect(screen.queryByText('会话 live')).toBeNull();
    expect(screen.queryByText('会话 claude-live')).toBeNull();
    // 驱动 monogram 徽标（DriverBadge：单字母 + token 配色）
    expect(screen.getByText('O')).toBeTruthy();
    expect(screen.getByText('C')).toBeTruthy();
  });

  it('合并按最近活动倒序（claude 混入后统一重排）', async () => {
    const fetchAll = vi.fn().mockResolvedValue([
      mk('older-oc', 'opencode', 'completed', '/repo/a', 1000),
      mk('newer-claude', 'claude', 'completed', '/repo/b', 2000),
    ]);
    renderHistory(fetchAll);
    await waitFor(() => expect(itemButtons().length).toBe(2));
    const names = itemButtons().map((b) => b.textContent ?? '');
    expect(names[0]).toContain('newer-claude');
    expect(names[1]).toContain('older-oc');
  });

  it('白名单族锚过滤对 claude 历史条目同样适用（修正4）', async () => {
    const fetchAll = vi.fn().mockResolvedValue([
      mk('in-family', 'claude', 'completed', '/repo/in-family'),
      mk('stray', 'claude', 'completed', '/private/tmp/scratch'),
      mk('oc-stray', 'opencode', 'completed', '/Users/kevin/misc'),
    ]);
    renderHistory(fetchAll);
    await waitFor(() => expect(itemButtons().length).toBe(1));
    expect(screen.getByText('会话 in-family')).toBeTruthy();
    expect(screen.queryByText('会话 stray')).toBeNull();
    expect(screen.queryByText('会话 oc-stray')).toBeNull();
  });

  it('点条目回抛 onSelect 并关闭（claude Done 走 respawn 复活入口）', async () => {
    const onSelect = vi.fn();
    const onClose = vi.fn();
    const fetchAll = vi.fn().mockResolvedValue([mk('claude-done', 'claude', 'completed')]);
    renderHistory(fetchAll, onSelect, onClose);
    await waitFor(() => expect(screen.getByText('会话 claude-done')).toBeTruthy());
    fireEvent.click(screen.getByText('会话 claude-done'));
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'claude-done', driver: 'claude' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('空态文案（无历史时）', async () => {
    const fetchAll = vi.fn().mockResolvedValue([]);
    renderHistory(fetchAll);
    await waitFor(() => expect(screen.getByText('暂无历史会话')).toBeTruthy());
  });
});
