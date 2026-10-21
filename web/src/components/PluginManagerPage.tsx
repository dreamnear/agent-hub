import { useEffect, useRef, useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  plugins,
  type AuditReport,
  type DiscoveredItem,
  type FaultRecord,
  type PluginView,
} from '../plugins';
import { useI18n } from '../i18n';
import './PluginManagerPage.css';

/// 生效时机标签（BR-04：一切状态变化文案带生效时机，与实际行为一致）
const EFFECT_LABEL = 'plugin.effect.immediate';

/// 凭据变更后的提示回调（页面挂载时注册；CredentialRow 深层调用，避免层层传参）
let onCredentialChanged: ((msg: string) => void) | null = null;

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/// 状态芯片（五色：启用绿 / 停用灰 / 故障红 / 审计通过待确认绿 / 审计失败红）
function StateChip({ uiState }: { uiState: string }): ReactElement {
  const t = useI18n();
  const labelKey =
    uiState === 'enabled'
      ? 'plugin.state.enabled'
      : uiState === 'disabled'
        ? 'plugin.state.disabled'
        : uiState === 'fault'
          ? 'plugin.state.fault'
          : uiState === 'audit_ok'
            ? 'plugin.state.auditOk'
            : 'plugin.state.auditFailed';
  return <span className={`plugin-chip plugin-chip--${uiState}`}>{t(labelKey)}</span>;
}

/// 单条凭据的「设置 / 清除」入口（2026-10-19 预览反馈增量）：
/// - 只显示「已配置 / 未配置」，**输入框永不回显旧值**（不预填、不回读）；
/// - 保存 = 上行值 → 落 hub 侧 plugin_credentials.json（0600），响应只回状态；
/// - 清除 = 移除该名（幂等）。
function CredentialRow({
  name,
  configured,
  onDone,
}: {
  name: string;
  configured: boolean;
  onDone: (msg: string) => void;
}): ReactElement {
  const t = useI18n();
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);

  const save = (): void => {
    if (value === '') return;
    setBusy(true);
    plugins
      .setCredential(name, value)
      .then(() => {
        setValue(''); // 上行即清：不留在任何可观察位置
        setEditing(false);
        onDone(t('plugin.cred.saved'));
      })
      .catch((e: unknown) => onDone(`${t('plugin.op.failed')}: ${e instanceof Error ? e.message : String(e)}`))
      .finally(() => setBusy(false));
  };

  const clear = (): void => {
    setBusy(true);
    plugins
      .clearCredential(name)
      .then(() => {
        setValue('');
        setEditing(false);
        onDone(t('plugin.cred.cleared'));
      })
      .catch((e: unknown) => onDone(`${t('plugin.op.failed')}: ${e instanceof Error ? e.message : String(e)}`))
      .finally(() => setBusy(false));
  };

  return (
    <div className="plugin-cred-row">
      <span className="plugin-mono plugin-cred-name">{name}</span>
      <span className={`plugin-chip ${configured ? 'plugin-chip--enabled' : 'plugin-chip--disabled'}`}>
        {configured ? t('plugin.cred.configured') : t('plugin.cred.missing')}
      </span>
      {editing ? (
        <>
          <input
            type="password"
            className="plugin-cred-input"
            aria-label={t('plugin.cred.placeholder')}
            placeholder={t('plugin.cred.placeholder')}
            autoComplete="off"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
          <button type="button" className="btn-primary" disabled={busy || value === ''} onClick={save}>
            {t('plugin.cred.save')}
          </button>
          <button
            type="button"
            className="btn-ghost"
            onClick={() => {
              setValue('');
              setEditing(false);
            }}
          >
            {t('plugin.cred.cancel')}
          </button>
        </>
      ) : (
        <>
          <button type="button" className="btn-ghost" onClick={() => setEditing(true)}>
            {t('plugin.cred.set')}
          </button>
          {configured ? (
            <button type="button" className="btn-ghost" disabled={busy} onClick={clear}>
              {t('plugin.cred.clear')}
            </button>
          ) : null}
        </>
      )}
    </div>
  );
}

