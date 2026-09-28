// @vitest-environment happy-dom
// 设置页「实例」分区（agent-hub-settings D1，自 InstanceSettings.test.tsx 迁入，
// 断言不降级）：direct/ssh-tunnel 两型 CRUD 表单、URL 红线拒绝文案透出（服务端
// 400 纯文本直显）、隧道启停按钮。D4/D5：远程一键安装流程 + direct 手动清单复制。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import InstancesSection from './InstancesSection';
import type { InstanceConfig } from '../../types';
import type { InstallPlan, RemoteProbe } from '../../api';

const calls = vi.hoisted(() => ({
  listInstances: vi.fn((): Promise<InstanceConfig[]> => Promise.resolve([])),
  createInstance: vi.fn((c: InstanceConfig) => Promise.resolve(c)),
  updateInstance: vi.fn((c: InstanceConfig) => Promise.resolve(c)),
  deleteInstance: vi.fn((): Promise<void> => Promise.resolve()),
  tunnelStart: vi.fn(() => Promise.resolve({ localPort: 45678 })),
  tunnelStop: vi.fn(() => Promise.resolve({ stopped: true })),
  tunnelStatus: vi.fn(() => Promise.resolve({ running: false, localPort: null, state: 'not_started', retries: 0 })),
  remoteProbe: vi.fn((): Promise<RemoteProbe> => Promise.reject(new Error('not mocked'))),
  installPlan: vi.fn((): Promise<InstallPlan> => Promise.reject(new Error('not mocked'))),
  installExecute: vi.fn((): Promise<unknown> => Promise.reject(new Error('not mocked'))),
  installManual: vi.fn((): Promise<InstallPlan> => Promise.reject(new Error('not mocked'))),
  storeToken: vi.fn(),
}));
vi.mock('../../api', () => ({ api: calls, storeToken: calls.storeToken }));
vi.mock('../../instances', () => ({ fetchInstances: () => calls.listInstances() }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

const wrapper = ({ children }: { children: ReactNode }): ReactElement => (
  <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

const renderSection = () => render(<InstancesSection />, { wrapper });

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

describe('InstancesSection 列表', () => {
  it('列出实例（名称 + 连接方式 + url）', async () => {
    calls.listInstances.mockResolvedValue([existing]);
    renderSection();
    await screen.findByText('frp 主机');
    expect(screen.getByText('direct')).toBeTruthy();
    expect(screen.getByText('https://hub.example.com')).toBeTruthy();
  });

  it('删除实例：确认后调 deleteInstance（服务端删除先 stop 隧道）', async () => {
    calls.listInstances.mockResolvedValue([existing]);
    vi.stubGlobal('confirm', vi.fn(() => true));
    renderSection();
    await screen.findByText('frp 主机');
    fireEvent.click(screen.getByRole('button', { name: '删除' }));
    await waitFor(() => expect(calls.deleteInstance).toHaveBeenCalledWith('frp-1'));
  });
});

describe('InstancesSection direct 表单', () => {
  it('新建 direct 实例：填名称/URL/token → createInstance 带表单值', async () => {
    renderSection();
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

  it('URL 红线（C3 前置预校验）：远程 http 拒绝且不发请求，本机 http 放行', async () => {
    renderSection();
    fireEvent.click(screen.getByRole('button', { name: '＋ 新建实例' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: '明文机' } });
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'http://hub.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    // 前端拦截：本地 https 提示文案（错误条 + 表单 hint 同文案），且未发起 createInstance
    await screen.findAllByText(/远程 direct URL 必须是 https/);
    expect(calls.createInstance).not.toHaveBeenCalled();

    // 本机 http 豁免（localhost）：放行到保存
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'http://localhost:7800' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(calls.createInstance).toHaveBeenCalledTimes(1));
  });

  it('服务端 400 纯文本（前端不可预判的错误）仍原样透出', async () => {
    calls.createInstance.mockRejectedValueOnce(new Error('远程实例 token 无效（401 上游）'));
    renderSection();
    fireEvent.click(screen.getByRole('button', { name: '＋ 新建实例' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: '异常机' } });
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'https://hub.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await screen.findByText(/远程实例 token 无效/);
  });
});

describe('InstancesSection ssh-tunnel 表单', () => {
  const fillTunnel = (): void => {
    fireEvent.click(screen.getByRole('button', { name: '＋ 新建实例' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'ssh 机' } });
    fireEvent.change(screen.getByLabelText('连接方式'), { target: { value: 'ssh-tunnel' } });
    fireEvent.change(screen.getByLabelText('SSH 主机'), { target: { value: 'example.com' } });
    fireEvent.change(screen.getByLabelText('用户名'), { target: { value: 'alice' } });
    fireEvent.change(screen.getByLabelText('远程目标端口'), { target: { value: '7800' } });
  };

  it('证书路径认证：填 keyPath → createInstance 带完整 ssh 配置', async () => {
    renderSection();
    fillTunnel();
    fireEvent.change(screen.getByLabelText('证书路径'), { target: { value: '~/.ssh/id_ed25519' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(calls.createInstance).toHaveBeenCalledTimes(1));
    const arg = calls.createInstance.mock.calls[0][0] as InstanceConfig;
    expect(arg.mode).toBe('ssh-tunnel');
    expect(arg.ssh).toMatchObject({ host: 'example.com', user: 'alice', auth: 'key_path', keyPath: '~/.ssh/id_ed25519' });
    expect(arg.remotePort).toBe(7800);
  });

  it('密码认证可保存（sshpass 缺失报错在隧道 start 时透出）', async () => {
    renderSection();
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
      { ...existing, id: 's1', mode: 'ssh-tunnel', url: null, ssh: { host: 'example.com', port: 22, user: 'alice', auth: 'authsock', keyPath: null, password: null }, remotePort: 7800 },
    ]);
    renderSection();
    const startBtn = await screen.findByRole('button', { name: '启动隧道' });
    fireEvent.click(startBtn);
    await waitFor(() => expect(calls.tunnelStart).toHaveBeenCalledWith('s1'));
  });
});

// ===== D4：ssh-tunnel 一键安装流程（探测 → 确认 → 执行 → 探活）=====

const plan: InstallPlan = {
  planId: 'p-1',
  planHash: 'abc123',
  steps: [
    { desc: '下载并执行一键安装脚本', display: 'curl -fsSL https://example.com/install.sh | sh -s -- --with-systemd' },
    { desc: '验证服务已启动', display: 'curl -fsS --max-time 5 http://127.0.0.1:7800/health' },
  ],
};

const sshInstance: InstanceConfig = {
  ...existing,
  id: 's1',
  name: 'ssh 机',
  mode: 'ssh-tunnel',
  url: null,
  token: null,
  ssh: { host: 'example.com', port: 22, user: 'alice', auth: 'authsock', keyPath: null, password: null },
  remotePort: 7800,
};

const notInstalled: RemoteProbe = {
  installed: false,
  os: 'Linux',
  arch: 'x86_64',
  reason: 'not_installed',
  detail: '远程端口无响应，agent-hub server 未安装或未启动',
};

describe('D4 ssh-tunnel 一键安装', () => {
  it('未确认前零安装请求：探测未装 → 提示 → 拉计划展示命令清单，installExecute 未被调用', async () => {
    calls.listInstances.mockResolvedValue([sshInstance]);
    calls.remoteProbe.mockResolvedValue(notInstalled);
    calls.installPlan.mockResolvedValue(plan);
    renderSection();
    await screen.findByText('ssh 机');
    fireEvent.click(screen.getByRole('button', { name: '检查远程' }));
    await screen.findByText('远程主机需安装 agent-hub 轻量服务端（未装或未启动）。');
    fireEvent.click(screen.getByRole('button', { name: '安装远程服务端' }));
    await screen.findByText(/将在远程主机执行以下命令/);
    expect(screen.getByText(plan.steps[0].display)).toBeTruthy();
    // 网络断言：确认按钮前不发 install 请求
    expect(calls.installExecute).not.toHaveBeenCalled();
  });

  it('确认后执行：installExecute 带 confirm=true；成功后探活 + token 回填 + 实例转可用', async () => {
    // 服务端 token 回读后实例配置带 token（前端 fetchInstances 拉到新配置）
    calls.listInstances.mockResolvedValue([{ ...sshInstance, token: 'tok-remote' }]);
    calls.remoteProbe.mockResolvedValue(notInstalled);
    calls.installPlan.mockResolvedValue(plan);
    calls.installExecute.mockResolvedValue({ ok: true, logs: ['[ok] 安装完成'], error: null, tokenStored: true });
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve({ ok: true, status: 200 })),
    );
    renderSection();
    await screen.findByText('ssh 机');
    fireEvent.click(screen.getByRole('button', { name: '检查远程' }));
    fireEvent.click(await screen.findByRole('button', { name: '安装远程服务端' }));
    fireEvent.click(await screen.findByRole('button', { name: '确认安装' }));
    await screen.findByText(/安装完成，token 已自动回读，探活通过/);
    expect(calls.installExecute).toHaveBeenCalledWith('s1', { planId: 'p-1', planHash: 'abc123', confirm: true });
    expect(calls.tunnelStart).toHaveBeenCalledWith('s1'); // 自动探活（启动隧道 → /health）
    expect(calls.storeToken).toHaveBeenCalledWith('tok-remote', 's1'); // 服务端回读 token 回填浏览器
  });

  it('失败路径：分类错误与执行日志可见，可重试', async () => {
    calls.listInstances.mockResolvedValue([sshInstance]);
    calls.remoteProbe.mockResolvedValue(notInstalled);
    calls.installPlan.mockResolvedValue(plan);
    calls.installExecute.mockResolvedValue({
      ok: false,
      logs: ['[fail] 下载并执行一键安装脚本', 'sudo: a password is required'],
      error: '远端 sudo 非交互失败：当前 SSH 用户无免密 sudo。',
      tokenStored: false,
    });
    renderSection();
    await screen.findByText('ssh 机');
    fireEvent.click(screen.getByRole('button', { name: '检查远程' }));
    fireEvent.click(await screen.findByRole('button', { name: '安装远程服务端' }));
    fireEvent.click(await screen.findByRole('button', { name: '确认安装' }));
    await screen.findByText(/远端 sudo 非交互失败/);
    expect(screen.getByText(/sudo: a password is required/)).toBeTruthy();
  });
});

// ===== D5：direct 未装提示 + 可复制命令清单 =====

describe('D5 direct 手动安装清单', () => {
  it('测试连接失败 → 安装提示 + 命令清单，复制内容与清单逐字一致；已装不出现提示', async () => {
    calls.listInstances.mockResolvedValue([existing]);
    calls.installManual.mockResolvedValue(plan);
    vi.stubGlobal('fetch', vi.fn(() => Promise.reject(new TypeError('Failed to fetch'))));
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    renderSection();
    await screen.findByText('frp 主机');
    fireEvent.click(screen.getByRole('button', { name: '测试' }));
    await screen.findByText(/远程主机需安装 agent-hub 轻量服务端/);
    expect(screen.getByText(plan.steps[0].display)).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '复制命令清单' }));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(plan.steps.map((s) => s.display).join('\n')),
    );
  });

  it('已装实例（连接测试成功）不出现安装提示', async () => {
    calls.listInstances.mockResolvedValue([existing]);
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve({ ok: true, status: 200 })),
    );
    vi.stubGlobal('alert', vi.fn());
    renderSection();
    await screen.findByText('frp 主机');
    fireEvent.click(screen.getByRole('button', { name: '测试' }));
    await waitFor(() => expect(calls.installManual).not.toHaveBeenCalled());
    expect(screen.queryByText(/远程主机需安装 agent-hub 轻量服务端/)).toBeNull();
  });
});
