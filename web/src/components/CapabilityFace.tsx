import { useEffect, useState, type ReactElement } from 'react';
import { useI18n } from '../i18n';
import { plugins, type Contribution, type PluginView } from '../plugins';
import './CapabilityFace.css';

/// 会话侧能力可发现面（B2 批5，需求功能点 6）：当前启用插件的工具 / skill /
/// MCP 清单（来源插件 × 风险标注 × 高危标记）。数据 = /api/plugins（启停即
/// 生效，BR-03）；折叠态默认收起（零插件不渲染——无插件时零回归）。
/// BR-14：固定标注「描述来自插件，非 hub 审定」；描述文本仅展示，不构成任何
/// 扩权依据（闸门在 hub 侧，与本面板零耦合）。
const KIND_ORDER: Contribution['kind'][] = ['tool', 'mcp', 'skill'];

export default function CapabilityFace(): ReactElement | null {
  const t = useI18n();
  const [list, setList] = useState<PluginView[]>([]);

  useEffect(() => {
    let alive = true;
    const fetch = (): void => {
      plugins
        .list()
        .then((all) => {
          if (alive) setList(all.filter((p) => p.enabled && p.uiState === 'enabled'));
        })
        .catch(() => {
          if (alive) setList([]);
        });
    };
    fetch();
    const timer = setInterval(fetch, 30_000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  const caps = list.flatMap((p) =>
    p.contributions
      .filter((c) => KIND_ORDER.includes(c.kind))
      .map((c) => ({ plugin: p, c })),
  );
  if (caps.length === 0) return null;

  const kindLabel = (kind: Contribution['kind']): string =>
    kind === 'tool'
      ? t('cap.tool')
      : kind === 'mcp'
        ? t('cap.mcp')
        : t('cap.skill');

  return (
    <details className="cap-face">
      <summary>
        {t('cap.title', { n: caps.length })}
        <span className="cap-face-untrusted">{t('cap.untrusted')}</span>
      </summary>
      <ul className="cap-face-list">
        {caps.map(({ plugin, c }) => (
          <li key={`${plugin.id}:${c.kind}:${c.id}`} className="cap-face-item">
            <span className={`cap-kind cap-kind--${c.kind}`}>{kindLabel(c.kind)}</span>
            <span className="cap-name">{c.name}</span>
            <span className="cap-plugin">{plugin.name}</span>
            {c.risk === 'high' ? (
              <span className="cap-risk cap-risk--high">{t('cap.highRisk')}</span>
            ) : (
              <span className="cap-risk">{t('cap.readOnly')}</span>
            )}
          </li>
        ))}
      </ul>
    </details>
  );
}
