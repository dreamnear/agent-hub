import { useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, type HarnessEntry } from '../api';
import { useI18n } from '../i18n';
import './HarnessPanel.css';

/// harness 发现区（agent-hub-settings 批A 需求5/6）：服务端探测本机已安装的
/// coding agent CLI（已知清单 × PATH + 常见安装位），列出路径与版本。
/// - 需求6 死亡过滤：默认只显示存活项，「显示全部（含失效）」为次级排查入口；
/// - 需求5 一键加入：ACP 类写入 `[[acp.agents]]`（服务端同时挂运行时清单，免重启）。
///   CLI 类（claude/codex/gemini）不走 ACP 驱动，hub 经各自原生链路使用，无加入动作。
/// 外壳可嵌入：纯 section，不自带 backdrop/滚动容器/路由（设置页框架落地后原样搬入），
/// 开关、过滤、加入动作全部在面板内部闭环。
export default function HarnessPanel(): ReactElement {
  const qc = useQueryClient();
  const t = useI18n();
  const [showAll, setShowAll] = useState(false);
  const [busy, setBusy] = useState('');
  const [err, setErr] = useState('');
  const { data } = useQuery({
    queryKey: ['harness'],
    queryFn: () => api.listHarnesses(),
    staleTime: 30_000,
  });

  const all = data?.harnesses ?? [];
  // 死亡过滤：alive=false（路径失效/不可执行）默认不出现
  const visible = showAll ? all : all.filter((h) => h.alive);
  const deadCount = all.length - visible.length;

  const add = async (h: HarnessEntry): Promise<void> => {
    setErr('');
    setBusy(h.name);
    try {
      await api.addHarness(h.name, h.path);
      await qc.invalidateQueries({ queryKey: ['harness'] });
    } catch (e) {
      setErr(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy('');
    }
  };

  return (
    <section className="harness" aria-label={t('harness.aria')}>
      <div className="harness-head">
        <span className="harness-title">{t('harness.title')}</span>
        <button
          type="button"
          className="inst-btn"
          onClick={() => setShowAll((v) => !v)}
          aria-pressed={showAll}
        >
          {showAll ? t('harness.showAlive') : t('harness.showAll', { n: deadCount })}
        </button>
      </div>
      <p className="inst-sub">{t('harness.sub')}</p>
      {err ? <p className="dialog-error">{err}</p> : null}
      <div className="harness-list">
        {visible.length === 0 ? (
          <p className="inst-empty">{t('harness.empty')}</p>
        ) : (
          visible.map((h) => (
            <div className={`harness-row${h.alive ? '' : ' harness-row--dead'}`} key={h.name}>
              <div className="harness-info">
                <span className="harness-name">
                  {h.name}
                  <span className="inst-row-mode">{h.kind}</span>
                  {!h.alive ? <span className="harness-dead">{t('harness.dead')}</span> : null}
                  {h.configured ? <span className="harness-configured">{t('harness.configured')}</span> : null}
                </span>
                <span className="harness-path">
                  {h.path}
                  {h.version ? ` · ${h.version}` : ''}
                </span>
              </div>
              {h.kind === 'acp' && !h.configured ? (
                <div className="inst-row-actions">
                  <button
                    type="button"
                    className="inst-btn"
                    disabled={!h.alive || busy === h.name}
                    onClick={() => void add(h)}
                  >
                    {t('harness.add')}
                  </button>
                </div>
              ) : null}
            </div>
          ))
        )}
      </div>
    </section>
  );
}