/// 权限声明面（只列名称与边界；凭据按名引用、值零回显，BR-10/11）
function PermissionFace({
  report,
  view,
  editableCredentials,
}: {
  report?: AuditReport;
  view?: PluginView;
  /** 已安装卡的凭据行提供「设置/清除」入口（审计卡只读展示） */
  editableCredentials?: boolean;
}): ReactElement {
  const t = useI18n();
  const data = report?.reach.data ?? view?.permissions.data ?? [];
  const creds = report?.reach.credentials ?? view?.permissions.credentials ?? [];
  const high = report?.reach.highRisk.map((h) => h.label) ?? view?.permissions.highRisk ?? [];
  const api = report?.reach.api ?? view?.permissions.api ?? [];
  const backend = report?.reach.backend;
  return (
    <dl className="plugin-perm">
      <dt>{t('plugin.perm.data')}</dt>
      <dd>{data.length > 0 ? data.join('、') : t('plugin.perm.none')}</dd>
      <dt>{t('plugin.perm.credentials')}</dt>
      <dd>
        {creds.length > 0 ? (
          <div className="plugin-cred-list">
            {creds.map((c) =>
              editableCredentials === true && onCredentialChanged != null ? (
                <CredentialRow
                  key={c.name}
                  name={c.name}
                  configured={c.configured}
                  onDone={onCredentialChanged}
                />
              ) : (
                <span key={c.name} className="plugin-cred-plain">
                  {c.name}（{c.configured ? t('plugin.cred.configured') : t('plugin.cred.missing')}）
                </span>
              ),
            )}
            {editableCredentials === true ? (
              <p className="plugin-hint">{t('plugin.cred.hint')}</p>
            ) : null}
          </div>
        ) : (
          t('plugin.perm.none')
        )}
      </dd>
      <dt>{t('plugin.perm.highRisk')}</dt>
      <dd>{high.length > 0 ? high.join('、') : t('plugin.perm.none')}</dd>
      {api.length > 0 ? (
        <>
          <dt>{t('plugin.perm.api')}</dt>
          <dd>{api.join('、')}</dd>
        </>
      ) : null}
      {backend != null ? (
        <>
          <dt>{t('plugin.perm.backend')}</dt>
          <dd className="plugin-mono">{backend.join(' ')}</dd>
        </>
      ) : null}
    </dl>
  );
}

