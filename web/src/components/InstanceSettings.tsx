import { useEffect, useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, storeToken } from '../api';
import { fetchInstances } from '../instances';
import type { InstanceConfig, InstanceMode, SshAuth, SshConfig } from '../types';
import './InstanceSettings.css';

interface Props {
  onClose: () => void;
}

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

const AUTH_LABEL: Record<SshAuth, string> = {
  key_path: '证书路径',
  authsock: 'SSH Agent (authsock)',
  password: '密码（需 sshpass）',
};

/// 实例管理设置面板（agent-hub-multi-instance 任务9）：
/// 实例 CRUD（direct + ssh-tunnel 两型）、连接测试、隧道状态展示。
/// URL 红线的拒绝文案透出（服务端 400 纯文本直接展示）；token 显示用占位符防误读，
/// 改 token 时前端只更新实例配置里的 token 字段（不强制重输）。
export default function InstanceSettings({ onClose }: Props): ReactElement {
  const qc = useQueryClient();
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
      if (!res.ok) throw new Error(`连接测试失败：HTTP ${res.status}`);
      setErr(''); // 成功不清除已有错误；用临时反馈
      alert(`连接成功：${base}`);
    } catch (e) {
      setErr(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
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
        setErr('direct 模式需填写 URL');
        return;
      }
      if (payload.mode === 'ssh-tunnel') {
        if (!payload.ssh?.host.trim() || !payload.ssh.user.trim()) {
          setErr('ssh-tunnel 需填写 SSH 主机与用户名');
          return;
        }
        if (payload.remotePort == null) {
          setErr('ssh-tunnel 需填写远程目标端口');
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
    <div className="inst-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="inst-card" role="dialog" aria-label="实例管理">
        <div className="inst-head">
          <span className="inst-title">实例管理</span>
          <button type="button" className="inst-close-btn" aria-label="关闭" onClick={onClose}>
            ×
          </button>
        </div>
        <p className="inst-sub">接入本机 + 远程 agent-hub 实例；direct 远程须 https，ssh-tunnel 走 SSH 加密</p>
        {err ? <p className="dialog-error">{err}</p> : null}

        {editing == null ? (
          <>
            <div className="inst-list">
              {instances.length === 0 ? (
                <p className="inst-empty">暂无实例。点击「新建实例」接入一个。</p>
              ) : (
                instances.map((inst) => {
                  const st = tunnelSt(inst.id);
                  return (
                    <div className="inst-row" key={inst.id}>
                      <div className="inst-row-info">
                        <span className="inst-row-name">
                          {inst.name}
                          <span className="inst-row-mode">{inst.mode === 'direct' ? 'direct' : 'ssh-tunnel'}</span>
                        </span>
                        <span className="inst-row-url">
                          {inst.mode === 'direct' ? inst.url : inst.localPort ? `127.0.0.1:${inst.localPort}` : '隧道未启动'}
                        </span>
                        {inst.mode === 'ssh-tunnel' && st ? (
                          <span className={`inst-tunnel-state inst-tunnel-state--${st.state}`}>
                            {st.state}（重连 {st.retries}）
                          </span>
                        ) : null}
                        {tunnelError[inst.id] ? <span className="inst-tunnel-err">{tunnelError[inst.id]}</span> : null}
                      </div>
                      <div className="inst-row-actions">
                        {inst.mode === 'ssh-tunnel' ? (
                          <button type="button" className="inst-btn" onClick={() => void toggleTunnel(inst)} disabled={busy}>
                            {st?.running ? '停止隧道' : '启动隧道'}
                          </button>
                        ) : null}
                        <button type="button" className="inst-btn" onClick={() => void testConnection(inst)} disabled={busy}>
                          测试
                        </button>
                        <button type="button" className="inst-btn" onClick={() => startEdit(inst)}>
                          编辑
                        </button>
                        <button
                          type="button"
                          className="inst-btn inst-btn--danger"
                          onClick={() => {
                            if (confirm(`删除实例「${inst.name}」？隧道将一并停止。`)) void remove(inst);
                          }}
                        >
                          删除
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>
            <div className="inst-actions">
              <button type="button" className="btn-primary" onClick={startNew}>
                ＋ 新建实例
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
              <label htmlFor="inst-name">名称</label>
              <input id="inst-name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="如 远程开发机" required />
            </div>
            <div className="inst-field">
              <label htmlFor="inst-mode">连接方式</label>
              <select
                id="inst-mode"
                value={mode}
                onChange={(e) => {
                  const m = e.target.value as InstanceMode;
                  setForm({ ...form, mode: m, url: m === 'direct' ? form.url : null, token: form.token, ssh: m === 'ssh-tunnel' ? emptySsh() : null, remotePort: null });
                }}
              >
                <option value="direct">direct（直连 https）</option>
                <option value="ssh-tunnel">ssh-tunnel（SSH 隧道）</option>
              </select>
            </div>

            {mode === 'direct' ? (
              <>
                <div className="inst-field">
                  <label htmlFor="inst-url">URL</label>
                  <input id="inst-url" value={form.url ?? ''} onChange={(e) => setForm({ ...form, url: e.target.value })} placeholder="https://hub.example.com" required />
                  <span className="inst-hint">远程 direct URL 必须是 https（本机 localhost 可用 http）</span>
                </div>
                <div className="inst-field">
                  <label htmlFor="inst-token">Token</label>
                  <input
                    id="inst-token"
                    value={form.token ?? ''}
                    onChange={(e) => setForm({ ...form, token: e.target.value })}
                    type="password"
                    placeholder={hasToken(editing) ? '已保存（留空不修改）' : '远程实例 token'}
                  />
                </div>
              </>
            ) : (
              <>
                <div className="inst-field">
                  <label htmlFor="ssh-host">SSH 主机</label>
                  <input id="ssh-host" value={sshForm.host} onChange={(e) => setSshForm({ ...sshForm, host: e.target.value })} placeholder="example.com" required />
                </div>
                <div className="inst-field">
                  <label htmlFor="ssh-port">SSH 端口</label>
                  <input id="ssh-port" type="number" value={sshForm.port} onChange={(e) => setSshForm({ ...sshForm, port: Number(e.target.value) || 22 })} />
                </div>
                <div className="inst-field">
                  <label htmlFor="ssh-user">用户名</label>
                  <input id="ssh-user" value={sshForm.user} onChange={(e) => setSshForm({ ...sshForm, user: e.target.value })} placeholder="user" required />
                </div>
                <div className="inst-field">
                  <label htmlFor="ssh-auth">认证方式</label>
                  <select
                    id="ssh-auth"
                    value={sshForm.auth}
                    onChange={(e) => setSshForm({ ...sshForm, auth: e.target.value as SshAuth, keyPath: null, password: null })}
                  >
                    {(Object.keys(AUTH_LABEL) as SshAuth[]).map((k) => (
                      <option key={k} value={k}>
                        {AUTH_LABEL[k]}
                      </option>
                    ))}
                  </select>
                </div>
                {sshForm.auth === 'key_path' ? (
                  <div className="inst-field">
                    <label htmlFor="ssh-keypath">证书路径</label>
                    <input id="ssh-keypath" value={sshForm.keyPath ?? ''} onChange={(e) => setSshForm({ ...sshForm, keyPath: e.target.value })} placeholder="~/.ssh/id_ed25519" required />
                  </div>
                ) : null}
                {sshForm.auth === 'password' ? (
                  <div className="inst-field">
                    <label htmlFor="ssh-password">密码</label>
                    <input id="ssh-password" type="password" value={sshForm.password ?? ''} onChange={(e) => setSshForm({ ...sshForm, password: e.target.value })} required />
                    <span className="inst-hint">密码认证需本机安装 sshpass（macOS 未预装，建议用证书/authsock）</span>
                  </div>
                ) : null}
                <div className="inst-field">
                  <label htmlFor="ssh-remoteport">远程目标端口</label>
                  <input id="ssh-remoteport" type="number" value={remotePort} onChange={(e) => setRemotePort(e.target.value)} placeholder="7800" required />
                </div>
              </>
            )}

            <div className="inst-form-actions">
              <button type="submit" className="btn-primary" disabled={busy}>
                保存
              </button>
              <button type="button" className="inst-btn" onClick={cancel}>
                取消
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}