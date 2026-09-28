import { useEffect, useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, storeToken } from '../../api';
import type { InstallPlan, InstallResult, RemoteProbe } from '../../api';
import { fetchInstances } from '../../instances';
import type { InstanceConfig, InstanceMode, SshAuth, SshConfig } from '../../types';
import { t as translate, useI18n } from '../../i18n';
import './InstancesSection.css';

/// 空 ssh 配置（模式切换时初始化）
function emptySsh(): SshConfig {
  return { host: '', port: 22, user: '', auth: 'key_path', keyPath: null, password: null };
}

/// 空实例草稿（新建默认 direct）
function draft(): InstanceConfig {
  return {
    id: '',
    name: '',
    mode: 'direct',
    url: null,
    token: null,
    ssh: null,
    remotePort: null,
    localPort: null,
  };
}

/// 认证方式显示名（i18n，渲染期翻译）
const AUTH_KEYS: SshAuth[] = ['key_path', 'authsock', 'password'];

/// 远程自动安装流程态（D4）：探测 → 确认面板 → 执行 → 探活 → 可用。
/// 确认按钮是唯一触发 installExecute 的地方（未确认零请求）。
type InstallFlow = {
  phase: 'probe-fail' | 'not-installed' | 'confirm' | 'running' | 'success' | 'failed';
  probe?: RemoteProbe;
  plan?: InstallPlan;
  result?: InstallResult;
  note?: string;
};

/// 探测结果短标签（行内展示）——文案渲染集中在 InstallFlowView，见 flow.phase 分支


