import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from 'react';
import { plugins, viewAssetUrl, type PluginNavItem, type PluginView } from '../plugins';
import { resolvedTheme, themeAliases, themeTokenSnapshot } from '../lib/themeTokens';
import { resolveLocale, useI18n } from '../i18n';
import { useTheme } from '../hooks/useTheme';
import './PluginViewHost.css';

interface Props {
  plugin: PluginView;
  view: PluginNavItem;
  onExit: () => void;
  /** 视图内的 hub:rpc 落点（默认走 /api/plugins/{id}/rpc；测试可注入假实现） */
  rpc?: (pluginId: string, method: string, params: unknown) => Promise<unknown>;
}

/// 宿主 → 视图消息（ADR-0004 第 5 条，v1 固定集合）
type HostMessage =
  | { type: 'hub:init'; payload: InitPayload }
  | { type: 'hub:theme'; payload: { theme: 'dark' | 'light'; tokens: Record<string, string> } }
  | { type: 'hub:rpc-result'; payload: { id: string; ok: true; result: unknown } }
  | { type: 'hub:rpc-error'; payload: { id: string; ok: false; code: string; message: string } };

interface InitPayload {
  apiVersion: number;
  pluginId: string;
  pluginVersion: string;
  viewId: string;
  theme: 'dark' | 'light';
  tokens: Record<string, string>;
  aliases: Record<string, string>;
  locale: string;
  viewBase: string;
}

const RESIZE_THROTTLE_MS = 250;
/// 票据在 TTL 的 80% 处静默重取（ADR-0004 第 3 条：到期 = 重建 iframe，
/// 视图内状态保持归插件——契约已明示）。
const TICKET_REFRESH_RATIO = 0.8;

