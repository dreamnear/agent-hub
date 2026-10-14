import { useQuery } from '@tanstack/react-query';
import { type ReactElement } from 'react';
import { api } from '../api';
import { useI18n } from '../i18n';
import { resolveDocRenderer } from '../lib/docRender';
import '../lib/docRenderPlugins';

interface Props {
  /** 文件绝对路径（白名单内，docs/file 端点语义） */
  path: string;
}

/// 文件内容只读渲染（agent-hub-files-panel F2）：docs/file 拉取 → type→renderer
/// 工厂（markdown/plantuml/图片/文本/binary 全类型，插件注册扩展）。从原 DocViewer
/// 预览段抽出——文档 Tab 退役后由主区文件页签（FileTabPanel）承载。
export default function FileContentView({ path }: Props): ReactElement {
  const t = useI18n();
  const file = useQuery({
    queryKey: ['docsFile', path],
    queryFn: () => api.docsFile(path),
  });

  if (file.isLoading) {
    return <p className="doc-hint">{t('docs.loading')}</p>;
  }
  if (file.isError) {
    return <p className="doc-hint doc-hint--err">{String(file.error)}</p>;
  }
  if (file.data == null) {
    return <p className="doc-hint" />;
  }
  const R = resolveDocRenderer(file.data.kind, file.data.name);
  return R != null ? <R.component file={file.data} path={path} api={api} /> : <p className="doc-hint" />;
}
