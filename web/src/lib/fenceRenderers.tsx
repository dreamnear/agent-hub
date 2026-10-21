import { useEffect, useRef, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import BuiCodeBlock from '../components/bui/CodeBlock';
import { plantumlSvgUrlWith } from './plantuml';

/// 围栏渲染注册表（B3，ADR-0012 声明档）：markdown 围栏按语言分流到
/// 「宿主内置引擎」——插件只声明 `{fence/ext → engine}` 映射，**插件零代码进宿主**。
/// 后注册先命中；内置 fence（plantuml/mermaid）服务端已禁插件声明（审计失败），
/// 这里是纯加法注册。一切引擎失败回退源码块（不假渲染，承 plantuml 先例）。

export interface FenceProps {
  code: string;
  lang: string;
  /** 声明的服务配置名（plantuml-server 类引擎；localStorage 键 = `hub_<name>`） */
  service?: string;
  /** 声明来源插件 id（调试展示用） */
  source?: string;
}

export interface FenceRendererDef {
  id: string;
  fences: string[];
  component: (props: FenceProps) => ReactElement;
}

const registry: FenceRendererDef[] = [];

export function registerFenceRenderer(def: FenceRendererDef): void {
  const i = registry.findIndex((d) => d.id === def.id);
  if (i >= 0) registry[i] = def;
  else registry.unshift(def);
}

/** 撤销全部插件侧注册（`plug_` 前缀；插件声明集变化时整体重注册——BR-03 同现同隐） */
export function unregisterPluginRenderers(): void {
  for (let i = registry.length - 1; i >= 0; i--) {
    if (registry[i].id.startsWith('plug_')) registry.splice(i, 1);
  }
}

export function resolveFenceRenderer(lang: string): FenceRendererDef | null {
  const l = lang.toLowerCase();
  return registry.find((d) => d.fences.includes(l)) ?? null;
}

// —— 公共回退：源码块 + 引导提示（不假渲染） ——

export function FenceFallback({
  code,
  lang,
  hintKey,
}: {
  code: string;
  lang: string;
  hintKey: string;
}): ReactElement {
  const t = useI18n();
  const lines = code.replace(/\n$/, '').split('\n');
  return (
    <>
      <div data-bui className="my-2 [&>div]:!max-w-full">
        <BuiCodeBlock lines={lines} filename={lang} labels={{ copy: t('md.copy'), copied: t('md.copied') }} />
      </div>
      <p className="md-fence-hint">{t(hintKey)}</p>
    </>
  );
}

/** 当前主题态（data-theme），属性翻转时通知订阅者——引擎重渲染用 */
export function useThemeSignal(): string {
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme ?? 'auto');
  useEffect(() => {
    const el = document.documentElement;
    const ob = new MutationObserver(() => setTheme(el.dataset.theme ?? 'auto'));
    ob.observe(el, { attributes: true, attributeFilter: ['data-theme'] });
    return () => ob.disconnect();
  }, []);
  return theme;
}

// —— 内置引擎：mermaid（B3 首批，ADR-0012 拍板 1）——

/** mermaid 库句柄缓存：动态 import（重库不进首屏 bundle），失败即 permanently null */
let mermaidMod: Promise<typeof import('mermaid')['default'] | null> | null = null;
function loadMermaid(): Promise<typeof import('mermaid')['default'] | null> {
  mermaidMod ??= import('mermaid')
    .then((m) => m.default)
    .catch(() => null);
  return mermaidMod;
}

let mermaidSeq = 0;

/**
 * 清理 mermaid 库注入 body 的错误横幅残留（F-7，tester B3-1）：render 失败时库会
 * 向 body append `d<渲染id>` 错误元素——失败回退源码（ADR 语义）后这些页面级驻留
 * 元素必须移除，否则跨消息持续可见。
 */
export function cleanupMermaidArtifacts(): void {
  document.querySelectorAll('[id^="dhub-mmd-"]').forEach((el) => el.remove());
}

