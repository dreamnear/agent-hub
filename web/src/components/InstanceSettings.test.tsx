// @vitest-environment happy-dom
// 实例管理面板（agent-hub-multi-instance 任务9）：direct/ssh-tunnel 两型 CRUD 表单、
// URL 红线拒绝文案透出（服务端 400 纯文本直显）、隧道启停按钮。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import InstanceSettings from './InstanceSettings';
import type { InstanceConfig } from '../types';

const calls = vi.hoisted(() => ({
  listInstances: vi.fn((): Promise<InstanceConfig[]> => Promise.resolve([])),
  createInstance: vi.fn((c: InstanceConfig) => Promise.resolve(c)),
  updateInstance: vi.fn((c: InstanceConfig) => Promise.resolve(c)),
  deleteInstance: vi.fn((): Promise<void> => Promise.resolve()),
  tunnelStart: vi.fn(() => Promise.resolve({ localPort: 45678 })),
  tunnelStop: vi.fn(() => Promise.resolve({ stopped: true })),
  tunnelStatus: vi.fn(() => Promise.resolve({ running: false, localPort: null, state: 'not_started', retries: 0 })),
}));
vi.mock('../api', () => ({ api: calls, storeToken: vi.fn() }));
vi.mock('../instances', () => ({ fetchInstances: () => calls.listInstances() }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const renderPanel = () =>
  render(<InstanceSettings onClose={vi.fn()} />, { wrapper });

const existing: InstanceConfig = {
  id: 'frp-1',
  name: 'frp 主机',
  mode: 'direct',
  url: 'https://hub.example.com',
  token: 'saved-tok',
  ssh: null,
  remotePort: null,
  localPort: null,
};

describe('InstanceSettings 列表', () => {
  it('列出实例（名称 + 连接方式 + url）', async () => {
    calls.listInstances.mockResolvedValue([existing]);
    renderPanel();
    await screen.findByText('frp 主机');
    expect(screen.getByText('direct')).toBeTruthy();
    expect(screen.getByText('https://hub.example.com')).toBeTruthy();
  });

  it('删除实例：确认后调 deleteInstance（服务端删除先 stop 隧道）', async () => {
    calls.listInstances.mockResolvedValue([existing]);
    vi.stubGlobal('confirm', vi.fn(() => true));
    renderPanel();
    await screen.findByText('frp 主机');
    fireEvent.click(screen.getByRole('button', { name: '删除' }));
    await waitFor(() => expect(calls.deleteInstance).toHaveBeenCalledWith('frp-1'));
  });
});

describe('InstanceSettings direct 表单', () => {
  it('新建 direct 实例：填名称/URL/token → createInstance 带表单值', async () => {
    renderPanel();
    fireEvent.click(screen.getByRole('button', { name: '＋ 新建实例' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: '远程机' } });
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'https://a.example.com' } });
    fireEvent.change(screen.getByLabelText('Token'), { target: { value: 'tok-1' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(calls.createInstance).toHaveBeenCalledTimes(1));
    const arg = calls.createInstance.mock.calls[0][0] as InstanceConfig;
    expect(arg.name).toBe('远程机');
    expect(arg.url).toBe('https://a.example.com');
    expect(arg.token).toBe('tok-1');
    expect(arg.mode).toBe('direct');
  });

  it('URL 红线：服务端 400 纯文本（非 https 拒绝）直接展示给用户', async () => {
    calls.createInstance.mockRejectedValueOnce(
      new Error('远程实例必须使用 https（frp 入口明文裸奔公网不可接受；本机可用 http）'),
    );
    renderPanel();
    fireEvent.click(screen.getByRole('button', { name: '＋ 新建实例' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: '明文机' } });
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'http://hub.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await screen.findByText(/远程实例必须使用 https/);
  });
});

describe('InstanceSettings ssh-tunnel 表单', () => {
  const fillTunnel = (): void => {
    fireEvent.click(screen.getByRole('button', { name: '＋ 新建实例' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'ssh 机' } });
    fireEvent.change(screen.getByLabelText('连接方式'), { target: { value: 'ssh-tunnel' } });
    fireEvent.change(screen.getByLabelText('SSH 主机'), { target: { value: 'example.com' } });
    fireEvent.change(screen.getByLabelText('用户名'), { target: { value: 'demo' } });
    fireEvent.change(screen.getByLabelText('远程目标端口'), { target: { value: '7800' } });
  };

  it('证书路径认证：填 keyPath → createInstance 带完整 ssh 配置', async () => {
    renderPanel();
    fillTunnel();
    fireEvent.change(screen.getByLabelText('证书路径'), { target: { value: '~/.ssh/id_ed25519' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(calls.createInstance).toHaveBeenCalledTimes(1));
    const arg = calls.createInstance.mock.calls[0][0] as InstanceConfig;
    expect(arg.mode).toBe('ssh-tunnel');
    expect(arg.ssh).toMatchObject({ host: 'example.com', user: 'demo', auth: 'key_path', keyPath: '~/.ssh/id_ed25519' });
    expect(arg.remotePort).toBe(7800);
  });

  it('密码认证可保存（sshpass 缺失报错在隧道 start 时透出）', async () => {
    renderPanel();
    fillTunnel();
    fireEvent.change(screen.getByLabelText('认证方式'), { target: { value: 'password' } });
    fireEvent.change(screen.getByLabelText('密码'), { target: { value: 'secret' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(calls.createInstance).toHaveBeenCalledTimes(1));
    const arg = calls.createInstance.mock.calls[0][0] as InstanceConfig;
    expect(arg.ssh?.auth).toBe('password');
  });

  it('隧道启停：未运行 → 启动（tunnelStart）；运行中 → 停止（tunnelStop）', async () => {
    calls.listInstances.mockResolvedValue([
      { ...existing, id: 's1', mode: 'ssh-tunnel', url: null, ssh: { host: 'example.com', port: 22, user: 'demo', auth: 'authsock', keyPath: null, password: null }, remotePort: 7800 },
    ]);
    renderPanel();
    const startBtn = await screen.findByRole('button', { name: '启动隧道' });
    fireEvent.click(startBtn);
    await waitFor(() => expect(calls.tunnelStart).toHaveBeenCalledWith('s1'));
  });
});
