import type { ReactElement } from 'react';
import { FONT_FAMILIES, FONT_SCALES, useAppearance } from '../hooks/useAppearance';
import { useTheme, type ThemeMode } from '../hooks/useTheme';
import { useI18n } from '../i18n';

/// 外观分区（agent-hub-settings B2 + B3）：主题三态 / 界面字体族 / 界面字号缩放。
/// 三项都是即时生效 + localStorage 持久化（各自 hook 内闭环），本组件只提供入口。
export default function SettingsAppearance(): ReactElement {
  const t = useI18n();
  const { mode, setMode } = useTheme();
  const { family, scale, setFamily, setScale } = useAppearance();

  const THEME_OPTIONS: { key: ThemeMode; labelKey: string; glyph: string }[] = [
    { key: 'auto', labelKey: 'settings.theme.auto', glyph: '◐' },
    { key: 'light', labelKey: 'settings.theme.light', glyph: '☀︎' },
    { key: 'dark', labelKey: 'settings.theme.dark', glyph: '☾' },
  ];

  return (
    <section className="set-group" aria-label={t('settings.appearance.aria')}>
      <div className="set-row">
        <span className="set-label" id="set-theme-label">
          {t('settings.theme')}
        </span>
        <div className="set-seg" role="group" aria-labelledby="set-theme-label">
          {THEME_OPTIONS.map((o) => (
            <button
              key={o.key}
              type="button"
              className={`set-seg-btn${mode === o.key ? ' is-active' : ''}`}
              aria-pressed={mode === o.key}
              onClick={() => setMode(o.key)}
            >
              <span aria-hidden="true">{o.glyph}</span> {t(o.labelKey)}
            </button>
          ))}
        </div>
      </div>

      <div className="set-row">
        <label className="set-label" htmlFor="set-font-family">
          {t('settings.fontFamily')}
        </label>
        <select
          id="set-font-family"
          className="set-select"
          value={family}
          onChange={(e) => setFamily(e.target.value as typeof family)}
        >
          {FONT_FAMILIES.map((f) => (
            <option key={f.key} value={f.key}>
              {t(`settings.fontFamily.${f.key}`)}
            </option>
          ))}
        </select>
      </div>

      <div className="set-row">
        <span className="set-label" id="set-font-scale-label">
          {t('settings.fontScale')}
        </span>
        <div className="set-seg" role="group" aria-labelledby="set-font-scale-label">
          {FONT_SCALES.map((s) => (
            <button
              key={s}
              type="button"
              className={`set-seg-btn${scale === s ? ' is-active' : ''}`}
              aria-pressed={scale === s}
              onClick={() => setScale(s)}
            >
              {Math.round(s * 100)}%
            </button>
          ))}
        </div>
      </div>

      <p className="set-preview">{t('settings.preview')}</p>
    </section>
  );
}
