import { useEffect, useState, type ReactElement } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import { useI18n } from '../i18n';
import './StartDialog.css';

type Driver = 'claude' | 'acp';

export default function StartDialog({
  onClose,
}: {
  onClose: () => void;
}): ReactElement {
  const qc = useQueryClient();
  const t = useI18n();
  // driver 选择（acp-omp 批2 任务7）：claude 走 bg 派发，acp 走 ACP 握手建会话
  const [driver, setDriver] = useState<Driver>('claude');
  const [projects, setProjects] = useState<string[]>([]);
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
    api.listProjects().then((ps) => {
      setProjects(ps);
      if (ps.length > 0) setCwd(ps[0]);
    });
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
        onClose();
      } catch (e) {
        setErr(String(e));
      } finally {
        setBusy(false);
      }
      return;
    }
    if (!cwd.trim() || !prompt.trim()) {
      setErr('cwd and prompt are required');
      return;
    }
    setBusy(true);
    setErr('');
    try {
      await api.startAgent({
        cwd: cwd.trim(),
        prompt: prompt.trim(),
        ...(model.trim() ? { model: model.trim() } : {}),
        ...(effort.trim() ? { effort: effort.trim() } : {}),
        ...(name.trim() ? { name: name.trim() } : {}),
      });
      // 派发默认项：记忆上次使用的 model/effort（P3）
      localStorage.setItem('hub_last_model', model.trim());
      localStorage.setItem('hub_last_effort', effort.trim());
      qc.invalidateQueries({ queryKey: ['agents'] });
      onClose();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 上次派发留有 model/effort 时展开高级区（记住上次值逻辑保留）
  const advancedOpen = model.trim() !== '' || effort.trim() !== '';

  return (
    <div className="dialog-backdrop" role="dialog" aria-modal="true" aria-label="Start agent">
      <div className="dialog-card start-card">
        <h2 className="dialog-title">{t('start.title')}</h2>
        <div className="driver-toggle" role="tablist" aria-label="driver">
          <button
            type="button"
            role="tab"
            aria-selected={driver === 'claude'}
            onClick={() => setDriver('claude')}
          >
            Claude Code
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={driver === 'acp'}
            onClick={() => setDriver('acp')}
          >
            ACP（omp）
          </button>
        </div>
        {driver === 'acp' ? (
          <p className="dialog-subtitle">{t('start.acpSubtitle')}</p>
        ) : (
          <p className="dialog-subtitle">{t('start.subtitle')}</p>
        )}
        <label>
          {t('start.cwd')}
          <select value={cwd} onChange={(e) => setCwd(e.target.value)}>
            <option value="">-- choose or type below --</option>
            {projects.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </label>
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
                CWD fallback
                <input
                  value={cwd}
                  onChange={(e) => setCwd(e.target.value)}
                  placeholder="/absolute/path"
                />
              </label>
              <label>
                Model (optional, blank = default)
                <input value={model} onChange={(e) => setModel(e.target.value)} placeholder="sonnet" />
              </label>
              <label>
                Effort (optional)
                <input value={effort} onChange={(e) => setEffort(e.target.value)} placeholder="high" />
              </label>
              <label>
                Name (optional)
                <input value={name} onChange={(e) => setName(e.target.value)} />
              </label>
            </details>
          </>
        )}
        {err ? <p className="dialog-error">{err}</p> : null}
        <div className="dialog-actions">
          <button type="button" className="btn-ghost" onClick={onClose} disabled={busy}>
            {t('dialog.cancel')}
          </button>
          <button type="button" className="btn-primary" onClick={() => void submit()} disabled={busy}>
            {busy && driver === 'acp' ? t('start.creating') : t('start.launch')}
          </button>
        </div>
      </div>
    </div>
  );
}
