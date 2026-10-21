import { useEffect, useRef, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import { plugins, viewAssetUrl } from '../plugins';
import { useThemeSignal } from './themeSignal';

/// 渲染器 iframe 档（B3 尾批，ADR-0012 拍板 2 + §12.2 契约）：插件声明
/// `engine:"plugin-iframe"` → 宿主把格式（fence/ext）映射到插件的渲染页
/// （跑在沙箱 iframe，ADR-0004 机制复用：票据 + 无 allow-same-origin）。
/// **插件零代码进宿主**——插件 JS 只在沙箱 iframe 内执行，宿主上下文零执行。
///
/// 消息契约（ADR-0012 §12.2，固定集合）：
/// - 宿主 → 渲染页：`render`  {source, lang, theme, tokens}
/// - 渲染页 → 宿主：`rendered` {ok:true, height} / {ok:false, reason}
/// - 渲染页 → 宿主：`hub:notify` {kind:"resize", height}（沿用视图宿主同族）
///
/// 失败回退：rendered ok:false / 超时（RENDER_TIMEOUT_MS）/ 票据失败 → 回退源码
/// 展示（不假渲染，承声明档同语义）。懒挂载：IntersectionObserver 进视口才挂。

const RENDER_TIMEOUT_MS = 15_000;
const RESIZE_MIN_HEIGHT = 60;

export interface IframeRenderProps {
  pluginId: string;
  entry: string;
  /** 渲染源（围栏 code 或文件内容） */
  source: string;
  lang: string;
  /** 回退：源码块（不假渲染） */
  fallback: ReactElement;
}

function currentTokens(): Record<string, string> {
  // 宿主 --c-* 生效值快照（getComputedStyle 逐 token 取——三态跟随的运行期值）
  const cs = getComputedStyle(document.documentElement);
  const tokens: Record<string, string> = {};
  for (const name of ['--c-bg', '--c-panel', '--c-text', '--c-sub', '--c-border', '--c-accent']) {
    const v = cs.getPropertyValue(name).trim();
    if (v) tokens[name] = v;
  }
  return tokens;
}

export function DeclarativeIframeRenderer({ pluginId, entry, source, lang, fallback }: IframeRenderProps): ReactElement {
  const t = useI18n();
  const theme = useThemeSignal();
  const [ticket, setTicket] = useState<string | null>(null);
  const [mounted, setMounted] = useState(false); // 懒挂载（IntersectionObserver）
  const [failed, setFailed] = useState(false);
  const [failReason, setFailReason] = useState<string | null>(null);
  const [height, setHeight] = useState<number | null>(null);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const sentRef = useRef(false);

  // 票据（120s TTL；失败即回退）
  useEffect(() => {
    let alive = true;
    void plugins
      .viewTicket(pluginId)
      .then((r) => {
        if (alive) setTicket((r as { ticket: string }).ticket ?? null);
      })
      .catch(() => {
        if (alive) setFailed(true);
      });
    return () => {
      alive = false;
    };
  }, [pluginId]);

  // 懒挂载：进视口才挂 iframe（滚动出视口可卸载，ADR-0012 §12.2）
  useEffect(() => {
    const el = wrapRef.current;
    if (el == null || typeof IntersectionObserver === 'undefined') {
      setMounted(true);
      return;
    }
    const ob = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) setMounted(true);
        }
      },
      { rootMargin: '120px' },
    );
    ob.observe(el);
    return () => ob.disconnect();
  }, []);

  // render 消息（ticket+iframe+mounted 后发；主题/token 变化重发——渲染页自适配）
  useEffect(() => {
    if (!mounted || ticket == null || failed) return;
    sentRef.current = false;
    const timer = window.setTimeout(() => {
      if (sentRef.current) return;
      // 超时无回执：回退源码（不假渲染）
      setFailReason(t('plugin.render.timeout'));
      setFailed(true);
    }, RENDER_TIMEOUT_MS);
    const send = (): void => {
      frameRef.current?.contentWindow?.postMessage(
        { type: 'render', payload: { source, lang, theme, tokens: currentTokens() } },
        '*',
      );
      // 首帧后补发（iframe onload 时才能收消息；重复发渲染页按幂等处理）
      window.setTimeout(() => {
        frameRef.current?.contentWindow?.postMessage(
          { type: 'render', payload: { source, lang, theme, tokens: currentTokens() } },
          '*',
        );
      }, 300);
    };
    send();
    return () => window.clearTimeout(timer);
  }, [mounted, ticket, source, lang, theme, failed]);

  // rendered / resize 回执（sandbox origin='null'，仅接受已知句柄）
  useEffect(() => {
    if (!mounted || ticket == null || failed) return;
    const onMsg = (ev: MessageEvent): void => {
      if (ev.source !== frameRef.current?.contentWindow) return;
      const data = ev.data || {};
      if (data.type === 'rendered') {
        sentRef.current = true;
        const p = data.payload || {};
        if (p.ok) {
          setFailed(false);
          setFailReason(null);
          const h = typeof p.height === 'number' ? Math.max(RESIZE_MIN_HEIGHT, Math.round(p.height)) : null;
          if (h != null) setHeight(h);
        } else {
          setFailReason(typeof p.reason === 'string' ? p.reason : null);
          setFailed(true);
        }
        return;
      }
      if (data.type === 'hub:notify') {
        const p = data.payload || {};
        if (p.kind === 'resize' && typeof p.height === 'number') {
          setHeight(Math.max(RESIZE_MIN_HEIGHT, Math.round(p.height)));
        }
      }
    };
    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, [mounted, ticket, failed]);

  // 失败判定先于 loading（票据失败时 ticket 仍 null——回退必须可达）
  if (failed) {
    return (
      <div ref={wrapRef}>
        {fallback}
        {failReason != null ? (
          <p className="md-fence-hint">{t('plugin.render.failReason')} {failReason}</p>
        ) : null}
      </div>
    );
  }
  if (ticket == null || mounted == false) {
    return (
      <div ref={wrapRef} className="md-fence-loading" aria-busy="true" title={t('plugin.view.loading')} />
    );
  }
  return (
    <div ref={wrapRef} className="iframe-render-wrap">
      <iframe
        ref={frameRef}
        className="iframe-render-frame"
        title={`render:${lang}`}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
        src={viewAssetUrl(pluginId, entry, ticket)}
        style={height != null ? { height: `${height}px` } : undefined}
      />
    </div>
  );
}
