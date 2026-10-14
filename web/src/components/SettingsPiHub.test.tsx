// @vitest-environment happy-dom
// Hub 助手设置分区（agent-hub-pi-hub-agent P3/P5）：回填/编辑/保存调用/重启调用。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import SettingsPiHub from './SettingsPiHub';

const getPiSettingsMock = vi.fn();
const getPiVersionMock = vi.fn();
const putPiSettingsMock = vi.fn();
const startAgentMock = vi.fn();
const getPiRestartStatusMock = vi.fn();

vi.mock('../api', () => ({
  api: {
    getPiSettings: () => getPiSettingsMock(),
    getPiVersion: () => getPiVersionMock(),
    putPiSettings: (b: { provider?: string; model?: string }) => putPiSettingsMock(b),
    startAgent: (b: unknown, driver?: string) => startAgentMock(b, driver),
    getPiRestartStatus: () => getPiRestartStatusMock(),
  },
}));

vi.mock('../i18n', () => ({
  // restart-feedback：t 需支持参数插值断言（失败原因摘要经 {reason} 进文案）
  useI18n: () => (k: string, params?: Record<string, unknown>) =>
    params ? `${k}:${JSON.stringify(params)}` : k,
}));

function renderEl(): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <SettingsPiHub />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  getPiSettingsMock.mockReset().mockResolvedValue({
    provider: 'anthropic',
    model: 'claude-opus-4-6',
    thinking: null,
    providerConfig: null,
    allowSessionInject: false,
  });
  getPiVersionMock.mockReset().mockResolvedValue({ version: '0.73.1' });
  putPiSettingsMock.mockReset().mockImplementation(async (b: { provider?: string; model?: string }) => ({
    provider: b.provider ?? null,
    model: b.model ?? null,
  }));
  startAgentMock.mockReset().mockResolvedValue({ id: 'hub-assistant' });
  // restart-feedback：默认「无记录」——保存用例自行覆盖序列
  getPiRestartStatusMock.mockReset().mockResolvedValue({ lastResult: null });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('SettingsPiHub（P3/P5）', () => {
  it('服务端设置回填输入框', async () => {
    renderEl();
    await waitFor(() => {
      expect((screen.getByLabelText('settings.piHub.provider') as HTMLInputElement).value).toBe('anthropic');
    });
    expect((screen.getByLabelText('settings.piHub.model') as HTMLInputElement).value).toBe('claude-opus-4-6');
  });

  it('保存调用 PUT 全量表单（P10：含 thinking/baseUrl/api/models；ak 留空不携带）', async () => {
    renderEl();
    await waitFor(() => {
      expect((screen.getByLabelText('settings.piHub.provider') as HTMLInputElement).value).toBe('anthropic');
    });
    fireEvent.change(screen.getByLabelText('settings.piHub.model'), { target: { value: 'claude-haiku-4-5-20251001' } });
    fireEvent.change(screen.getByLabelText('settings.piHub.thinking'), { target: { value: 'high' } });
    fireEvent.change(screen.getByLabelText('settings.piHub.baseUrl'), { target: { value: 'https://relay/v1' } });
    fireEvent.change(screen.getByLabelText('settings.piHub.apiType'), { target: { value: 'openai-completions' } });
    fireEvent.change(screen.getByLabelText('settings.piHub.apiKey'), { target: { value: 'sk-new-12345678' } });
    fireEvent.change(screen.getByLabelText('settings.piHub.models'), { target: { value: 'gpt-x, gpt-y' } });
    fireEvent.change(screen.getByLabelText('settings.piHub.allowInject'), { target: { value: 'on' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings.piHub.save' }));
    await waitFor(() => {
      expect(putPiSettingsMock).toHaveBeenCalledWith({
        provider: 'anthropic',
        model: 'claude-haiku-4-5-20251001',
        thinking: 'high',
        allowSessionInject: true,
        workDir: '',
        baseUrl: 'https://relay/v1',
        api: 'openai-completions',
        models: ['gpt-x', 'gpt-y'],
        apiKey: 'sk-new-12345678',
      });
    });
  });

  it('P10 脱敏：掩码只进 placeholder，输入框不回填明文/掩码值', async () => {
    getPiSettingsMock.mockResolvedValue({
      provider: 'lmapi',
      model: 'm0',
      thinking: 'low',
      providerConfig: {
        baseUrl: 'https://keep',
        api: 'anthropic-messages',
        apiKeyMasked: 'ah…keep',
        models: ['m0'],
      },
      allowSessionInject: true,
    });
    renderEl();
    await waitFor(() => {
      expect((screen.getByLabelText('settings.piHub.provider') as HTMLInputElement).value).toBe('lmapi');
    });
    const keyInput = screen.getByLabelText('settings.piHub.apiKey') as HTMLInputElement;
    expect(keyInput.value).toBe(''); // ak 输入框恒空
    expect(keyInput.placeholder).toContain('ah…keep');
    expect((screen.getByLabelText('settings.piHub.models') as HTMLInputElement).value).toBe('m0');
  });

  it('重启按钮走 agents start（P5 重开入口，driver=pi）', async () => {
    renderEl();
    await waitFor(() => {
      expect(screen.getByLabelText('settings.piHub.provider')).toBeTruthy();
    });
    fireEvent.click(screen.getByRole('button', { name: 'settings.piHub.restartAssistant' }));
    await waitFor(() => {
      expect(startAgentMock).toHaveBeenCalledWith(expect.objectContaining({ prompt: '' }), 'pi');
    });
  });

  it('P9 版本显示：探测值可见（容错：探测失败显示降级提示不炸分区）', async () => {
    getPiVersionMock.mockResolvedValue({ version: null });
    renderEl();
    await waitFor(() => {
      expect(screen.getByText('settings.piHub.piVersionMissing')).toBeTruthy();
    });
    getPiVersionMock.mockResolvedValue({ version: '0.73.1' });
    renderEl();
    await waitFor(() => {
      expect(screen.getByText('0.73.1')).toBeTruthy();
    });
  });

  it('PUT 失败透出错误文案（role=alert）', async () => {
    putPiSettingsMock.mockRejectedValue(new Error('boom'));
    renderEl();
    await waitFor(() => {
      expect(screen.getByLabelText('settings.piHub.provider')).toBeTruthy();
    });
    fireEvent.click(screen.getByRole('button', { name: 'settings.piHub.save' }));
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('boom');
    });
  });
});

// ---- restart-feedback：保存后重启终态收敛（C1/C2/C3） ----

describe('SettingsPiHub restart feedback（idle 修复同批：restart-feedback）', () => {
  const saveAndSettle = async (): Promise<void> => {
    renderEl();
    await waitFor(() => {
      expect((screen.getByLabelText('settings.piHub.provider') as HTMLInputElement).value).toBe('anthropic');
    });
    fireEvent.click(screen.getByText('settings.piHub.save'));
  };

  it('C1: 重启完成 → 成功终态文案 + 按钮恢复可点', async () => {
    getPiRestartStatusMock
      .mockResolvedValueOnce({ lastResult: { ok: true, error: null, atMs: 100 } }) // 保存前置快照
      .mockResolvedValue({ lastResult: { ok: true, error: null, atMs: 200 } }); // 轮询命中新记录
    await saveAndSettle();
    // 过渡文案先出现
    await waitFor(() => expect(screen.getByText('settings.piHub.savedNote')).toBeTruthy());
    // 新记录到达 → 成功终态
    await waitFor(() => expect(screen.getByText('settings.piHub.restartedOkNote')).toBeTruthy());
    expect(screen.queryByText('settings.piHub.savedNote')).toBeNull();
    // C3 解除：按钮恢复可点
    await waitFor(() => {
      expect((screen.getByText('settings.piHub.save') as HTMLButtonElement).disabled).toBe(false);
    });
  });

  it('C2: 重启失败 → 错误含原因摘要，不静默常驻过渡文案', async () => {
    getPiRestartStatusMock
      .mockResolvedValueOnce({ lastResult: { ok: true, error: null, atMs: 100 } })
      .mockResolvedValue({
        lastResult: { ok: false, error: 'Hub 助手启动失败: 工作目录不是目录', atMs: 200 },
      });
    renderEl();
    await waitFor(() => {
      expect((screen.getByLabelText('settings.piHub.provider') as HTMLInputElement).value).toBe('anthropic');
    });
    fireEvent.click(screen.getByText('settings.piHub.save'));
    // 先让轮询链真实跑完（1s sleep + mock 即时），再断言 DOM
    await new Promise((r) => setTimeout(r, 1600));
    const alert = screen.getByRole('alert');
    expect(alert.textContent).toContain('settings.piHub.restartedFailNote');
    expect(alert.textContent).toContain('工作目录不是目录');
    expect(screen.queryByText('settings.piHub.savedNote')).toBeNull();
  });

  it('C3: 重启窗口内保存/重启按钮禁用（防双重启）', async () => {
    // 轮询慢命中（1s 后才有新记录）：窗口期按钮应禁用
    getPiRestartStatusMock
      .mockResolvedValueOnce({ lastResult: null })
      .mockImplementation(
        () => new Promise((r) => setTimeout(() => r({ lastResult: { ok: true, error: null, atMs: 5 } }), 1500)),
      );
    await saveAndSettle();
    // 窗口期（首次轮询前）：按钮禁用
    await act(async () => {});
    expect((screen.getByText('settings.piHub.save') as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByText('settings.piHub.restartAssistant') as HTMLButtonElement).disabled).toBe(true);
    // 轮询命中（1s sleep + 1.5s mock）→ 终态解除
    await act(async () => {
      await new Promise((r) => setTimeout(r, 3200));
    });
    expect((screen.getByText('settings.piHub.save') as HTMLButtonElement).disabled).toBe(false);
  });
});