/// 设置页「实例」分区（agent-hub-settings D1，自 InstanceSettings 弹层迁入，功能等价）：
/// 实例 CRUD（direct + ssh-tunnel 两型）、连接测试、隧道状态展示。
/// URL 红线的拒绝文案透出（服务端 400 纯文本直接展示）；token 显示用占位符防误读，
/// 改 token 时前端只更新实例配置里的 token 字段（不强制重输）。
export default function InstancesSection(): ReactElement {
  const qc = useQueryClient();
  const t = useI18n();
  const { data: instances = [] } = useQuery<InstanceConfig[]>({
    queryKey: ['instances'],
    queryFn: fetchInstances,
    staleTime: 30_000,
  });
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);
  // 正在编辑：null = 列表视图；'new' = 新建；id = 编辑该实例
  const [editing, setEditing] = useState<InstanceConfig | 'new' | null>(null);
  const [form, setForm] = useState<InstanceConfig>(draft());
  const [sshForm, setSshForm] = useState<SshConfig>(emptySsh());
  const [remotePort, setRemotePort] = useState('');
  // 隧道状态缓存（实例 id → status），列表轮询展示
  const [tunnelStatus, setTunnelStatus] = useState<Record<string, { running: boolean; state: string; retries: number }>>({});
  const [tunnelError, setTunnelError] = useState<Record<string, string>>({});
  // 远程自动安装流程（D4，ssh-tunnel 实例）：实例 id → 流程态
  const [installs, setInstalls] = useState<Record<string, InstallFlow | undefined>>({});
  const setFlow = (id: string, flow: InstallFlow | undefined): void =>
    setInstalls((m) => ({ ...m, [id]: flow }));
  // D5：direct 实例测试失败 → install-manual 命令清单（可复制，不自动执行）
  const [manual, setManual] = useState<Record<string, InstallPlan | undefined>>({});
  const [copied, setCopied] = useState<Record<string, boolean>>({});

  // 新建/编辑打开 → 复位表单态
  const startNew = (): void => {
    setErr('');
    setEditing('new');
    setForm(draft());
    setSshForm(emptySsh());
    setRemotePort('');
  };
  const startEdit = (inst: InstanceConfig): void => {
    setErr('');
    setEditing(inst);
    setForm({ ...inst });
    setSshForm(inst.ssh ? { ...inst.ssh } : emptySsh());
    setRemotePort(inst.remotePort != null ? String(inst.remotePort) : '');
  };
  const cancel = (): void => {
    setErr('');
    setEditing(null);
  };

  // 有效 token 占位（P3 惯例）：真实 token 不回填进输入框，只显示"已保存"占位
  const hasToken = (inst: InstanceConfig | 'new' | null): boolean =>
    inst != null && inst !== 'new' && !!inst.token;

  // ssh-tunnel 实例隧道状态轮询（列表展示运行/重连/失败）
  useEffect(() => {
    const id = setInterval(() => {
      for (const inst of instances) {
        if (inst.mode !== 'ssh-tunnel') continue;
        api
          .tunnelStatus(inst.id)
          .then((s) => setTunnelStatus((m) => ({ ...m, [inst.id]: { running: s.running, state: s.state, retries: s.retries } })))
          .catch(() => {});
      }
    }, 3000);
    return () => clearInterval(id);
  }, [instances]);

  const testConnection = async (inst: InstanceConfig): Promise<void> => {
    setErr('');
    setBusy(true);
    try {
      // ssh-tunnel：先确保本地端口（start），再经 localhost:localPort 打 /health；
      // direct：直接打配置 url /health。
      let base = inst.url ?? '';
      if (inst.mode === 'ssh-tunnel') {
        const { localPort } = await api.tunnelStart(inst.id);
        base = `http://127.0.0.1:${localPort}`;
      }
      const res = await fetch(`${base}/health`);
      if (!res.ok) throw new Error(translate('inst.testFailed', { code: res.status }));
      alert(translate('inst.testOk', { base }));
    } catch (e) {
      const msg = String(e instanceof Error ? e.message : e);
      setErr(msg);
      // D5：direct 失败（远程不可达 = 未装/未启动）→ 拉取可复制的手动命令清单；
      // 无 SSH 通道不提供自动执行。清单拉取失败不阻断错误展示。
      if (inst.mode === 'direct') {
        api
          .installManual()
          .then((plan) => setManual((m) => ({ ...m, [inst.id]: plan })))
          .catch(() => {});
      }
    } finally {
      setBusy(false);
    }
  };

  // ===== D4：一键安装流程（探测 → 提示 → 命令清单确认 → 执行 → 探活 → 可用）=====

  const runProbe = async (inst: InstanceConfig): Promise<void> => {
    setFlow(inst.id, { phase: 'running', note: translate('inst.probing') });
    try {
      const probe = await api.remoteProbe(inst.id);
      if (probe.reason === 'not_installed') {
        setFlow(inst.id, { phase: 'not-installed', probe });
      } else if (probe.reason === 'installed') {
        setFlow(inst.id, { phase: 'success', probe, note: translate('inst.remoteOk') });
      } else {
        // 端口被占 / SSH 不可达：原样透出可读原因
        setFlow(inst.id, { phase: 'probe-fail', probe });
      }
    } catch (e) {
      setFlow(inst.id, { phase: 'failed', note: String(e instanceof Error ? e.message : e) });
    }
  };

  // 拉 plan（只生成不执行）→ 确认面板展示命令清单；确认按钮前不发起 install 请求
  const openPlan = async (inst: InstanceConfig): Promise<void> => {
    setErr('');
    try {
      const plan = await api.installPlan(inst.id);
      setFlow(inst.id, { phase: 'confirm', plan });
    } catch (e) {
      setFlow(inst.id, { phase: 'failed', note: String(e instanceof Error ? e.message : e) });
    }
  };

  const confirmInstall = async (inst: InstanceConfig): Promise<void> => {
    const flow = installs[inst.id];
    if (!flow?.plan) return;
    setErr('');
    setFlow(inst.id, { phase: 'running', note: translate('inst.installing') });
    let result: InstallResult | undefined;
    try {
      result = await api.installExecute(inst.id, {
        planId: flow.plan.planId,
        planHash: flow.plan.planHash,
        confirm: true,
      });
      if (!result.ok) {
        setFlow(inst.id, { phase: 'failed', plan: flow.plan, result });
        return;
      }
      // token 已由服务端回读落实例配置：回填浏览器存储（实例转可用的前提）并刷新
      const fresh = await fetchInstances();
      const token = fresh.find((i) => i.id === inst.id)?.token;
      if (token) storeToken(token, inst.id);
      qc.invalidateQueries({ queryKey: ['instances'] });
      // 自动探活：启动隧道 → 打 /health。隧道 ssh 进程建立有亚秒级延迟
      //（E2E 实测：start 返回后立刻打会 000），重试 3 次兜竞态
      const { localPort } = await api.tunnelStart(inst.id);
      let healthOk = false;
      let lastStatus = 0;
      for (let attempt = 0; attempt < 3 && !healthOk; attempt++) {
        await new Promise((r) => setTimeout(r, attempt * 500));
        const health = await fetch(`http://127.0.0.1:${localPort}/health`);
        lastStatus = health.status;
        healthOk = health.ok;
      }
      if (!healthOk) throw new Error(translate('inst.healthFailed', { code: lastStatus }));
      qc.invalidateQueries({ queryKey: ['instances'] });
      setFlow(inst.id, {
        phase: 'success',
        result,
        note: result.tokenStored ? translate('inst.doneToken') : translate('inst.doneHealth'),
      });
    } catch (e) {
      setFlow(inst.id, {
        phase: 'failed',
        plan: flow.plan,
        result,
        note: String(e instanceof Error ? e.message : e),
      });
    }
  };

  // D5：复制 direct 手动命令清单（剪贴板内容与清单逐字一致）
  const copyManual = async (inst: InstanceConfig): Promise<void> => {
    const plan = manual[inst.id];
    if (!plan) return;
    await navigator.clipboard.writeText(plan.steps.map((s) => s.display).join('\n'));
    setCopied((m) => ({ ...m, [inst.id]: true }));
    setTimeout(() => setCopied((m) => ({ ...m, [inst.id]: false })), 1500);
  };

  const save = async (): Promise<void> => {
    setErr('');
    setBusy(true);
    try {
      // 组装实例配置：ssh-tunnel 时把 ssh/remotePort 并入
      const payload: InstanceConfig = {
        ...form,
        id: form.id.trim() || crypto.randomUUID(),
        name: form.name.trim(),
        url: form.mode === 'direct' ? form.url?.trim() || null : null,
        token: form.token?.trim() || null,
        ssh: form.mode === 'ssh-tunnel' ? { ...sshForm, keyPath: sshForm.keyPath?.trim() || null, password: sshForm.password?.trim() || null } : null,
        remotePort: form.mode === 'ssh-tunnel' && remotePort ? Number(remotePort) : null,
      };
      if (payload.mode === 'direct' && payload.url == null) {
        setErr(translate('inst.err.directUrl'));
        return;
      }
      // https 预校验（agent-hub-settings C3 裁定：前端可预判的服务端 400 改本地文案）
      // 规则镜像服务端 instances.rs：loopback 主机豁免，其余 direct 必须 https
      if (payload.mode === 'direct' && payload.url) {
        let violatesHttps = false;
        try {
          const u = new URL(payload.url);
          const loopback = ['localhost', '127.0.0.1', '[::1]', '::1'].includes(u.hostname);
          violatesHttps = u.protocol !== 'https:' && !loopback;
        } catch {
          // URL 解析失败不拦截，交服务端判定
        }
        if (violatesHttps) {
          setErr(translate('inst.urlHint'));
          return;
        }
      }
      if (payload.mode === 'ssh-tunnel') {
        if (!payload.ssh?.host.trim() || !payload.ssh.user.trim()) {
          setErr(translate('inst.err.sshRequired'));
          return;
        }
        if (payload.remotePort == null) {
          setErr(translate('inst.err.remotePort'));
          return;
        }
      }
      const saved = editing === 'new' ? await api.createInstance(payload) : await api.updateInstance(payload);
      if (payload.token) storeToken(payload.token, saved.id);
      qc.invalidateQueries({ queryKey: ['instances'] });
      setEditing(null);
    } catch (e) {
      // 服务端 URL 红线 400 纯文本直接展示
      setErr(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (inst: InstanceConfig): Promise<void> => {
    setErr('');
    setBusy(true);
    try {
      await api.deleteInstance(inst.id); // 服务端删除先 stop 隧道
      qc.invalidateQueries({ queryKey: ['instances'] });
    } catch (e) {
      setErr(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };

  const toggleTunnel = async (inst: InstanceConfig): Promise<void> => {
    setErr('');
    setBusy(true);
    setTunnelError((m) => ({ ...m, [inst.id]: '' }));
    try {
      if (tunnelStatus[inst.id]?.running) {
        await api.tunnelStop(inst.id);
      } else {
        await api.tunnelStart(inst.id);
      }
      const s = await api.tunnelStatus(inst.id);
      setTunnelStatus((m) => ({ ...m, [inst.id]: { running: s.running, state: s.state, retries: s.retries } }));
      qc.invalidateQueries({ queryKey: ['instances'] });
    } catch (e) {
      const msg = String(e instanceof Error ? e.message : e);
      setTunnelError((m) => ({ ...m, [inst.id]: msg }));
      setErr(msg);
    } finally {
      setBusy(false);
    }
  };

  const mode = form.mode;
  const tunnelSt = (id: string) => tunnelStatus[id];

  return (
    <section className="set-group" aria-label={t('inst.aria')}>
      <p className="inst-sub">{t('inst.subtitle')}</p>
      {err ? <p className="dialog-error">{err}</p> : null}

      {editing == null ? (
        <>
          <div className="inst-list">
            {instances.length === 0 ? (
              <p className="inst-empty">{t('inst.empty')}</p>
            ) : (
              instances.map((inst) => {
                const st = tunnelSt(inst.id);
                const flow = installs[inst.id];
                const manualPlan = manual[inst.id];
                return (
                  <div className="inst-block" key={inst.id}>
                    <div className="inst-row">
                      <div className="inst-row-info">
                        <span className="inst-row-name">
                          {inst.name}
                          <span className="inst-row-mode">{inst.mode === 'direct' ? 'direct' : 'ssh-tunnel'}</span>
                        </span>
                        <span className="inst-row-url">
                          {inst.mode === 'direct' ? inst.url : inst.localPort ? `127.0.0.1:${inst.localPort}` : t('inst.tunnelNotStarted')}
                        </span>
                        {inst.mode === 'ssh-tunnel' && st ? (
                          <span className={`inst-tunnel-state inst-tunnel-state--${st.state}`}>
                            {st.state}
                            {t('inst.reconnect', { n: st.retries })}
                          </span>
                        ) : null}
                        {tunnelError[inst.id] ? <span className="inst-tunnel-err">{tunnelError[inst.id]}</span> : null}
                      </div>
                      <div className="inst-row-actions">
                        {inst.mode === 'ssh-tunnel' ? (
                          <button type="button" className="inst-btn" onClick={() => void toggleTunnel(inst)} disabled={busy}>
                            {st?.running ? t('inst.stopTunnel') : t('inst.startTunnel')}
                          </button>
                        ) : null}
                        {inst.mode === 'ssh-tunnel' && flow?.phase !== 'success' ? (
                          <button type="button" className="inst-btn" onClick={() => void runProbe(inst)} disabled={busy}>
                            {t('inst.checkRemote')}
                          </button>
                        ) : null}
                        <button type="button" className="inst-btn" onClick={() => void testConnection(inst)} disabled={busy}>
                          {t('inst.test')}
                        </button>
                        <button type="button" className="inst-btn" onClick={() => startEdit(inst)}>
                          {t('inst.edit')}
                        </button>
                        <button
                          type="button"
                          className="inst-btn inst-btn--danger"
                          onClick={() => {
                            if (confirm(t('inst.deleteConfirm', { name: inst.name }))) void remove(inst);
                          }}
                        >
                          {t('inst.delete')}
                        </button>
                      </div>
                    </div>
                    {/* D4：远程安装流程块（确认面板/进度/日志）——行内展开，移动端不溢出 */}
                    {flow ? <InstallFlowView flow={flow} onConfirm={() => void confirmInstall(inst)} onRetry={() => void openPlan(inst)} onCancel={() => setFlow(inst.id, undefined)} busy={busy} /> : null}
                    {/* D5：direct 未装 → 手动命令清单 + 一键复制（不提供自动执行） */}
                    {inst.mode === 'direct' && manualPlan ? (
                      <div className="inst-manual">
                        <p className="inst-remote-hint">{t('inst.manualHint')}</p>
                        {manualPlan.steps.map((s) => (
                          <div key={s.display}>
                            <p className="inst-step-desc">{s.desc}</p>
                            <pre className="inst-cmd">{s.display}</pre>
                          </div>
                        ))}
                        <button type="button" className="inst-btn" onClick={() => void copyManual(inst)}>
                          {copied[inst.id] ? t('inst.copied') : t('inst.copyList')}
                        </button>
                      </div>
                    ) : null}
                  </div>
                );
              })
            )}
          </div>
          <div className="inst-actions">
            <button type="button" className="btn-primary" onClick={startNew}>
              {t('inst.new')}
            </button>
          </div>
        </>
      ) : (
        <form
          className="inst-form"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <div className="inst-field">
            <label htmlFor="inst-name">{t('inst.field.name')}</label>
            <input id="inst-name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder={t('inst.field.namePh')} required />
          </div>
          <div className="inst-field">
            <label htmlFor="inst-mode">{t('inst.field.mode')}</label>
            <select
              id="inst-mode"
              value={mode}
              onChange={(e) => {
                const m = e.target.value as InstanceMode;
                setForm({ ...form, mode: m, url: m === 'direct' ? form.url : null, token: form.token, ssh: m === 'ssh-tunnel' ? emptySsh() : null, remotePort: null });
              }}
            >
              <option value="direct">{t('inst.mode.direct')}</option>
              <option value="ssh-tunnel">{t('inst.mode.tunnel')}</option>
            </select>
          </div>

          {mode === 'direct' ? (
            <>
              <div className="inst-field">
                <label htmlFor="inst-url">URL</label>
                <input id="inst-url" value={form.url ?? ''} onChange={(e) => setForm({ ...form, url: e.target.value })} placeholder="https://hub.example.com" required />
                <span className="inst-hint">{t('inst.urlHint')}</span>
              </div>
              <div className="inst-field">
                <label htmlFor="inst-token">Token</label>
                <input
                  id="inst-token"
                  value={form.token ?? ''}
                  onChange={(e) => setForm({ ...form, token: e.target.value })}
                  type="password"
                  placeholder={hasToken(editing) ? t('inst.tokenSaved') : t('inst.tokenPh')}
                />
              </div>
            </>
          ) : (
            <>
              <div className="inst-field">
                <label htmlFor="ssh-host">{t('inst.field.sshHost')}</label>
                <input id="ssh-host" value={sshForm.host} onChange={(e) => setSshForm({ ...sshForm, host: e.target.value })} placeholder="example.com" required />
              </div>
              <div className="inst-field">
                <label htmlFor="ssh-port">{t('inst.field.sshPort')}</label>
                <input id="ssh-port" type="number" value={sshForm.port} onChange={(e) => setSshForm({ ...sshForm, port: Number(e.target.value) || 22 })} />
              </div>
              <div className="inst-field">
                <label htmlFor="ssh-user">{t('inst.field.sshUser')}</label>
                <input id="ssh-user" value={sshForm.user} onChange={(e) => setSshForm({ ...sshForm, user: e.target.value })} placeholder="alice" required />
              </div>
              <div className="inst-field">
                <label htmlFor="ssh-auth">{t('inst.field.sshAuth')}</label>
                <select
                  id="ssh-auth"
                  value={sshForm.auth}
                  onChange={(e) => setSshForm({ ...sshForm, auth: e.target.value as SshAuth, keyPath: null, password: null })}
                >
                  {AUTH_KEYS.map((k) => (
                    <option key={k} value={k}>
                      {t(`inst.auth.${k}`)}
                    </option>
                  ))}
                </select>
              </div>
              {sshForm.auth === 'key_path' ? (
                <div className="inst-field">
                  <label htmlFor="ssh-keypath">{t('inst.field.keyPath')}</label>
                  <input id="ssh-keypath" value={sshForm.keyPath ?? ''} onChange={(e) => setSshForm({ ...sshForm, keyPath: e.target.value })} placeholder="~/.ssh/id_ed25519" required />
                </div>
              ) : null}
              {sshForm.auth === 'password' ? (
                <div className="inst-field">
                  <label htmlFor="ssh-password">{t('inst.field.password')}</label>
                  <input id="ssh-password" type="password" value={sshForm.password ?? ''} onChange={(e) => setSshForm({ ...sshForm, password: e.target.value })} required />
                  <span className="inst-hint">{t('inst.sshpassHint')}</span>
                </div>
              ) : null}
              <div className="inst-field">
                <label htmlFor="ssh-remoteport">{t('inst.field.remotePort')}</label>
                <input id="ssh-remoteport" type="number" value={remotePort} onChange={(e) => setRemotePort(e.target.value)} placeholder="7800" required />
              </div>
            </>
          )}

          <div className="inst-form-actions">
            <button type="submit" className="btn-primary" disabled={busy}>
              {t('inst.save')}
            </button>
            <button type="button" className="inst-btn" onClick={cancel}>
              {t('inst.cancel')}
            </button>
          </div>
        </form>
      )}
    </section>
  );
}

interface FlowViewProps {
  flow: InstallFlow;
  onConfirm: () => void;
  onRetry: () => void;
  onCancel: () => void;
  busy: boolean;
}

/// D4 流程块视图：探测结论 / 确认面板（命令清单） / 执行进度 / 结果与日志。
/// 确认按钮是唯一发起 install 请求的入口（未确认零请求，服务端同样校验）。
function InstallFlowView({ flow, onConfirm, onRetry, onCancel, busy }: FlowViewProps): ReactElement {
  const t = useI18n();
  const probe = flow.probe;
  if (flow.phase === 'success') {
    return (
      <div className="inst-confirm">
        <p className="inst-install-ok">
          {probe?.reason === 'installed' ? t('inst.remoteOk') : (flow.note ?? t('inst.done'))}
        </p>
      </div>
    );
  }
  if (flow.phase === 'not-installed') {
    return (
      <div className="inst-confirm">
        <p className="inst-remote-hint">{t('inst.notInstalled')}</p>
        <button type="button" className="btn-primary" onClick={onRetry} disabled={busy}>
          {t('inst.installBtn')}
        </button>
      </div>
    );
  }
  if (flow.phase === 'probe-fail') {
    return (
      <div className="inst-confirm">
        <p className="inst-tunnel-err">{probe?.detail ?? t('inst.probeFailed')}</p>
      </div>
    );
  }
  if (flow.phase === 'running') {
    return (
      <div className="inst-confirm">
        <p className="inst-install-running">{flow.note ?? t('inst.processing')}</p>
      </div>
    );
  }
  if (flow.phase === 'confirm' && flow.plan) {
    return (
      <div className="inst-confirm" aria-label={t('inst.confirmAria')}>
        <p className="inst-confirm-title">{t('inst.confirmTitle')}</p>
        {flow.plan.steps.map((s) => (
          <div key={s.display}>
            <p className="inst-step-desc">{s.desc}</p>
            <pre className="inst-cmd">{s.display}</pre>
          </div>
        ))}
        <div className="inst-form-actions">
          <button type="button" className="btn-primary" onClick={onConfirm} disabled={busy}>
            {t('inst.confirmInstall')}
          </button>
          <button type="button" className="inst-btn" onClick={onCancel}>
            {t('inst.cancel')}
          </button>
        </div>
      </div>
    );
  }
  // failed：可读错误 + 执行日志尾部 + 原清单重试
  return (
    <div className="inst-confirm">
      <p className="inst-tunnel-err">{flow.result?.error ?? flow.note ?? t('inst.failed')}</p>
      {flow.result?.logs?.length ? <pre className="inst-logs">{flow.result.logs.join('\n')}</pre> : null}
      <div className="inst-form-actions">
        <button type="button" className="inst-btn" onClick={onRetry} disabled={busy}>
          {t('inst.retry')}
        </button>
        <button type="button" className="inst-btn" onClick={onCancel}>
          {t('inst.close')}
        </button>
      </div>
    </div>
  );
}
