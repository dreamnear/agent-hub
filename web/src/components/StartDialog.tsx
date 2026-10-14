import { useEffect, useState, type ReactElement } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import { useI18n } from '../i18n';
import CwdField from './CwdField';
import './StartDialog.css';

/// driver 选项事实源 = GET /api/drivers（注册表注册序，去硬编码）。
/// 表单形态按 driver 分派：acp 走 agent 握手，其余统一 cwd+prompt 创建
///（claude/dsh/opencode 的 start 均实现 session 创建 + 首条 prompt）。
const DRIVER_LABELS: Record<string, string> = {
  claude: 'Claude Code',
  acp: 'ACP（omp）',
  dsh: 'DSH',
  opencode: 'OpenCode',
};

export default function StartDialog({
  onClose,
  onCreated,
  embedded = false,
}: {
  /** 对话框形态关闭回调；embedded（设置页分区）下无遮罩可关，可缺省 */
  onClose?: () => void;
  /** 创建成功回带会话 id：父级立即置为选中（不等轮询，防空 id 发送窗口） */
  onCreated?: (driver: string, id: string) => void;
  /** embedded（agent-hub-settings-consolidation S2）：设置页分区内嵌形态——去遮罩，表单行为不变（S3） */
  embedded?: boolean;
}): ReactElement {
  const qc = useQueryClient();
  const t = useI18n();
  const [driver, setDriver] = useState('claude');
  const [drivers, setDrivers] = useState<string[]>([]);
  const [cwd, setCwd] = useState('');
  const [prompt, setPrompt] = useState('');
  const [model, setModel] = useState(() => localStorage.getItem('hub_last_model') ?? '');
  const [effort, setEffort] = useState(() => localStorage.getItem('hub_last_effort') ?? '');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');
  // ACP agent 清单（config [acp] 声明），默认选首个
  const [acpAgents, setAcpAgents] = useState<string[]>([]);
  const [acpAgent, setAcpAgent] = useState('');

  useEffect(() => {
    api.listDrivers().then(setDrivers).catch(() => setDrivers(['claude']));
  }, []);

  useEffect(() => {
    if (driver !== 'acp' || acpAgents.length > 0) return;
    api
      .listAcpAgents()
      .then((r) => {
        setAcpAgents(r.agents.map((a) => a.name));
        setAcpAgent((cur) => cur || r.agents[0]?.name || '');
      })
      .catch((e: unknown) => setErr(String(e)));
  }, [driver, acpAgents.length]);

  const submit = async (): Promise<void> => {
    if (driver === 'acp') {
      if (!cwd.trim() || !acpAgent) {
        setErr('cwd and agent are required');
        return;
      }
      setBusy(true);
      setErr('');
      try {
        await api.createAcpSession({
          agent: acpAgent,
          cwd: cwd.trim(),
          ...(model.trim() ? { model: model.trim() } : {}),
        });
        localStorage.setItem('hub_last_model', model.trim());
        qc.invalidateQueries({ queryKey: ['agents'] });
        onClose?.();
      } catch (e) {
        setErr(String(e));
      } finally {
        setBusy(false);
      }
      return;
    }
    // dsh/opencode/claude 同构：创建会话 + 首条 prompt（prompt 必填——
    // attach-service 驱动空会话不在侧栏显示，首发即活跃 turn）
    if (!cwd.trim() || !prompt.trim()) {
      setErr('cwd and prompt are required');
      return;
    }
    setBusy(true);
    setErr('');
    try {
      const created = await api.startAgent(
        {
          cwd: cwd.trim(),
          prompt: prompt.trim(),
          ...(model.trim() ? { model: model.trim() } : {}),
          ...(effort.trim() ? { effort: effort.trim() } : {}),
          ...(name.trim() ? { name: name.trim() } : {}),
        },
        driver,
      );
      onCreated?.(driver, created.id);
      // 派发默认项：记忆上次使用的 model/effort（P3）
      localStorage.setItem('hub_last_model', model.trim());
      localStorage.setItem('hub_last_effort', effort.trim());
      qc.invalidateQueries({ queryKey: ['agents'] });
      onClose?.();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 上次派发留有 model/effort 时展开高级区（记住上次值逻辑保留）
  const advancedOpen = model.trim() !== '' || effort.trim() !== '';
  const toggle = drivers.length > 0 ? drivers : ['claude'];

  const card = (
      <div className={`dialog-card start-card${embedded ? ' start-card--embedded' : ''}`}>
        <h2 className="dialog-title">{t('start.title')}</h2>
        <div className="driver-toggle" role="tablist" aria-label="driver">
          {toggle.map((d) => (
            <button
              key={d}
              type="button"
              role="tab"
              aria-selected={driver === d}
              onClick={() => setDriver(d)}
            >
              {DRIVER_LABELS[d] ?? d}
            </button>
          ))}
        </div>
        {driver === 'acp' ? (
          <p className="dialog-subtitle">{t('start.acpSubtitle')}</p>
        ) : (
          <p className="dialog-subtitle">{t('start.subtitle')}</p>
        )}
        <CwdField label={t('start.cwd')} inputId="start-cwd" value={cwd} onChange={setCwd} />
        {driver === 'acp' ? (
          <>
            <label>
              ACP Agent
              <select value={acpAgent} onChange={(e) => setAcpAgent(e.target.value)}>
                {acpAgents.map((n) => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Model (optional, blank = agent default)
              <input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder={t('start.modelPh')}
              />
            </label>
          </>
        ) : (
          <>
            <label>
              {t('start.prompt')}
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                rows={3}
                placeholder={t('start.promptPh')}
              />
            </label>
            <details className="start-advanced" open={advancedOpen}>
              <summary>{t('start.advanced')}</summary>
              <label>
                Model (optional, blank = default)
                <input value={model} onChange={(e) => setModel(e.target.value)} placeholder="sonnet" />
              </label>
              {driver === 'claude' ? (
                <label>
                  Effort (optional)
                  <input value={effort} onChange={(e) => setEffort(e.target.value)} placeholder="high" />
                </label>
              ) : null}
              <label>
                Name (optional)
                <input value={name} onChange={(e) => setName(e.target.value)} />
              </label>
            </details>
          </>
        )}
        {err ? <p className="dialog-error">{err}</p> : null}
        <div className="dialog-actions">
          {onClose != null ? (
            <button type="button" className="btn-ghost" onClick={onClose} disabled={busy}>
              {t('dialog.cancel')}
            </button>
          ) : null}
          <button type="button" className="btn-primary" onClick={() => void submit()} disabled={busy}>
            {busy && driver === 'acp' ? t('start.creating') : t('start.launch')}
          </button>
        </div>
      </div>
  );

  if (embedded) return card;
  return (
    <div className="dialog-backdrop" role="dialog" aria-modal="true" aria-label="Start agent">
      {card}
    </div>
  );
}
