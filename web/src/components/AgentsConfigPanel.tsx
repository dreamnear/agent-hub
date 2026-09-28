import { useEffect, useState, type ReactElement } from 'react';
import { api } from '../api';
import { useI18n } from '../i18n';
import './AgentsConfigPanel.css';

interface AgentDocSummary {
  name: string;
  description: string | null;
  model: string | null;
  tools: string | null;
}

interface AgentDoc extends AgentDocSummary {
  content: string;
}

/// agents 配置管理器（P3）：~/.claude/agents/*.md 列表 / 查看 / 编辑写回。
export default function AgentsConfigPanel({ onClose }: { onClose: () => void }): ReactElement {
  const t = useI18n();
  const [docs, setDocs] = useState<AgentDocSummary[]>([]);
  const [editing, setEditing] = useState<AgentDoc | null>(null);
  const [draft, setDraft] = useState('');
  const [err, setErr] = useState('');
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    api
      .listAgentConfigs()
      .then(setDocs)
      .catch((e) => setErr(String(e)));
  }, []);

  const open = async (name: string): Promise<void> => {
    setErr('');
    try {
      const doc = await api.getAgentConfig(name);
      setEditing(doc);
      setDraft(doc.content);
      setSaved(false);
    } catch (e) {
      setErr(String(e));
    }
  };

  const save = async (): Promise<void> => {
    if (!editing) return;
    setErr('');
    setSaved(false);
    try {
      await api.putAgentConfig(editing.name, draft);
      setSaved(true);
      const docs = await api.listAgentConfigs();
      setDocs(docs);
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div className="agentscfg-backdrop" role="dialog" aria-modal="true" aria-label="Agents config"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="agentscfg-card">
        <div className="agentscfg-head">
          <h2>{t('agents.title')}</h2>
          <button type="button" className="btn-ghost" onClick={onClose}>
            {t('agents.close')}
          </button>
        </div>
        {err ? <p className="dialog-error">{err}</p> : null}
        <div className="agentscfg-body">
          <ul className="agentscfg-list" aria-label="agent docs">
            {docs.map((d) => (
              <li key={d.name}>
                <button
                  type="button"
                  className={`agentscfg-opt ${editing?.name === d.name ? 'agentscfg-opt--sel' : ''}`}
                  onClick={() => void open(d.name)}
                >
                  <span className="agentscfg-name">{d.name}</span>
                  <span className="agentscfg-model">{d.model ?? ''}</span>
                  <span className="agentscfg-desc">{d.description ?? ''}</span>
                </button>
              </li>
            ))}
            {docs.length === 0 ? <li className="agentscfg-empty">{t('agents.empty')}</li> : null}
          </ul>
          {editing ? (
            <div className="agentscfg-editor">
              <div className="agentscfg-editing-name">
                {t('agents.editing', { name: editing.name })} {saved ? <span className="agentscfg-saved">{t('agents.saved')}</span> : null}
              </div>
              <textarea
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                rows={16}
                spellCheck={false}
                aria-label="agent config content"
              />
              <div className="agentscfg-actions">
                <button type="button" className="btn-primary" onClick={() => void save()}>
                  {t('agents.writeBack')}
                </button>
              </div>
            </div>
          ) : (
            <p className="agentscfg-empty">{t('agents.pickHint')}</p>
          )}
        </div>
      </div>
    </div>
  );
}
