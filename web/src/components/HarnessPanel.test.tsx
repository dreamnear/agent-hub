// @vitest-environment happy-dom
// harness 发现区（agent-hub-settings 批A 需求5/6）：死亡过滤（默认隐藏失效项）、
// 「显示全部」开关、ACP 项一键加入配置（调 addHarness 后刷新）。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import HarnessPanel from './HarnessPanel';
import type { HarnessEntry } from '../api';

const calls = vi.hoisted(() => ({
  listHarnesses: vi.fn((): Promise<{ harnesses: HarnessEntry[] }> => Promise.resolve({ harnesses: [] })),
  addHarness: vi.fn((_name: string, _path: string) => Promise.resolve({ added: true, name: _name })),
}));
vi.mock('../api', () => ({ api: calls }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const omp: HarnessEntry = { name: 'omp', path: '/Users/x/.bun/bin/omp', version: 'omp/18.0.11', alive: true, kind: 'acp', configured: false };
const deadOmp: HarnessEntry = { name: 'omp', path: '/nope/omp', version: null, alive: false, kind: 'acp', configured: true };
const claude: HarnessEntry = { name: 'claude', path: '/Users/x/.local/bin/claude', version: '2.1.283 (Claude Code)', alive: true, kind: 'cli', configured: false };

describe('HarnessPanel 死亡过滤（需求6）', () => {
  it('默认只显示存活项（路径 + 版本）', async () => {
    calls.listHarnesses.mockResolvedValue({ harnesses: [omp, deadOmp, claude] });
    render(<HarnessPanel />, { wrapper });
    await screen.findByText(/\.bun\/bin\/omp/);
    expect(screen.getByText('claude')).toBeTruthy();
    expect(screen.queryByText('/nope/omp')).toBeNull(); // 失效项默认隐藏
    expect(screen.queryByText('失效')).toBeNull();
  });

  it('「显示全部」开关切出失效项并带「失效」标记', async () => {
    calls.listHarnesses.mockResolvedValue({ harnesses: [omp, deadOmp] });
    render(<HarnessPanel />, { wrapper });
    await screen.findByText(/\.bun\/bin\/omp/);
    expect(screen.getByRole('button', { name: '显示全部（含失效 1）' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '显示全部（含失效 1）' }));
    expect(await screen.findByText('/nope/omp')).toBeTruthy();
    expect(screen.getByText('失效')).toBeTruthy();
    expect(screen.getByRole('button', { name: '仅显示可用' })).toBeTruthy();
  });

  it('失效项的加入按钮禁用（死亡过滤不因开关露出而解除）', async () => {
    calls.listHarnesses.mockResolvedValue({
      harnesses: [{ ...deadOmp, configured: false }],
    });
    render(<HarnessPanel />, { wrapper });
    fireEvent.click(await screen.findByRole('button', { name: '显示全部（含失效 1）' }));
    const btn = await screen.findByRole('button', { name: '加入配置' });
    expect((btn as HTMLButtonElement).disabled).toBe(true);
  });
});

describe('HarnessPanel 一键加入（需求5）', () => {
  it('ACP 存活项点「加入配置」→ addHarness(name, path) 并刷新清单', async () => {
    calls.listHarnesses.mockResolvedValueOnce({ harnesses: [omp] }).mockResolvedValueOnce({
      harnesses: [{ ...omp, configured: true }],
    });
    render(<HarnessPanel />, { wrapper });
    fireEvent.click(await screen.findByRole('button', { name: '加入配置' }));
    await waitFor(() => expect(calls.addHarness).toHaveBeenCalledWith('omp', omp.path));
    expect(await screen.findByText('已加入')).toBeTruthy(); // 刷新后按钮换已加入标记
  });

  it('已配置项不重复给加入按钮；CLI 项无加入动作（hub 走原生驱动）', async () => {
    calls.listHarnesses.mockResolvedValue({
      harnesses: [{ ...omp, configured: true }, claude],
    });
    render(<HarnessPanel />, { wrapper });
    await screen.findByText(/\.local\/bin\/claude/);
    expect(screen.queryByRole('button', { name: '加入配置' })).toBeNull();
    expect(screen.getAllByText('已加入').length).toBe(1);
  });

  it('服务端拒绝文案透出（如路径已失效 400）', async () => {
    calls.listHarnesses.mockResolvedValue({ harnesses: [omp] });
    calls.addHarness.mockRejectedValue(new Error('路径不存在或不可执行，拒绝加入配置'));
    render(<HarnessPanel />, { wrapper });
    fireEvent.click(await screen.findByRole('button', { name: '加入配置' }));
    expect(await screen.findByText(/拒绝加入配置/)).toBeTruthy();
  });
});
