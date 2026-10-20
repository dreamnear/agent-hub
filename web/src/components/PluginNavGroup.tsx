import { useQuery } from '@tanstack/react-query';
import type { ReactElement } from 'react';
import { navItemsOf, plugins, type PluginNavItem } from '../plugins';
import { useI18n } from '../i18n';
import './PluginNavGroup.css';

interface Props {
  onOpenView: (item: PluginNavItem) => void;
  onOpenManager: () => void;
  /** 当前打开的插件视图（高亮用） */
  activePluginId?: string | null;
}

/// 侧栏「插件视图」导航组（ADR-0004 第 2 条 / BR-03 / BR-07）：
/// - 由 `/api/plugins` 数据**动态渲染**（非静态路由表）；
/// - 仅「已安装-启用」与「运行故障」态且有视图贡献的插件出现；停用/卸载后入口消失；
/// - 故障态保留入口 + 红色「故障」徽标（故障本身就是要被观察的事实，不隐藏）；
/// - **零插件时整组不渲染**——导航集合与引入前一致（BR-06 / ADR-0010 第 5 条）。
export default function PluginNavGroup({
  onOpenView,
  onOpenManager,
  activePluginId,
}: Props): ReactElement | null {
  const t = useI18n();
  const { data } = useQuery({
    queryKey: ['plugins'],
    queryFn: plugins.list,
    // 周期扫描同量级（ADR-0002 第 2 条）；写操作后由调用方主动失效
    refetchInterval: 10_000,
  });
  const items = navItemsOf(data ?? []);
  if (items.length === 0) return null;

  return (
    <nav className="plugin-nav" aria-label={t('plugin.nav.group')}>
      <div className="plugin-nav-head">
        <span className="plugin-nav-title">{t('plugin.nav.group')}</span>
        <button type="button" className="plugin-nav-manage" onClick={onOpenManager}>
          {t('plugin.nav.manage')}
        </button>
      </div>
      <ul className="plugin-nav-list">
        {items.map((it) => (
          <li key={`${it.pluginId}:${it.viewId}`}>
            <button
              type="button"
              className={`plugin-nav-item ${activePluginId === it.pluginId ? 'is-active' : ''}`}
              title={
                it.fault
                  ? t('plugin.nav.faultTitle', {
                      name: it.pluginName,
                      reason: it.faultMessage ?? t('plugin.nav.faultUnknown'),
                    })
                  : t('plugin.nav.itemTitle', { name: it.pluginName })
              }
              onClick={() => onOpenView(it)}
            >
              <span className="plugin-nav-label">{it.title}</span>
              <span className="plugin-nav-source">{it.pluginName}</span>
              {it.fault ? <span className="plugin-nav-fault">{t('plugin.state.fault')}</span> : null}
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
