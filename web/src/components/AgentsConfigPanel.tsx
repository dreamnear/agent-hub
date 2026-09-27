import { useEffect, useState, type ReactElement } from 'react';
import { api } from '../api';
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
          <h2>Agents 配置</h2>
          <button type="button" className="btn-ghost" onClick={onClose}>
            关闭
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
            {docs.length === 0 ? <li className="agentscfg-empty">无 agents 配置</li> : null}
          </ul>
          {editing ? (
            <div className="agentscfg-editor">
              <div className="agentscfg-editing-name">
                编辑：{editing.name} {saved ? <span className="agentscfg-saved">已保存</span> : null}
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
                  保存写回
                </button>
              </div>
            </div>
          ) : (
            <p className="agentscfg-empty">左侧选择一个配置查看/编辑</p>
          )}
        </div>
      </div>
    </div>
  );
}
