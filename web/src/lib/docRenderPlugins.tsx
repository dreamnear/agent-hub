import { useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import { registerDocRenderer, type DocRenderProps } from './docRender';
import { plantumlSvgUrl } from './plantuml';

/// docs 预览插件示例（2026-10-06 用户需求：type→renderer 工厂以插件式接入）：
/// .puml 源文件 → 经配置的渲染服务出图（未配置/失败回退源码 + 引导）。
/// markdown 内的 ```plantuml 围栏同源能力在 MarkdownView（围栏分流）。

function PlantumlFileDoc({ file }: DocRenderProps): ReactElement {
  const t = useI18n();
  const raw = file.content ?? '';
  const [failed, setFailed] = useState(false);
  const url = plantumlSvgUrl(raw);
  if (url == null || failed) {
    return (
      <>
        <pre className="doc-text">{raw}</pre>
        <p className="doc-hint">{t('md.plantumlNeedConfig')}</p>
      </>
    );
  }
  return (
    <div className="doc-md">
      <img className="md-plantuml" src={url} alt="plantuml diagram" onError={() => setFailed(true)} />
    </div>
  );
}

// 后注册先命中：.puml 在 builtin-text 之前截获
registerDocRenderer({
  id: 'plugin-plantuml-file',
  match: (_kind, name) => /\.puml$/i.test(name),
  component: PlantumlFileDoc,
});