export function MermaidBlock({ code, lang }: FenceProps): ReactElement {
  const theme = useThemeSignal();
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const seq = useRef(++mermaidSeq);
  useEffect(() => {
    let alive = true;
    setSvg(null);
    setFailed(false);
    void (async () => {
      const mermaid = await loadMermaid();
      if (!mermaid) {
        if (alive) setFailed(true);
        return;
      }
      // securityLevel strict：图源视为不可信输入，输出经库内消毒（ADR-0012 消毒路径）。
      // 供应链注记（M15）：mermaid 升级须回归 XSS 用例（strict 行为随版本可能变化）
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: 'strict',
        theme: theme === 'dark' ? 'dark' : 'default',
      });
      // 离屏容器隔离渲染（F-7）：库的中间产物/错误注入不落正文 DOM
      const container = document.createElement('div');
      container.style.cssText = 'position:absolute;left:-99999px;top:0;visibility:hidden;';
      document.body.appendChild(container);
      try {
        const { svg: out } = await mermaid.render(`hub-mmd-${seq.current}`, code, container);
        if (alive) setSvg(out);
      } catch {
        if (alive) setFailed(true);
      } finally {
        container.remove();
        cleanupMermaidArtifacts();
      }
    })();
    return () => {
      alive = false;
    };
  }, [code, theme]);
  if (failed) return <FenceFallback code={code} lang={lang} hintKey="md.fenceRenderFailed" />;
  if (svg == null) return <div className="md-fence-loading" aria-busy="true" />;
  // mermaid strict 模式输出的 SVG（消毒路径产物）
  return <div className="md-mermaid" dangerouslySetInnerHTML={{ __html: svg }} />;
}

// —— 内置引擎：plantuml-server（既有能力插件化承接；服务地址一律用户设置） ——

export function PlantumlServerBlock({ code, lang, service }: FenceProps): ReactElement {
  const [failed, setFailed] = useState(false);
  // 服务配置名 → 设置值（localStorage 键 = hub_<name>；无声明承 plantumlBase 先例）
  let base = '';
  try {
    base =
      service != null
        ? (localStorage.getItem(`hub_${service}`) ?? '')
        : (localStorage.getItem('hub_plantuml_base') ?? '');
  } catch {
    base = '';
  }
  const url = base !== '' ? plantumlSvgUrlWith(code, base) : null;
  if (url == null || failed) return <FenceFallback code={code} lang={lang} hintKey="md.plantumlNeedConfig" />;
  return <img className="md-plantuml" src={url} alt="plantuml diagram" onError={() => setFailed(true)} />;
}

// —— 内置引擎：image（fence 内容为图片 URL）——

export function ImageBlock({ code, lang }: FenceProps): ReactElement {
  const [failed, setFailed] = useState(false);
  const u = code.trim();
  const safe = /^https?:\/\//i.test(u) || u.startsWith('/');
  if (!safe || failed) return <FenceFallback code={code} lang={lang} hintKey="md.fenceRenderFailed" />;
  return <img className="md-plantuml" src={u} alt={lang} onError={() => setFailed(true)} />;
}

// —— 内置引擎：html-sanitized（fence 内容为 HTML 源，sandbox 静态渲染）——

export function HtmlSanitizedBlock({ code, lang }: FenceProps): ReactElement {
  return (
    <iframe
      className="md-fence-html"
      sandbox=""
      title={lang}
      srcDoc={code}
    />
  );
}

// 内置注册（unshift 语义：后注册先命中；插件的 fence 由 usePluginRenderers 转译注册）
registerFenceRenderer({ id: 'builtin:html-sanitized', fences: ['html'], component: HtmlSanitizedBlock });
registerFenceRenderer({ id: 'builtin:image', fences: ['image'], component: ImageBlock });
registerFenceRenderer({ id: 'builtin:plantuml', fences: ['plantuml'], component: PlantumlServerBlock });
registerFenceRenderer({ id: 'builtin:mermaid', fences: ['mermaid'], component: MermaidBlock });

/** 由插件声明组装引擎组件（宿主转译：声明 → 内置引擎实例，插件零代码） */
export function componentForEngine(
  engine: string,
  opts: { service?: string; source?: string },
): ((props: FenceProps) => ReactElement) | null {
  switch (engine) {
    case 'mermaid':
      return MermaidBlock;
    case 'plantuml-server':
      return (p: FenceProps) => <PlantumlServerBlock {...p} service={opts.service} />;
    case 'image':
      return ImageBlock;
    case 'html-sanitized':
      return HtmlSanitizedBlock;
    default:
      return null;
  }
}
