import { useEffect, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import { plantumlBase, setPlantumlBase } from '../lib/plantuml';

/// 文档渲染分区（2026-10-06 用户需求）：plantuml 渲染服务地址（可自托管，
/// 内网文档不默认外发公共服务器）。即时生效 + localStorage 持久化。
export default function SettingsDocRender(): ReactElement {
  const t = useI18n();
  const [url, setUrl] = useState(plantumlBase());
  // 外部（其他标签页）改动落回本表单：input 事件同步即可，无需跨页广播
  useEffect(() => setUrl(plantumlBase()), []);

  return (
    <section className="set-group" aria-label={t('settings.docs.aria')}>
      <div className="set-row">
        <label className="set-label" htmlFor="set-plantuml-base">
          {t('settings.docs.plantuml')}
        </label>
        <input
          id="set-plantuml-base"
          className="set-select"
          type="url"
          placeholder="https://plantuml.example.com/plantuml"
          value={url}
          onChange={(e) => {
            setUrl(e.target.value);
            setPlantumlBase(e.target.value);
          }}
        />
      </div>
      <p className="set-preview">{t('settings.docs.plantumlHint')}</p>
    </section>
  );
}
