import { useEffect, useRef, type ReactElement } from 'react';
import type { Api } from '../api';
import { useI18n } from '../i18n';
import './TerminalTab.css';

interface Props {
  /** 终端启动目录（选中会话 cwd，agent-hub-terminal T1） */
  cwd: string;
  /** WS 地址来源（按实例 baseUrl/token 三态） */
  api: Api;
  /** 关闭页签（App 卸载本组件 → WS 断 → 后端回收 PTY，T3） */
  onClose: () => void;
}

/// 从三态主题 token 采样 xterm 配色（T4：前景/背景/光标/选区全走 --c-*，
/// data-theme 切换由 MutationObserver 感知后重涂，组件不自带主题逻辑）
function termTheme(): Record<string, string> {
  const s = getComputedStyle(document.documentElement);
  const v = (name: string): string => s.getPropertyValue(name).trim();
  return {
    background: v('--c-bg'),
    foreground: v('--c-text'),
    cursor: v('--c-accent'),
    cursorAccent: v('--c-bg'),
    selectionBackground: v('--c-border-input'),
    selectionForeground: v('--c-text'),
  };
}

/// xterm 终端最小面（仅本组件消费，避免引入完整类型依赖进主图）。
/// rows/cols 是 Terminal 实例字段（fit 后为真实行列）——首尺寸上报只读这里，
/// 禁读 FitAddon（其实例无 rows/cols，走查 FAIL-2 实证帧无数值）。
interface TermLike {
  loadAddon: (addon: unknown) => void;
  open: (el: HTMLElement) => void;
  dispose: () => void;
  rows: number;
  cols: number;
  onData: (cb: (d: string) => void) => void;
  onResize: (cb: (d: { rows: number; cols: number }) => void) => void;
  write: (d: Uint8Array) => void;
  options: { theme: Record<string, string> };
}

interface FitLike {
  fit: () => void;
  rows: number;
  cols: number;
}

/// 纯工具终端页签（agent-hub-terminal）：xterm.js dynamic import（主包预算照守），
/// WS ⇄ PTY 字节透传；组件卸载即断 WS（后端随断回收进程组，T3）。
export default function TerminalTab({ cwd, api, onClose }: Props): ReactElement {
  const t = useI18n();
  const hostRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let disposed = false;
    let ws: WebSocket | null = null;
    let observer: MutationObserver | null = null;
    let resizeObs: ResizeObserver | null = null;
    let term: TermLike | null = null;
    let raf: number | null = null;

    void (async () => {
      // dynamic import：xterm 及其 CSS 全部走独立 chunk，主包 bundle 不受影响（T4/T6）
      const xterm = await import('@xterm/xterm');
      const fitMod = await import('@xterm/addon-fit');
      await import('@xterm/xterm/css/xterm.css');
      if (disposed || hostRef.current == null) return;

      term = new xterm.Terminal({
        fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
        fontSize: 13.5, // 舒适默认（T7：偏小反馈）；fit 行列数随字号重算
        cursorBlink: true,
        scrollback: 4000,
        theme: termTheme(),
      }) as unknown as TermLike;
      const fit = new fitMod.FitAddon() as unknown as FitLike;
      const t0 = term;
      // 挂载必须在 open/fit 之前：未挂载 FitAddon 的 activate 未调 → _terminal
      // undefined → proposeDimensions undefined → fit() 全程静默 no-op（走查
      // FAIL-2 轮 2 实证 .xterm-screen 恒 80×24）
      t0.loadAddon(fit);
      t0.open(hostRef.current);

      // T7 视口铺满：fit 前护判容器有效尺寸——页签 CSS 隐藏态/未布局完成时容器
      // 为 0×0，此时 fit 会把视口钉死在极小行列（用户实测缺陷根因）。零尺寸跳过，
      // 待 ResizeObserver 报告真实尺寸（切回页签/首帧布局完成）时自动重算。
      const doFit = (): void => {
        const el = hostRef.current;
        if (el == null || el.clientWidth === 0 || el.clientHeight === 0) return;
        fit.fit();
      };
      doFit();
      // 首帧布局晚于动态 import 落定时兜底重算一次
      raf = requestAnimationFrame(() => doFit());

      ws = new WebSocket(api.terminalWsUrl(cwd));
      ws.binaryType = 'arraybuffer';
      let up = false;
      ws.onopen = () => {
        up = true;
        // 首尺寸上报：读 terminal 实例真实行列（fit 可能晚于 WS 建立才落定，
        // 此处取到的即 fit 后最新值）；此后尺寸变化由 onResize 驱动
        ws?.send(JSON.stringify({ type: 'resize', rows: t0.rows, cols: t0.cols }));
      };
      ws.onmessage = (ev) => {
        if (ev.data instanceof ArrayBuffer) t0.write(new Uint8Array(ev.data));
      };
      ws.onclose = () => {
        up = false;
      };
      t0.onData((d) => {
        if (up) ws?.send(new TextEncoder().encode(d));
      });
      t0.onResize(({ rows, cols }) => {
        if (up) ws?.send(JSON.stringify({ type: 'resize', rows, cols }));
      });

      // 容器尺寸变化 → 护判后重排（窗口/面板拖动/页签切换/375px 均走此通道）
      resizeObs = new ResizeObserver(() => doFit());
      resizeObs.observe(hostRef.current);
      // 三态主题切换重涂（T4）
      observer = new MutationObserver(() => {
        t0.options.theme = termTheme();
      });
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ['data-theme'],
      });
    })();

    return () => {
      disposed = true;
      if (raf != null) cancelAnimationFrame(raf);
      resizeObs?.disconnect();
      observer?.disconnect();
      ws?.close(); // WS 断 → 后端 pump 退出 → kill_and_reap（T3 无僵尸）
      term?.dispose();
    };
    // 终端随 cwd 重建（T3：重开=新实例，历史不保留）；cwd/api 挂载期不变
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cwd, api]);

  return (
    <div className="terminal-tab" aria-label={t('tabs.terminal')}>
      <div className="terminal-head">
        <span className="terminal-cwd" title={cwd}>
          {cwd}
        </span>
        <button
          type="button"
          className="terminal-close"
          aria-label={t('tabs.close')}
          onClick={onClose}
        >
          ×
        </button>
      </div>
      <div className="terminal-host" ref={hostRef} />
    </div>
  );
}
