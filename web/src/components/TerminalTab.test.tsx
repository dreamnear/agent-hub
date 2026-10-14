// @vitest-environment happy-dom
// 纯工具终端页签（agent-hub-terminal）：xterm 挂载/WS 透传/卸载回收（T1/T3）。
// xterm 动态导入 mock 成假实现（happy-dom 无 canvas，真库不可用）；WebSocket 换可控假体。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, waitFor } from '@testing-library/react';
import { render } from '@testing-library/react';
import { type ReactElement } from 'react';
import TerminalTab from './TerminalTab';
import type { Api } from '../api';

// —— xterm 假体：记录 open/dispose/回调，供断言透传链路 ——
let lastTerm: {
  loadAddon: ReturnType<typeof vi.fn>;
  open: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  write: ReturnType<typeof vi.fn>;
  onData: ReturnType<typeof vi.fn>;
  onResize: ReturnType<typeof vi.fn>;
  options: { theme: Record<string, string> };
} | null = null;
vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    loadAddon = vi.fn();
    open = vi.fn();
    dispose = vi.fn();
    write = vi.fn();
    onData = vi.fn();
    onResize = vi.fn();
    // 真实 Terminal 实例字段（fit 后为实际行列）；首尺寸上报必须读这里（FAIL-2）
    rows = 33;
    cols = 111;
    options: { theme: Record<string, string> } = { theme: {} };
    constructor(opts: { theme: Record<string, string> }) {
      this.options.theme = opts.theme;
      lastTerm = this;
    }
  },
}));
let lastFitRef: { fit: ReturnType<typeof vi.fn>; rows: number; cols: number } | null = null;
vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn();
    rows = 24;
    cols = 80;
    constructor() {
      lastFitRef = this;
    }
  },
}));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

// —— WebSocket 假体：记录 URL/发送，测试内手动触发消息 ——
let lastWs: {
  url: string;
  sent: (string | ArrayBuffer)[];
  onopen: (() => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: (() => void) | null;
  close: ReturnType<typeof vi.fn>;
  send: ReturnType<typeof vi.fn>;
} | null = null;

class FakeWebSocket {
  url: string;
  sent: (string | ArrayBuffer)[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  close = vi.fn();
  send = vi.fn((d: string | ArrayBuffer) => {
    lastWs?.sent.push(d);
  });
  constructor(url: string) {
    this.url = url;
    lastWs = this;
  }
}

const api = {
  terminalWsUrl: (cwd: string) => `ws://hub.test/ws/terminal?cwd=${encodeURIComponent(cwd)}`,
} as unknown as Api;

const onClose = vi.fn();

function renderTab(cwd = '/repo/a'): ReactElement {
  return <TerminalTab cwd={cwd} api={api} onClose={onClose} />;
}

afterEach(() => {
  cleanup();
  lastTerm = null;
  lastWs = null;
  lastFitRef = null;
  vi.restoreAllMocks();
});

describe('TerminalTab（纯工具终端页签，agent-hub-terminal）', () => {
  it('T1 挂载即开 xterm + 连 WS（cwd 参数化、带 token query 由 api 层拼装）', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    render(renderTab());
    await waitFor(() => expect(lastTerm?.open).toHaveBeenCalled());
    await waitFor(() => expect(lastWs?.onopen).not.toBeNull());
    expect(lastWs!.url).toBe(`ws://hub.test/ws/terminal?cwd=${encodeURIComponent('/repo/a')}`);
    expect(lastTerm!.open.mock.calls[0][0]).toBeInstanceOf(HTMLElement);
    // fit 后首尺寸上报
    lastWs!.onopen?.();
    // 帧必含数值 rows/cols 且取自 terminal 实例（FAIL-2：FitAddon 无此字段曾致帧无数值）
    expect(lastWs!.send).toHaveBeenCalledWith(JSON.stringify({ type: 'resize', rows: 33, cols: 111 }));
  });

  it('双向透传：xterm onData → WS binary；WS ArrayBuffer → term.write（T1 输入回显/输出渲染）', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    render(renderTab());
    await waitFor(() => expect(lastTerm?.onData).not.toBeNull());
    lastWs!.onopen?.();
    // 上行：按键 → ArrayBuffer
    const dataCb = lastTerm!.onData.mock.calls[0][0] as (d: string) => void;
    dataCb('ls -la');
    const sent = lastWs!.send.mock.calls.at(-1)?.[0] as ArrayBuffer;
    expect(new TextDecoder().decode(sent)).toBe('ls -la');
    // 下行：PTY 字节 → xterm.write
    const payload = new TextEncoder().encode('hello term');
    lastWs!.onmessage?.({ data: payload.buffer });
    expect(lastTerm!.write).toHaveBeenCalledWith(payload);
  });

  it('T3 卸载回收：WS close + xterm dispose；重开=新实例（历史不保留）', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    const { unmount } = render(renderTab());
    await waitFor(() => expect(lastWs?.onopen).not.toBeNull());
    unmount();
    expect(lastWs!.close).toHaveBeenCalled();
    expect(lastTerm!.dispose).toHaveBeenCalled();
  });

  it('T4 主题：xterm 配色采样 --c-* token（三态切换经 MutationObserver 重涂）', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    render(renderTab());
    await waitFor(() => expect(lastTerm).not.toBeNull());
    // happy-dom getComputedStyle 返回空串属预期——断言采样键在位（真值由浏览器实测兜底）
    expect(Object.keys(lastTerm!.options.theme)).toEqual(
      expect.arrayContaining(['background', 'foreground', 'cursor', 'selectionBackground']),
    );
  });
});

describe('T7 视口铺满（agent-hub-terminal 预览反馈批）', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('T7 FAIL-2 真根因：FitAddon 必须 loadAddon 挂载（未挂载 fit 全程 no-op）且先于 open', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    render(renderTab());
    await waitFor(() => expect(lastTerm?.loadAddon).toHaveBeenCalled());
    // 挂载对象 = FitAddon 实例（wiring 锁定：漏挂 → activate 未调 → _terminal undefined）
    expect(lastTerm!.loadAddon).toHaveBeenCalledWith(lastFitRef);
    // 时序：挂载先于 open（canonical 顺序）
    expect(lastTerm!.loadAddon.mock.invocationCallOrder[0]).toBeLessThan(
      lastTerm!.open.mock.invocationCallOrder[0],
    );
  });

  it('容器有真实尺寸：fit 按实际容器重算（xterm 铺满内容区）', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(1280);
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(720);
    render(renderTab());
    await waitFor(() => expect(lastTerm?.open).toHaveBeenCalled());
    // 尺寸有效 → doFit 放行（首帧 + rAF 兜底至少各一次）
    await waitFor(() => expect(lastFitRef!.fit.mock.calls.length).toBeGreaterThanOrEqual(1));
    expect(lastWs).not.toBeNull();
  });

  it('隐藏态/未布局护判：容器 0×0 跳过 fit（不把视口钉死在极小行列），rAF 兜底也不动', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    // happy-dom 默认 clientWidth/Height = 0
    render(renderTab());
    await waitFor(() => expect(lastTerm?.open).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 30)); // rAF 兜底窗口
    expect(lastFitRef!.fit.mock.calls.length).toBe(0);
    // WS 仍建立（连接不受视口影响）；onopen 上报 terminal 实例行列（数值帧保证）
    lastWs!.onopen?.();
    expect(lastWs!.send).toHaveBeenCalledWith(
      JSON.stringify({ type: 'resize', rows: 33, cols: 111 }),
    );
  });
});
