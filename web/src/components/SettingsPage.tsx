import { useEffect, useState, type ReactElement } from 'react';
import HarnessPanel from './HarnessPanel';
import SettingsAppearance from './SettingsAppearance';
import SettingsLanguage from './SettingsLanguage';
import InstancesSection from './settings/InstancesSection';
import { useI18n } from '../i18n';
import './SettingsPage.css';

export type SettingsSectionKey = 'appearance' | 'language' | 'instances' | 'harness' | 'about';

interface Props {
  /** 打开时落哪个分区（侧栏入口默认外观分区） */
  section?: SettingsSectionKey;
  onClose: () => void;
}

/// 构建期注入（vite.config.ts define，零服务端改动）；vitest / 无 define 环境回落 dev
const HUB_VERSION = typeof __HUB_VERSION__ === 'string' ? __HUB_VERSION__ : 'dev';
const HUB_GIT_SHA = typeof __HUB_GIT_SHA__ === 'string' ? __HUB_GIT_SHA__ : 'dev';

function SettingsAbout(): ReactElement {
  const t = useI18n();
  return (
    <section className="set-group">
      <div className="set-row">
        <span className="set-label">{t('settings.about.product')}</span>
        <span className="set-value">agent-hub（claude-view）</span>
      </div>
      <div className="set-row">
        <span className="set-label">{t('settings.about.version')}</span>
        <span className="set-value">{HUB_VERSION}</span>
      </div>
      <div className="set-row">
        <span className="set-label">{t('settings.about.build')}</span>
        <span className="set-value">{HUB_GIT_SHA}</span>
      </div>
    </section>
  );
}

/// 分区注册表（agent-hub-settings B1）：后续分区（D1 实例、C1 语言）按数组加一行
/// 挂载，不改编排代码。render 只负责内容，导航与容器由外壳统一管。
/// label 存 i18n key（C1/C2），渲染时经 useI18n 翻译——语言切换即时生效。
const SECTIONS: { key: SettingsSectionKey; labelKey: string; render: () => ReactElement }[] = [
  { key: 'appearance', labelKey: 'settings.section.appearance', render: () => <SettingsAppearance /> },
  { key: 'language', labelKey: 'settings.section.language', render: () => <SettingsLanguage /> },
  { key: 'instances', labelKey: 'settings.section.instances', render: () => <InstancesSection /> },
  { key: 'harness', labelKey: 'settings.section.harness', render: () => <HarnessPanel /> },
  { key: 'about', labelKey: 'settings.section.about', render: () => <SettingsAbout /> },
];

/// 设置页外壳：全屏容器 + 分区导航（桌面左列 / ≤768px 顶部分段控件）+ 关闭返回。
/// 作为覆盖层渲染在主界面之上——主界面不卸载，关闭后选中会话/抽屉/折叠原样保留。
export default function SettingsPage({ section, onClose }: Props): ReactElement {
  const t = useI18n();
  const [active, setActive] = useState<SettingsSectionKey>(section ?? 'appearance');
  const current = SECTIONS.find((s) => s.key === active) ?? SECTIONS[0];

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="settings" role="dialog" aria-modal="true" aria-label={t('settings.title')}>
      <header className="settings-head">
        <h1 className="settings-title">{t('settings.title')}</h1>
        <button
          type="button"
          className="btn-ghost settings-close"
          onClick={onClose}
          aria-label={t('settings.close')}
        >
          ✕
        </button>
      </header>
      <div className="settings-body">
        <nav className="settings-nav" aria-label={t('settings.nav')}>
          {SECTIONS.map((s) => (
            <button
              key={s.key}
              type="button"
              className={`settings-tab${s.key === current.key ? ' is-active' : ''}`}
              aria-current={s.key === current.key}
              onClick={() => setActive(s.key)}
            >
              {t(s.labelKey)}
            </button>
          ))}
        </nav>
        <section className="settings-content" aria-label={t(current.labelKey)}>
          {current.render()}
        </section>
      </div>
    </div>
  );
}
