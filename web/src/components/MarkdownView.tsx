import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useState, type ReactElement } from 'react';
import BuiCodeBlock from './bui/CodeBlock';
import { useI18n } from '../i18n';
import { plantumlSvgUrl } from '../lib/plantuml';
import './MarkdownView.css';

/// assistant 消息 markdown 渲染（P5 preview 反馈：###/**/列表/代码块正常渲染）。
/// react-markdown 默认不渲染 raw HTML（无 dangerouslySetInnerHTML，XSS 面封闭）。
/// 代码块（Beautiful UI 批2）：pre → BuiCodeBlock（行号 + 轻语法着色 + 复制），
/// 行内 code 仍走 .md-body code 样式。
/// plantuml 围栏块（2026-10-06 用户需求）：配置了渲染服务 → 出图（img，不 inline
/// SVG 防注入）；未配置/加载失败 → 回退源码块 + 引导提示（服务地址在设置-文档）。

/// 从 react-markdown 的 pre>code 提取语言与文本行，交 BuiCodeBlock 渲染
function nodeText(node: React.ReactNode): string {
  if (node == null || typeof node === 'boolean') return '';
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(nodeText).join('');
  const el = node as { props?: { children?: React.ReactNode } };
  return el.props ? nodeText(el.props.children) : '';
}

function PlantumlFallback({ lines, lang }: { lines: string[]; lang: string }): ReactElement {
  const t = useI18n();
  return (
    <>
      <div data-bui className="my-2 [&>div]:!max-w-full">
        <BuiCodeBlock lines={lines} filename={lang} labels={{ copy: t('md.copy'), copied: t('md.copied') }} />
      </div>
      <p className="md-plantuml-hint">{t('md.plantumlNeedConfig')}</p>
    </>
  );
}

function PlantumlBlock({ code, lines, lang }: { code: string; lines: string[]; lang: string }): ReactElement {
  // onError 回退源码（服务不可达/图语法错）：url 换 key 触发不重试
  const [failed, setFailed] = useState(false);
  const url = plantumlSvgUrl(code);
  if (url == null || failed) return <PlantumlFallback lines={lines} lang={lang} />;
  return (
    <img
      className="md-plantuml"
      src={url}
      alt="plantuml diagram"
      onError={() => setFailed(true)}
    />
  );
}

function BuiPre({ children }: { children?: React.ReactNode }) {
  const t = useI18n();
  const codeEl = (Array.isArray(children) ? children[0] : children) as
    | { props?: { className?: string; children?: React.ReactNode } }
    | undefined;
  const cls = codeEl?.props?.className ?? '';
  const lang = /language-([\w-]+)/.exec(cls)?.[1] ?? 'code';
  const raw = nodeText(codeEl?.props?.children);
  const lines = raw.replace(/\n$/, '').split('\n');
  if (/^plantuml$/i.test(lang) && lines.length > 0) {
    return <PlantumlBlock code={raw.replace(/\n$/, '')} lines={lines} lang={lang} />;
  }
  return (
    <div data-bui className="my-2 [&>div]:!max-w-full">
      <BuiCodeBlock lines={lines} filename={lang} labels={{ copy: t('md.copy'), copied: t('md.copied') }} />
    </div>
  );
}

/// 宽表格不撑破消息列：包横向滚动壳（375 实测 table right=381>375，
/// 撑破后整个消息流可平移、工具卡片右缘出屏）
function ScrollTable({ children }: { children?: React.ReactNode }) {
  return (
    <div className="md-table-wrap">
      <table>{children}</table>
    </div>
  );
}

export default function MarkdownView({ text }: { text: string }): ReactElement {
  return (
    <div className="md-body">
      <Markdown remarkPlugins={[remarkGfm]} components={{ pre: BuiPre, table: ScrollTable }}>
        {text}
      </Markdown>
    </div>
  );
}
