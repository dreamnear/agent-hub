import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { ReactElement } from 'react';
import BuiCodeBlock from './bui/CodeBlock';
import './MarkdownView.css';

/// assistant 消息 markdown 渲染（P5 preview 反馈：###/**/列表/代码块正常渲染）。
/// react-markdown 默认不渲染 raw HTML（无 dangerouslySetInnerHTML，XSS 面封闭）。
/// 代码块（Beautiful UI 批2）：pre → BuiCodeBlock（行号 + 轻语法着色 + 复制），
/// 行内 code 仍走 .md-body code 样式。

/// 从 react-markdown 的 pre>code 提取语言与文本行，交 BuiCodeBlock 渲染
function nodeText(node: React.ReactNode): string {
  if (node == null || typeof node === 'boolean') return '';
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(nodeText).join('');
  const el = node as { props?: { children?: React.ReactNode } };
  return el.props ? nodeText(el.props.children) : '';
}

function BuiPre({ children }: { children?: React.ReactNode }) {
  const codeEl = (Array.isArray(children) ? children[0] : children) as
    | { props?: { className?: string; children?: React.ReactNode } }
    | undefined;
  const cls = codeEl?.props?.className ?? '';
  const lang = /language-([\w-]+)/.exec(cls)?.[1] ?? 'code';
  const raw = nodeText(codeEl?.props?.children);
  const lines = raw.replace(/\n$/, '').split('\n');
  return (
    <div data-bui className="my-2 [&>div]:!max-w-full">
      <BuiCodeBlock lines={lines} filename={lang} labels={{ copy: '复制', copied: '已复制' }} />
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
