import type { ReactElement } from 'react';
import { useI18n, setLangChoice, getLangChoice, type LangChoice } from '../i18n';

/// 语言分区（agent-hub-settings C1）：跟随系统 / 中文 / English 三选。
/// 切换即时生效（i18n store 驱动全树重渲）+ localStorage 持久化 + 默认跟系统。
const CHOICES: { key: LangChoice; labelKey: string }[] = [
  { key: 'system', labelKey: 'settings.language.followSystem' },
  { key: 'zh', labelKey: 'settings.language.zh' },
  { key: 'en', labelKey: 'settings.language.en' },
];

export default function SettingsLanguage(): ReactElement {
  const t = useI18n();
  const current: LangChoice = getLangChoice();

  return (
    <section className="set-group" aria-label={t('settings.language.aria')}>
      <div className="set-row">
        <span className="set-label" id="set-lang-label">
          {t('settings.language.label')}
        </span>
        <div className="set-seg" role="group" aria-labelledby="set-lang-label">
          {CHOICES.map((o) => {
            const active = o.key === current;
            return (
              <button
                key={o.key}
                type="button"
                className={`set-seg-btn${active ? ' is-active' : ''}`}
                aria-pressed={active}
                onClick={() => setLangChoice(o.key)}
              >
                {t(o.labelKey)}
              </button>
            );
          })}
        </div>
      </div>
    </section>
  );
}
