import { useEffect, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import type { Api } from '../api';
import type { DocFile } from '../types';
import MarkdownView from '../components/MarkdownView';

/// docs 预览 type→renderer 工厂（2026-10-06 用户需求）：预览分发从 if-else 链
/// 改为注册表——内置渲染器（markdown/html/image/pdf/text/binary）在模块尾部
/// 注册，插件经 registerDocRenderer 注入，后注册先命中（可覆盖内置）。
/// 渲染服务类插件读设置（如 plantumlBase），未配置时插件自行回退（不假渲染）。

/** 渲染器入参：docs 端点内容 + 原始字节 URL（image/pdf 用） */
export interface DocRenderProps {
  file: DocFile;
  /** 绝对路径（docsRawUrl 参数） */
  path: string;
  api: Api;
}

export interface DocRendererDef {
  /** 插件 id（调试/去重用） */
  id: string;
  /** 命中判定：后端预判 kind + 文件名（ext 级插件在此分流，如 .puml） */
  match: (kind: string, name: string) => boolean;
  component: (props: DocRenderProps) => ReactElement;
}

const registry: DocRendererDef[] = [];

/** 插件注册缝：后注册先命中（同 match 下可覆盖内置/先前插件） */
export function registerDocRenderer(def: DocRendererDef): void {
  const i = registry.findIndex((d) => d.id === def.id);
  if (i >= 0) registry[i] = def;
  else registry.unshift(def);
}

export function resolveDocRenderer(kind: string, name: string): DocRendererDef | null {
  return registry.find((d) => d.match(kind, name)) ?? null;
}

// —— 内置渲染器 ——

function MarkdownDoc({ file }: DocRenderProps): ReactElement {
  return (
    <div className="doc-md">
      <MarkdownView text={file.content ?? ''} />
    </div>
  );
}

function HtmlDoc({ file, file: { name } }: DocRenderProps): ReactElement {
  const t = useI18n();
  // sandbox=""：禁脚本/表单/同源，仅静态渲染
  return <iframe className="doc-html" sandbox="" title={name} srcDoc={file.content ?? ''} aria-label={t('docs.title')} />;
}

function ImageDoc({ path, api }: DocRenderProps): ReactElement {
  const t = useI18n();
  // 图片：适宽显示 + 点击 lightbox 放大（点遮罩关闭）；raw URL token 走 query
  const [zoomed, setZoomed] = useState(false);
  useEffect(() => setZoomed(false), [path]);
  return (
    <>
      <div className="doc-image-wrap">
        <img className="doc-image" src={api.docsRawUrl(path)} alt="" onClick={() => setZoomed(true)} />
      </div>
      {zoomed ? (
        // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- 遮罩点击即关，整层就是关闭热区
        <div className="doc-lightbox" role="dialog" aria-label={t('docs.closeZoom')} onClick={() => setZoomed(false)}>
          <img className="doc-lightbox-img" src={api.docsRawUrl(path)} alt="" />
        </div>
      ) : null}
    </>
  );
}

function PdfDoc({ path, api, file: { name } }: DocRenderProps): ReactElement {
  // PDF：浏览器原生渲染（不引 pdf.js）
  return <iframe className="doc-pdf" src={api.docsRawUrl(path)} title={name} />;
}

function TextDoc({ file }: DocRenderProps): ReactElement {
  return <pre className="doc-text">{file.content}</pre>;
}

function BinaryDoc({ file }: DocRenderProps): ReactElement {
  const t = useI18n();
  // 未知二进制：友好空态（文件名/大小）
  const fmt = (n: number): string =>
    n >= 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : n >= 1024 ? `${(n / 1024).toFixed(1)} KB` : `${n} B`;
  return (
    <div className="doc-binary">
      <p className="doc-hint">{t('docs.noPreview')}</p>
      <p className="doc-binary-meta">
        {file.name} · {fmt(file.size)}
      </p>
    </div>
  );
}

// 内置注册：unshift 语义（后注册先命中）下按【低→高优先级】注册——
// 兜底 binary 最后注册落最前，markdown/html/image/pdf/text 依次插到其前；
// 插件再经 registerDocRenderer 插到所有内置之前
registerDocRenderer({ id: 'builtin-binary', match: () => true, component: BinaryDoc });
registerDocRenderer({ id: 'builtin-text', match: (k) => k === 'text', component: TextDoc });
registerDocRenderer({ id: 'builtin-pdf', match: (k) => k === 'pdf', component: PdfDoc });
registerDocRenderer({ id: 'builtin-image', match: (k) => k === 'image', component: ImageDoc });
registerDocRenderer({ id: 'builtin-html', match: (k) => k === 'html', component: HtmlDoc });
registerDocRenderer({ id: 'builtin-markdown', match: (k) => k === 'markdown', component: MarkdownDoc });