/// iframe 沙箱宿主（ADR-0004 第 1/3/5/6 条）：
/// - `sandbox="allow-scripts allow-forms allow-popups"`——**不含 allow-same-origin**
///   （视图为 null origin，跨源隔离由浏览器保证）；
/// - 门禁 = 视图票据（票据 ≠ hub token，换不到任何 `/api` 能力）；
/// - 主题走 token 字典桥（BR-16），视图不自带固定明暗配色；
/// - 视图崩溃/加载失败 → 故障层覆盖内容区，宿主框架不白屏（BR-08）。
export default function PluginViewHost({ plugin, view, onExit, rpc }: Props): ReactElement {
  const t = useI18n();
  const { effective } = useTheme();
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [ticket, setTicket] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const [fault, setFault] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [height, setHeight] = useState<number | null>(null);
  const resizeGate = useRef<number>(0);
  // 视图 entry 的文件名（资源根 = entry 的父目录，见服务端 view_root）
  const entryFile = useMemo(() => {
    const entry = plugin.contributions.find((c) => c.kind === 'view' && c.id === view.viewId)?.path;
    const file = (entry ?? 'view/index.html').split('/').pop();
    return file != null && file !== '' ? file : 'index.html';
  }, [plugin.contributions, view.viewId]);

  const initPayload = useCallback((): InitPayload => {
    const tokens = themeTokenSnapshot();
    return {
      apiVersion: 1,
      pluginId: plugin.id,
      pluginVersion: plugin.version,
      viewId: view.viewId,
      theme: resolvedTheme(),
      tokens,
      aliases: themeAliases(tokens),
      locale: resolveLocale(),
      viewBase: `/plugin-view/${plugin.id}/`,
    };
  }, [plugin.id, plugin.version, view.viewId]);

  const send = useCallback((msg: HostMessage): void => {
    // 目标 origin 为 opaque（null），只能以 '*' 投递；来源校验在入站侧做
    iframeRef.current?.contentWindow?.postMessage(msg, '*');
  }, []);

  // 取票据 → 设 src（重试 = 重新取票据 + 重建 iframe）
  useEffect(() => {
    let cancelled = false;
    setReady(false);
    setFault(null);
    plugins
      .viewTicket(plugin.id)
      .then((r) => {
        if (!cancelled) setTicket(r.ticket);
      })
      .catch((e: unknown) => {
        if (!cancelled) setFault(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [plugin.id, attempt]);

  // 票据到期前静默重取（重建 iframe；状态保持归插件）
  useEffect(() => {
    if (ticket == null) return;
    const ttlMs = 120_000 * TICKET_REFRESH_RATIO;
    const timer = window.setTimeout(() => setAttempt((n) => n + 1), ttlMs);
    return () => window.clearTimeout(timer);
  }, [ticket]);

  // 宿主主题 / 语言变化 → hub:theme（全量 token 重发，幂等）
  useEffect(() => {
    if (!ready) return;
    send({ type: 'hub:theme', payload: { theme: resolvedTheme(), tokens: themeTokenSnapshot() } });
  }, [effective, ready, send]);

  // 入站消息桥（来源校验 + 类型分派；未知类型丢弃）
  useEffect(() => {
    const onMessage = (ev: MessageEvent): void => {
      const frame = iframeRef.current;
      // 判据 = event.source 与 iframe 内容窗口句柄比对（ADR-0004 第 5 条纪律 2）；
      // sandbox 无 allow-same-origin 时 origin 为 'null'，仅接受已知句柄
      if (frame == null || ev.source !== frame.contentWindow) return;
      const data = ev.data as { type?: string; payload?: unknown } | null;
      if (data == null || typeof data.type !== 'string') return;
      if (data.type === 'hub:ready') {
        setReady(true);
        send({ type: 'hub:init', payload: initPayload() });
        return;
      }
      if (data.type === 'hub:rpc') {
        const p = data.payload as { id?: string; method?: string; params?: unknown } | undefined;
        if (p?.id == null || p.method == null) return;
        const call = rpc ?? ((pid: string, method: string, params: unknown) => plugins.rpc(pid, method, params));
        call(plugin.id, p.method, p.params ?? {})
          .then((result) =>
            send({ type: 'hub:rpc-result', payload: { id: p.id!, ok: true, result } }),
          )
          .catch((e: unknown) => {
            const msg = e instanceof Error ? e.message : String(e);
            send({
              type: 'hub:rpc-error',
              payload: {
                id: p.id!,
                ok: false,
                code: /403|forbidden|unauthorized|violation/i.test(msg)
                  ? 'violation'
                  : 'plugin_error',
                message: msg,
              },
            });
          });
        return;
      }
      if (data.type === 'hub:notify') {
        const p = data.payload as
          | { kind?: string; height?: number; message?: string; viewId?: string }
          | undefined;
        if (p?.kind === 'resize') {
          const now = Date.now();
          if (now - resizeGate.current < RESIZE_THROTTLE_MS) return;
          resizeGate.current = now;
          const h = typeof p.height === 'number' ? Math.max(120, Math.round(p.height)) : null;
          if (h != null) setHeight(h);
          return;
        }
        if (p?.kind === 'error') {
          setFault(p.message ?? t('plugin.view.errorReported'));
          return;
        }
        // navigate：v1 只允许跳「插件视图组内」的视图（跨插件/主体页面拒绝）
        if (p?.kind === 'navigate' && p.viewId != null && p.viewId !== view.viewId) {
          const own = plugin.views.some((v) => v.id === p.viewId);
          if (!own) setFault(t('plugin.view.navigateDenied'));
        }
      }
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, [initPayload, plugin.id, plugin.views, rpc, send, t, view.viewId]);

  const retry = (): void => setAttempt((n) => n + 1);

  return (
    <div className="plugin-view-host">
      <header className="plugin-view-head">
        <div className="plugin-view-title">
          <span className="plugin-view-name">{view.title}</span>
          <span className="plugin-view-tag">
            {plugin.name} · v{plugin.version}
          </span>
        </div>
        {/* BR-16 可观察化：视图头显示宿主当前主题态 */}
        <span className="plugin-view-theme" title={t('plugin.view.themeTitle')}>
          {effective === 'dark' ? t('plugin.view.themeDark') : t('plugin.view.themeLight')}
        </span>
        <button type="button" className="plugin-view-exit" onClick={onExit}>
          {t('plugin.view.exit')}
        </button>
      </header>
      <div className="plugin-view-body" style={height != null ? { minHeight: `${height}px` } : undefined}>
        {ticket != null && fault == null ? (
          <iframe
            ref={iframeRef}
            className="plugin-view-frame"
            title={view.title}
            sandbox="allow-scripts allow-forms allow-popups"
            referrerPolicy="no-referrer"
            src={viewAssetUrl(plugin.id, entryFile, ticket)}
            onError={() => setFault(t('plugin.view.loadFailed'))}
          />
        ) : null}
        {ticket == null && fault == null ? (
          <div className="plugin-view-loading" role="status">
            {t('plugin.view.loading')}
          </div>
        ) : null}
        {/* 故障层：覆盖内容区、不覆盖宿主框架（BR-08 不白屏） */}
        {fault != null ? (
          <div className="plugin-view-fault" role="alert">
            <div className="plugin-view-fault-title">{t('plugin.view.faultTitle')}</div>
            <div className="plugin-view-fault-msg">{fault}</div>
            <div className="plugin-view-fault-actions">
              <button type="button" className="btn-primary" onClick={retry}>
                {t('plugin.view.retry')}
              </button>
              <button type="button" className="btn-ghost" onClick={onExit}>
                {t('plugin.view.exit')}
              </button>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
