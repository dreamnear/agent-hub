import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useState, type ReactElement } from 'react';
import { api } from '../api';
import { useI18n } from '../i18n';
import { DOC_REFRESH_MS } from '../lib/docRefresh';
import type { DocEntry } from '../types';
import './FilesTree.css';

interface Props {
  /** 浏览根目录（选中会话 cwd，白名单成员）；null = 无会话空态 */
  root: string | null;
  /** 主区当前打开的文件（面板行高亮同步） */
  activePath?: string | null;
  /** 点文件上抛（主区覆盖层渲染，agent-hub-files-panel F2） */
  onOpenFile: (path: string, name: string) => void;
}

/// 文件树（agent-hub-files-panel F1）：从原 DocViewer 树段抽出改造——目录懒加载
/// （展开按层拉 /api/docs/list，大仓只加载展开路径）、▸/▾ 折叠、点击文件上抛
/// （内容渲染归主区覆盖层，树只做导航）；root=null 渲染空态（与 git 面板同）。
/// 原「文档」Tab 的树⇄预览分栏/移动两态由「面板 + 主区覆盖层」模型承接，能力不回退。
export default function FilesTree({ root, activePath, onOpenFile }: Props): ReactElement {
  const qc = useQueryClient();
  const t = useI18n();
  const [dirs, setDirs] = useState<Record<string, DocEntry[] | undefined>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());

  const loadDir = (path: string): void => {
    void qc
      .fetchQuery({
        queryKey: ['docsList', path],
        queryFn: () => api.docsList(path),
        // #36 主修：staleTime 0——任何 loadDir 调用都实际拉取（原 30s staleTime
        // 让「展开后重取」被缓存吞掉，折叠→再展开永不刷新，正是根因 1）
        staleTime: 0,
      })
      .then((entries) => setDirs((d) => ({ ...d, [path]: entries })))
      .catch(() => setDirs((d) => ({ ...d, [path]: [] })));
  };

  // root 首次/切换时：展开根并加载（useEffect 而非 render 体，防 setState 循环）
  useEffect(() => {
    if (root == null) return;
    setExpanded(new Set([root]));
    loadDir(root);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  // #36 主修：面板挂载期（= 文件面板打开）对已展开目录周期重取——
  // 新文件写入后在 DOC_REFRESH_MS 窗口内出现，无需刷新页面。未展开目录
  // 不在 expanded 集合 → 不拉取，大仓懒加载语义不回退。卸载即清理。
  useEffect(() => {
    if (root == null) return;
    const id = window.setInterval(() => {
      expanded.forEach((p) => loadDir(p));
    }, DOC_REFRESH_MS);
    return () => window.clearInterval(id);
  }, [root, expanded, qc]);

  const toggleDir = (path: string): void => {
    // #36 主修：展开总是重取新清单（不再以 dirs[path] === undefined 为首载守卫）——
    // 折叠→再展开即手动刷新；重复展开同目录也每次刷新
    if (!expanded.has(path)) {
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

  if (root == null) {
    return <p className="doc-hint">{t('files.noSession')}</p>;
  }

  const row = (entry: DocEntry, path: string, depth: number): ReactElement => {
    const isExpanded = expanded.has(path);
    const active = activePath === path;
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
              onOpenFile(path, entry.name);
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

  return (
    <nav className="files-tree" aria-label={t('files.treeAria')}>
      <ul className="doc-tree" aria-label={t('files.tree')}>
        {(dirs[root] ?? []).map((e) => row(e, `${root}/${e.name}`, 0))}
        {dirs[root] != null && (dirs[root]?.length ?? 0) === 0 ? (
          <li className="doc-hint">{t('docs.emptyDir')}</li>
        ) : null}
      </ul>
    </nav>
  );
}
