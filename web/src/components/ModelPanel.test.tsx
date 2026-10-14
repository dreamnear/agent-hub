// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ModelSettings } from '../api';
import ModelPanel from './ModelPanel';

const settings = (overrides: Partial<ModelSettings> = {}): ModelSettings => ({
  caps: { canSet: true, canRead: true, supportsThinking: true },
  current: null,
  models: [
    {
      id: 'space-bunny-free',
      provider: 'opencode',
      name: 'Space Bunny Free',
      free: true,
      variants: ['low', 'high'],
    },
    { id: 'muse-free', provider: 'opencode', name: 'Muse Free', free: true, variants: [] },
    { id: 'big-pickle', provider: 'opencode', name: 'Big Pickle', free: false, variants: [] },
  ],
  ...overrides,
});

function renderPanel(
  settingsData: ModelSettings,
  setModel: ReturnType<typeof vi.fn> = vi.fn().mockResolvedValue({ message: '已切换' }),
  onSelect: () => void = vi.fn(),
): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const api = { getModelSettings: vi.fn().mockResolvedValue(settingsData), setModelSettings: setModel };
  render(
    <QueryClientProvider client={qc}>
      <ModelPanel sessionId="ses_x" driver="opencode" onSelect={onSelect} onClose={vi.fn()} api={api as never} />
    </QueryClientProvider>,
  );
}

function modelRows(): HTMLElement[] {
  return screen.getAllByRole('button').filter((b) => b.className.includes('model-row'));
}

afterEach(cleanup);

describe('ModelPanel（agent-hub-model-thinking）', () => {
  it('清单渲染：免费小标 + 当前模型勾标；思考档位行内展开（二段选择）', async () => {
    renderPanel(
      settings({
        current: { id: 'space-bunny-free', provider: 'opencode', variant: 'high' },
      }),
    );
    await waitFor(() => expect(modelRows().length).toBe(3));
    // 免费小标
    expect(screen.getAllByText('免费').length).toBeGreaterThan(0);
    // 当前模型行勾标（✓）；只验渲染不抛（行为在后续用例）
    const cur = modelRows().find((b) => b.className.includes('model-row--cur'));
    expect(cur).toBeTruthy();
    expect(cur?.textContent).toContain('Space Bunny Free');
  });

  it('点无档位模型 → 直接设置（model 透传）', async () => {
    const setModel = vi.fn().mockResolvedValue({ message: '已切换' });
    renderPanel(settings(), setModel);
    await waitFor(() => expect(modelRows().length).toBe(3));
    fireEvent.click(modelRows().filter((b) => !b.className.includes('--cur'))[1]); // muse-free
    await waitFor(() => expect(setModel).toHaveBeenCalledWith('ses_x', { model: 'muse-free' }, 'opencode'));
  });

  it('点有档位模型 → 先展开档位，点档位带 variant 设置；收场文案上屏', async () => {
    const setModel = vi.fn().mockResolvedValue({ message: '已切换' });
    renderPanel(settings(), setModel);
    await waitFor(() => expect(modelRows().length).toBe(3));
    // 选非当前的 space-bunny（有档位）→ 展开而非立即设置
    fireEvent.click(modelRows().find((b) => !b.className.includes('--cur')) as HTMLElement);
    await waitFor(() => expect(screen.getByRole('group')).toBeTruthy());
    fireEvent.click(screen.getByText('low'));
    await waitFor(() =>
      expect(setModel).toHaveBeenCalledWith('ses_x', { model: 'space-bunny-free', variant: 'low' }, 'opencode'),
    );
    // 收场文案（驱动返回）如实上屏
    await waitFor(() => expect(screen.getByText('已切换')).toBeTruthy());
  });

  it('设置收场「已排队」如实上屏（claude working 场景同形态）', async () => {
    const setModel = vi.fn().mockResolvedValue({ message: '已排队（会话正在处理，当前轮结束后生效）' });
    renderPanel(
      settings({
        caps: { canSet: true, canRead: true, supportsThinking: false },
        // claude 口径：内置清单无思考档位
        models: settings().models.map((m) => ({ ...m, variants: [] })),
      }),
      setModel,
    );
    await waitFor(() => expect(modelRows().length).toBe(3));
    fireEvent.click(modelRows()[0]);
    await waitFor(() => expect(screen.getByText(/已排队/)).toBeTruthy(), { timeout: 3000 });
  });
});