/// 贡献物清单
function ContributionList({ report, view }: { report?: AuditReport; view?: PluginView }): ReactElement {
  const t = useI18n();
  const items = report?.contributions ?? view?.contributions ?? [];
  if (items.length === 0) return <p className="plugin-empty-line">{t('plugin.contributes.none')}</p>;
  return (
    <ul className="plugin-contrib">
      {items.map((c) => (
        <li key={`${c.kind}:${c.id}`}>
          <span className="plugin-contrib-kind">{t(`plugin.kind.${c.kind}`)}</span>
          <span className="plugin-contrib-name">{c.name}</span>
          {c.risk === 'high' ? (
            <span className="plugin-contrib-high">{t('plugin.contributes.high')}</span>
          ) : null}
          {c.path != null ? <span className="plugin-mono plugin-contrib-path">{c.path}</span> : null}
          {c.description != null ? (
            <span className="plugin-contrib-desc">{c.description}</span>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

function IssueList({ kind, issues }: { kind: 'error' | 'warning'; issues: { field: string; rule: string; message: string }[] }): ReactElement {
  const t = useI18n();
  if (issues.length === 0) return <span />;
  return (
    <ul className={`plugin-issues plugin-issues--${kind}`}>
      {issues.map((i) => (
        <li key={`${i.field}:${i.rule}:${i.message}`}>
          <span className="plugin-mono">
            {i.field} · {i.rule}
          </span>
          <span>{i.message}</span>
          {kind === 'error' ? null : <span className="plugin-warn-tag">{t('plugin.audit.warning')}</span>}
        </li>
      ))}
    </ul>
  );
}

interface InstallDialog {
  item: DiscoveredItem;
}

interface UninstallDialog {
  plugin: PluginView;
}

/// 插件管理视图（ui.md 层级 1）：L0 新发现/待处理 → L1 已安装清单 → L2 故障记录。
/// 入口 = 设置页「插件管理」分区（设计中的「系统 · 插件管理」），零插件时主侧栏
/// 不出现任何新增入口（BR-06：导航集合与引入前一致）。
export default function PluginManagerPage(): ReactElement {
  const t = useI18n();
  const qc = useQueryClient();
  const [installFor, setInstallFor] = useState<InstallDialog | null>(null);
  const [uninstallFor, setUninstallFor] = useState<UninstallDialog | null>(null);
  const [purgeData, setPurgeData] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);
  const scannedOnce = useRef(false);

  const list = useQuery({ queryKey: ['plugins'], queryFn: plugins.list });
  const discovered = useQuery({
    queryKey: ['plugins', 'discovered'],
    queryFn: plugins.discovered,
  });
  const records = useQuery({
    queryKey: ['plugins', expanded ?? '', 'records'],
    queryFn: () => plugins.records(expanded ?? ''),
    enabled: expanded != null,
  });

  // 打开管理页即刷新一次发现面（等价原型「刷新」按钮的确定性；周期扫描另有 10s tick）
  useEffect(() => {
    if (scannedOnce.current) return;
    scannedOnce.current = true;
    void plugins
      .scan()
      .then(() => {
        void qc.invalidateQueries({ queryKey: ['plugins'] });
      })
      .catch(() => {
        /* 扫描失败不阻断页面：既有清单照常显示 */
      });
  }, [qc]);

  useEffect(() => {
    if (toast == null) return;
    const timer = window.setTimeout(() => setToast(null), 4000);
    return () => window.clearTimeout(timer);
  }, [toast]);

  // 凭据变更回调注册（含卸载清理；保存/清除后刷新清单以更新「已配置」态）
  useEffect(() => {
    onCredentialChanged = (msg: string) => {
      setToast(msg);
      refresh();
    };
    return () => {
      onCredentialChanged = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- refresh 依赖 qc，稳定
  }, []);

  const refresh = (): void => {
    void qc.invalidateQueries({ queryKey: ['plugins'] });
  };

  const run = async (key: string, fn: () => Promise<string>): Promise<void> => {
    setBusy(key);
    try {
      const msg = await fn();
      setToast(msg);
    } catch (e: unknown) {
      setToast(`${t('plugin.op.failed')}: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(null);
      refresh();
    }
  };

  const doInstall = (item: DiscoveredItem): void => {
    void run(item.id, async () => {
      const r = await plugins.install(item.id, item.report.contentHash);
      // BR-04：一切状态变化 toast 首段固定「已生效 · 立即」（与启停/卸载同口径）
      return r.alreadyInstalled
        ? `${t(EFFECT_LABEL)} · ${t('plugin.toast.alreadyInstalled', { name: r.plugin.name })}`
        : `${t(EFFECT_LABEL)} · ${t('plugin.toast.installed', { name: r.plugin.name })}`;
    });
    setInstallFor(null);
  };

  const doToggle = (p: PluginView): void => {
    void run(p.id, async () => {
      if (p.enabled) {
        const r = await plugins.disable(p.id);
        return `${t(EFFECT_LABEL)} · ${t('plugin.toast.disabled', { name: r.plugin.name })}`;
      }
      const r = await plugins.enable(p.id);
      return `${t(EFFECT_LABEL)} · ${t('plugin.toast.enabled', { name: r.plugin.name })}`;
    });
  };

  const doRetry = (p: PluginView): void => {
    void run(p.id, async () => {
      const r = await plugins.retry(p.id);
      return r.plugin.uiState === 'enabled'
        ? `${t(EFFECT_LABEL)} · ${t('plugin.toast.retried', { name: r.plugin.name })}`
        : t('plugin.toast.retryFailed', { name: r.plugin.name });
    });
  };

  const doUninstall = (): void => {
    const p = uninstallFor?.plugin;
    if (p == null) return;
    setUninstallFor(null);
    void run(p.id, async () => {
      const r = await plugins.uninstall(p.id, purgeData);
      return `${t(EFFECT_LABEL)} · ${t('plugin.toast.uninstalled', { name: p.name })} · ${
        r.regression.passed ? t('plugin.toast.regressionOk') : t('plugin.toast.regressionFailed')
      }`;
    });
  };

  const items = discovered.data?.items ?? [];
  const installed = list.data ?? [];

  return (
    <div className="plugin-manager">
      {/* L0 新发现 · 待处理区（置顶） */}
      {items.length > 0 ? (
        <section className="plugin-l0" aria-label={t('plugin.l0.title')}>
          <h3 className="plugin-l0-title">{t('plugin.l0.title')}</h3>
          {items.map((item) => {
            const failed = item.status === 'audit_failed' || item.report.errors.length > 0;
            return (
              <article
                key={`${item.id}:${item.report.contentHash}`}
                className={`plugin-card ${failed ? 'plugin-card--failed' : ''}`}
              >
                <header className="plugin-card-head">
                  <span className="plugin-card-name">
                    {item.report.pluginId !== '' ? item.report.pluginId : item.dirName}
                  </span>
                  <StateChip uiState={failed ? 'audit_failed' : item.status === 'update' ? 'audit_ok' : 'audit_ok'} />
                  {item.status === 'update' ? (
                    <span className="plugin-chip plugin-chip--update">{t('plugin.state.update')}</span>
                  ) : null}
                  {item.sourceKind === 'transient' ? (
                    <span className="plugin-chip plugin-chip--transient">{t('plugin.state.transient')}</span>
                  ) : null}
                  <span className="plugin-card-path plugin-mono">{item.path}</span>
                </header>

                {failed ? (
                  <IssueList kind="error" issues={item.report.errors} />
                ) : (
                  <>
                    <ContributionList report={item.report} />
                    <PermissionFace report={item.report} />
                    <p className="plugin-hint">{t('plugin.install.noSandbox')}</p>
                    {item.report.warnings.length > 0 ? (
                      <IssueList kind="warning" issues={item.report.warnings} />
                    ) : null}
                  </>
                )}

                <footer className="plugin-card-actions">
                  {failed ? (
                    <>
                      <span className="plugin-hint">{t('plugin.audit.failedHint')}</span>
                      <button type="button" className="btn-ghost" onClick={refresh}>
                        {t('plugin.action.rescan')}
                      </button>
                    </>
                  ) : item.status === 'update' ? (
                    <>
                      {/* B2 无更新流（B4 交付）：对已安装插件点安装必被 hash 判定拒绝
                          （400）——不再渲染死按钮，明示「旧版继续可用」（ADR-0007 保持旧版） */}
                      <span className="plugin-hint">{t('plugin.update.deferred')}</span>
                      <button type="button" className="btn-ghost" onClick={refresh}>
                        {t('plugin.action.rescan')}
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        type="button"
                        className="btn-primary"
                        disabled={busy != null}
                        onClick={() => setInstallFor({ item })}
                      >
                        {t('plugin.action.install')}
                      </button>
                      <button type="button" className="btn-ghost" onClick={refresh}>
                        {t('plugin.action.keepUninstalled')}
                      </button>
                    </>
                  )}
                </footer>
              </article>
            );
          })}
        </section>
      ) : null}

      {/* L1 已安装清单 */}
      <section aria-label={t('plugin.l1.title')}>
        <h3 className="plugin-l1-title">
          {t('plugin.l1.title')}
          <span className="plugin-scan-at">
            {discovered.data?.lastScanAt != null
              ? t('plugin.lastScan', { at: new Date(discovered.data.lastScanAt).toLocaleTimeString() })
              : ''}
          </span>
        </h3>
        {installed.length === 0 ? (
          // 空态（BR-06）：零插件时说明主体行为与安装前一致
          <p className="plugin-empty">{t('plugin.empty')}</p>
        ) : (
          installed.map((p) => (
            <article
              key={p.id}
              className={`plugin-card ${p.uiState === 'fault' ? 'plugin-card--fault' : ''} ${
                p.uiState === 'disabled' ? 'plugin-card--disabled' : ''
              }`}
            >
              <header className="plugin-card-head">
                <span className="plugin-card-name">{p.name}</span>
                <span className="plugin-card-version">v{p.version}</span>
                <StateChip uiState={p.uiState} />
                <span className="plugin-card-assets">{formatBytes(p.assetBytes)}</span>
              </header>

              {/* 故障原因就近展示（L2 首行红字） */}
              {p.uiState === 'fault' && p.lastFault != null ? (
                <p className="plugin-fault-line" role="alert">
                  {p.lastFault.message}
                </p>
              ) : null}

              <ContributionList view={p} />
              <PermissionFace view={p} editableCredentials />

              <footer className="plugin-card-actions">
                {p.uiState === 'fault' ? (
                  <button
                    type="button"
                    className="btn-primary"
                    disabled={busy != null}
                    onClick={() => doRetry(p)}
                  >
                    {t('plugin.action.retry')}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="btn-primary"
                    disabled={busy != null}
                    onClick={() => doToggle(p)}
                  >
                    {p.enabled ? t('plugin.action.disable') : t('plugin.action.enable')}
                  </button>
                )}
                <button
                  type="button"
                  className="btn-ghost"
                  onClick={() => setExpanded(expanded === p.id ? null : p.id)}
                >
                  {expanded === p.id ? t('plugin.action.hideRecords') : t('plugin.action.records')}
                </button>
                <button
                  type="button"
                  className="btn-danger"
                  disabled={busy != null}
                  onClick={() => {
                    setPurgeData(false);
                    setUninstallFor({ plugin: p });
                  }}
                >
                  {t('plugin.action.uninstall')}
                </button>
              </footer>

              {/* L2 故障记录（含越界违规与自动恢复记录） */}
              {expanded === p.id ? (
                <div className="plugin-records">
                  {records.isLoading ? (
                    <p className="plugin-empty-line">{t('plugin.records.loading')}</p>
                  ) : (records.data?.records ?? []).length === 0 ? (
                    <p className="plugin-empty-line">{t('plugin.records.empty')}</p>
                  ) : (
                    <table>
                      <thead>
                        <tr>
                          <th>{t('plugin.records.at')}</th>
                          <th>{t('plugin.records.kind')}</th>
                          <th>{t('plugin.records.message')}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {(records.data?.records ?? []).map((r: FaultRecord, i: number) => (
                          <tr key={`${r.at}:${i}`}>
                            <td className="plugin-mono">{new Date(r.at).toLocaleString()}</td>
                            <td>{t(`plugin.record.${r.kind}`)}</td>
                            <td>{r.message}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  )}
                </div>
              ) : null}
            </article>
          ))
        )}
      </section>

      {/* 安装确认弹窗（审计摘要 + 无沙箱诚实声明） */}
      {installFor != null ? (
        <div className="dialog-backdrop" role="dialog" aria-modal="true">
          <div className="dialog-card plugin-dialog">
            <h2>{t('plugin.install.title', { name: installFor.item.report.pluginId || installFor.item.dirName })}</h2>
            <ContributionList report={installFor.item.report} />
            <PermissionFace report={installFor.item.report} />
            <p className="plugin-hint">{t('plugin.install.noSandbox')}</p>
            {installFor.item.report.warnings.length > 0 ? (
              <IssueList kind="warning" issues={installFor.item.report.warnings} />
            ) : null}
            <div className="dialog-actions">
              <button type="button" className="btn-ghost" onClick={() => setInstallFor(null)}>
                {t('plugin.action.keepUninstalled')}
              </button>
              <button
                type="button"
                className="btn-primary"
                onClick={() => doInstall(installFor.item)}
              >
                {t('plugin.action.confirmInstall')}
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {/* 卸载确认弹窗（自持数据保留策略：默认保留，可改选清除） */}
      {uninstallFor != null ? (
        <div className="dialog-backdrop" role="dialog" aria-modal="true">
          <div className="dialog-card plugin-dialog">
            <h2>{t('plugin.uninstall.title', { name: uninstallFor.plugin.name })}</h2>
            <p>{t('plugin.uninstall.clearNote')}</p>
            <div className="plugin-radio">
              <label>
                <input
                  type="radio"
                  name="purgeData"
                  checked={!purgeData}
                  onChange={() => setPurgeData(false)}
                />
                {t('plugin.uninstall.keepData')}
              </label>
              <label>
                <input
                  type="radio"
                  name="purgeData"
                  checked={purgeData}
                  onChange={() => setPurgeData(true)}
                />
                {t('plugin.uninstall.purgeData')}
              </label>
            </div>
            <p className="plugin-hint">{t('plugin.uninstall.dirKept')}</p>
            <div className="dialog-actions">
              <button type="button" className="btn-ghost" onClick={() => setUninstallFor(null)}>
                {t('dialog.cancel')}
              </button>
              <button type="button" className="btn-danger" onClick={doUninstall}>
                {t('plugin.action.confirmUninstall')}
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {/* 生效提示（BR-04：首段固定「已生效 · 立即」） */}
      {toast != null ? (
        <div className="plugin-toast" role="status" aria-live="polite">
          {toast}
        </div>
      ) : null}
    </div>
  );
}
