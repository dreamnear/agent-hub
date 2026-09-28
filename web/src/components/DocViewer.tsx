import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState, type ReactElement } from 'react';
import { api } from '../api';
import { useI18n } from '../i18n';
import type { DocEntry } from '../types';
import MarkdownView from './MarkdownView';
import './DocViewer.css';

interface Props {
  /** 浏览根目录（agent cwd，白名单成员） */
  root: string;
  /** 移动端树态顶栏关闭（切回聊天视图）；桌面端不渲染 */
  onClose?: () => void;
}

interface Preview {
  path: string;
  name: string;
}

/// P6 反馈轮 12：VSCode 式树状文件浏览器——目录懒加载（展开时按层拉
/// /api/docs/list，大仓只加载展开路径），同级条目左缘严格对齐（depth 统一缩进），
/// ▸/▾ 折叠 + 📁/📄 图标，点击文件只读预览（markdown/html/text/binary 同前）。
/// 反馈轮 15：左右分栏——左树固定宽自身滚动，右内容区占满余宽。
/// 反馈轮 16：≤768px 两态切换（设计稿 DocViewer-Mobile-A/B）——树态顶栏+树全高，
/// 点文件进预览态（44px 返回栏+预览占满），回树态保留已选文件；桌面端零变化。
export default function DocViewer({ root, onClose }: Props): ReactElement {
  const qc = useQueryClient();
  const t = useI18n();
  // 每目录已加载子项：undefined = 未加载；root 初始即加载
  const [dirs, setDirs] = useState<Record<string, DocEntry[] | undefined>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([root]));
  const [preview, setPreview] = useState<Preview | null>(null);
  // 移动端两态（≤768px）：false=树态 / true=预览态；回树态保留已选文件
  const [mobilePreview, setMobilePreview] = useState(false);

  const loadDir = (path: string): void => {
    void qc
      .fetchQuery({
        queryKey: ['docsList', path],
        queryFn: () => api.docsList(path),
        staleTime: 30_000,
      })
      .then((entries) => setDirs((d) => ({ ...d, [path]: entries })))
      .catch(() => setDirs((d) => ({ ...d, [path]: [] })));
  };
  // root 目录首次/切换时加载（useEffect 而非 render 体，防 setState 循环）
  useEffect(() => {
    setExpanded(new Set([root]));
    loadDir(root);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  const toggleDir = (path: string): void => {
    if (dirs[path] === undefined) {
      loadDir(path);
    }
    setExpanded((s) => {
      const next = new Set(s);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  const file = useQuery({
    queryKey: ['docsFile', preview?.path],
    queryFn: () => api.docsFile(preview!.path),
    enabled: preview != null,
  });

  const row = (entry: DocEntry, path: string, depth: number): ReactElement => {
    const isExpanded = expanded.has(path);
    const active = preview?.path === path;
    return (
      <li key={path}>
        <button
          type="button"
          className={`doc-row${active ? ' doc-row--active' : ''}`}
          style={{ paddingLeft: `${depth * 14 + 6}px` }}
          onClick={() => {
            if (entry.isDir) {
              toggleDir(path);
            } else {
              setPreview({ path, name: entry.name });
              setMobilePreview(true);
            }
          }}
        >
          {entry.isDir ? (
            <>
              <span className="doc-caret" aria-hidden="true">
                {isExpanded ? '▾' : '▸'}
              </span>
              <span className="doc-icon">📁</span>
            </>
          ) : (
            <>
              <span className="doc-caret" aria-hidden="true" />
              <span className="doc-icon">📄</span>
            </>
          )}
          <span className="doc-row-name">{entry.name}</span>
          {!entry.isDir && entry.kind !== 'binary' ? (
            <span className="doc-kind">{entry.kind}</span>
          ) : null}
        </button>
        {entry.isDir && isExpanded ? (
          <ul className="doc-tree-sub" role="group">
            {(dirs[path] ?? []).map((c) => row(c, `${path}/${c.name}`, depth + 1))}
          </ul>
        ) : null}
      </li>
    );
  };

  // 返回栏显示相对 root 的路径
  const relPath =
    preview && preview.path.startsWith(`${root}/`)
      ? preview.path.slice(root.length + 1)
      : preview?.path;

  return (
    // 桌面：左右分栏（左树固定宽自身滚，右内容占满余宽）；
    // ≤768px 两态：树态（顶栏+树全高）/ 预览态（44px 返回栏+预览占满），类切换驱动
    <div className={`doc-viewer${mobilePreview && preview ? ' doc-viewer--m-preview' : ''}`}>
      <div className="doc-mobile-head">
        <span>{t('docs.title')}</span>
        {onClose ? (
          <button type="button" onClick={onClose} aria-label={t('docs.close')}>
            ×
          </button>
        ) : null}
      </div>

      <nav className="doc-tree-pane" aria-label={t('docs.treeAria')}>
        {preview ? (
          <button
            type="button"
            className="doc-mobile-back"
            onClick={() => setMobilePreview(false)}
          >
            <span className="doc-mobile-back-arrow" aria-hidden="true">
              ‹
            </span>
            <span className="doc-mobile-back-path">{relPath}</span>
          </button>
        ) : null}
        <ul className="doc-tree" aria-label={t('docs.tree')}>
          {(dirs[root] ?? []).map((e) => row(e, `${root}/${e.name}`, 0))}
          {dirs[root] != null && (dirs[root]?.length ?? 0) === 0 ? (
            <li className="doc-hint">{t('docs.emptyDir')}</li>
          ) : null}
        </ul>
      </nav>

      <div className="doc-preview-pane">
        {preview ? (
          <div className="doc-preview">
            <div className="doc-preview-head">
              <span>{preview.name}</span>
              <button type="button" onClick={() => setPreview(null)} aria-label={t('docs.closePreview')}>
                ×
              </button>
            </div>
            {file.isLoading ? (
              <p className="doc-hint">{t('docs.loading')}</p>
            ) : file.isError ? (
              <p className="doc-hint doc-hint--err">{String(file.error)}</p>
            ) : file.data?.kind === 'markdown' ? (
              <div className="doc-md">
                <MarkdownView text={file.data.content ?? ''} />
              </div>
            ) : file.data?.kind === 'html' ? (
              // sandbox=""：禁脚本/表单/同源，仅静态渲染
              <iframe
                className="doc-html"
                sandbox=""
                title={preview.name}
                srcDoc={file.data.content ?? ''}
              />
            ) : file.data?.kind === 'text' ? (
              <pre className="doc-text">{file.data.content}</pre>
            ) : (
              <p className="doc-hint">{t('docs.noPreview')}</p>
            )}
          </div>
        ) : (
          <p className="doc-empty-hint">{t('docs.pickHint')}</p>
        )}
      </div>
    </div>
  );
}
