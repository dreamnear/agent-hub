import { useEffect, useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, makeApi } from '../../api';
import type { InstanceConfig } from '../../types';
import { useI18n } from '../../i18n';
import CwdField from '../CwdField';
import type { TaskSpecWithAction, TaskRun } from '../../types';
import './TasksSection.css';

/// 表单草稿（动作类型切换决定 spawn/inject 域显隐）
interface Draft {
  id: string; // '' = 新建
  name: string;
  cron: string;
  enabled: boolean;
  action: 'spawn' | 'inject';
  driver: string;
  cwd: string;
  name_template: string;
  prompt_template: string;
  session_id: string;
  text: string;
}

function emptyDraft(): Draft {
  return {
    id: '',
    name: '',
    cron: '* * * * *',
    enabled: true,
    action: 'spawn',
    driver: 'claude',
    cwd: '',
    name_template: '',
    prompt_template: '',
    session_id: '',
    text: '',
  };
}

function draftOf(t: TaskSpecWithAction): Draft {
  const base = emptyDraft();
  base.id = t.id;
  base.name = t.name;
  base.cron = t.cron;
  base.enabled = t.enabled;
  if (t.action === 'spawn') {
    return {
      ...base,
      action: 'spawn',
      driver: t.driver,
      cwd: t.cwd,
      name_template: t.name_template ?? '',
      prompt_template: t.prompt_template,
    };
  }
  return { ...base, action: 'inject', driver: t.driver, session_id: t.session_id, text: t.text };
}

/// 草稿 → 请求体（必填校验；空串模板归 null）。返回 null = 校验不过（就地提示）。
function draftToSpec(d: Draft): TaskSpecWithAction | null {
  const name = d.name.trim();
  const cron = d.cron.trim();
  if (name === '' || cron === '') return null;
  if (d.action === 'spawn') {
    const driver = d.driver.trim();
    const cwd = d.cwd.trim();
    const prompt = d.prompt_template.trim();
    if (driver === '' || cwd === '' || prompt === '') return null;
    const nt = d.name_template.trim();
    return {
      id: d.id,
      name,
      cron,
      enabled: d.enabled,
      action: 'spawn',
      driver,
      cwd,
      name_template: nt === '' ? null : nt,
      prompt_template: prompt,
    };
  }
  const driver = d.driver.trim();
  const sessionId = d.session_id.trim();
  const text = d.text.trim();
  if (driver === '' || sessionId === '' || text === '') return null;
  return { id: d.id, name, cron, enabled: d.enabled, action: 'inject', driver, session_id: sessionId, text };
}

interface Props {
  /// Run 记录里的 sessionId 跳转该会话（App：关设置页 + 选中）
  onOpenSession?: (agentId: string) => void;
}

/// 设置中心「定时任务」段（agent-hub-daemon-capabilities C3）：
/// 列表（名称/cron/动作摘要/启停/最近 Run 状态点）+ 新建/编辑表单（spawn/inject 域切换）
/// + 删除二次确认 + Run 历史展开（状态/时间/时长/sessionId 可点跳转/error）。
export default function TasksSection({ onOpenSession }: Props): ReactElement {
  // 实例路由（gateway-node-p3 M4）：node-link 实例的定时任务经同源代理面
  // /p/api/tasks 可视可控——节点自带 TaskManager，中心 hub 只做路由不改语义。
  const [instanceId, setInstanceId] = useState<string>('');
  const instances = useQuery({
    queryKey: ['instances'],
    queryFn: api.listInstances,
    refetchInterval: 30_000,
  });
  const nodeLinkInstances = (instances.data ?? []).filter(
    (i: InstanceConfig) => i.mode === 'node-link',
  );
  const scoped =
    instanceId === '' ? api : makeApi(`/api/instances/${encodeURIComponent(instanceId)}/p`, instanceId);
  const t = useI18n();
  const qc = useQueryClient();
  const tasks = useQuery({ queryKey: ['tasks', instanceId], queryFn: () => scoped.listTasks(), refetchInterval: 30_000 });
  // schedule-lock S2：调度权状态（30s 轮询对齐任务表）
  const sched = useQuery({
    queryKey: ['schedulerStatus'],
    queryFn: () => scoped.schedulerStatus(),
    refetchInterval: 30_000,
  });
  const [form, setForm] = useState<Draft | null>(null);
  const [formError, setFormError] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (formError && form != null) setFormError(false); // 再编辑即清提示
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 草稿变化即清错误提示
  }, [form]);

  const invalidate = (): void => {
    void qc.invalidateQueries({ queryKey: ['tasks', instanceId] });
    void qc.invalidateQueries({ queryKey: ['taskRuns'] });
  };

  const save = async (): Promise<void> => {
    if (form == null) return;
    const spec = draftToSpec(form);
    if (spec == null) {
      setFormError(true);
      return;
    }
    setBusy(true);
    try {
      if (form.id === '') await scoped.createTask(spec);
      else await scoped.updateTask(form.id, spec);
      setForm(null);
      invalidate();
    } finally {
      setBusy(false);
    }
  };

  const toggleEnabled = async (task: TaskSpecWithAction): Promise<void> => {
    await scoped.updateTask(task.id, { ...task, enabled: !task.enabled });
    invalidate();
  };

  const remove = async (id: string): Promise<void> => {
    if (confirmDelete !== id) {
      setConfirmDelete(id); // 二次确认：第一次点变「确认删除」
      return;
    }
    setConfirmDelete(null);
    await scoped.deleteTask(id);
    invalidate();
  };

  const runNow = async (id: string): Promise<void> => {
    setBusy(true);
    try {
      await scoped.runTask(id);
      setExpanded(id);
      invalidate();
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="set-group task-section">
      <div className="set-row">
        <span className="set-label">
          {sched.data != null && (
            <span
              className={`task-sched-badge ${sched.data.primary ? 'task-sched-badge--on' : 'task-sched-badge--off'}`}
              title={t(sched.data.primary ? 'tasks.sched.primaryTitle' : 'tasks.sched.standbyTitle')}
            >
              {t(sched.data.primary ? 'tasks.sched.primary' : 'tasks.sched.standby')}
            </span>
          )}
          {t('tasks.sectionDesc')}
        </span>
        <label className="flex items-center gap-2 text-xs text-text/60">
          {t('tasks.instance')}
          <select
            className="bg-bg border border-line rounded px-2 py-1 text-sm text-text"
            value={instanceId}
            onChange={(e) => setInstanceId(e.target.value)}
            data-testid="tasks-instance"
          >
            <option value="">{t('tasks.localInstance')}</option>
            {nodeLinkInstances.map((i) => (
              <option key={i.id} value={i.id}>
                {i.name}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          className="btn-ghost task-new"
          onClick={() => setForm(emptyDraft())}
          disabled={form != null}
        >
          + {t('tasks.new')}
        </button>
      </div>

      {form != null ? (
        <div className="task-form" aria-label={t('tasks.form')}>
          <label className="task-field">
            <span>{t('tasks.name')} *</span>
            <input
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder={t('tasks.namePh')}
            />
          </label>
          <label className="task-field">
            <span>{t('tasks.cron')} *</span>
            <input
              value={form.cron}
              onChange={(e) => setForm({ ...form, cron: e.target.value })}
              placeholder="0 30 9 * * *"
            />
          </label>
          <label className="task-field task-field--check">
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
            />
            <span>{t('tasks.enabled')}</span>
          </label>
          <label className="task-field">
            <span>{t('tasks.action')}</span>
            <select
              value={form.action}
              onChange={(e) => setForm({ ...form, action: e.target.value as Draft['action'] })}
            >
              <option value="spawn">{t('tasks.action.spawn')}</option>
              <option value="inject">{t('tasks.action.inject')}</option>
            </select>
          </label>
          <label className="task-field">
            <span>{t('tasks.driver')}</span>
            <select
              value={form.driver}
              onChange={(e) => setForm({ ...form, driver: e.target.value })}
            >
              <option value="claude">claude</option>
              <option value="opencode">opencode</option>
              <option value="dsh">dsh</option>
              <option value="acp">acp</option>
            </select>
          </label>

          {form.action === 'spawn' ? (
            <>
              <CwdField
                label={
                  <>
                    {t('tasks.cwd')} *
                  </>
                }
                value={form.cwd}
                onChange={(v) => setForm({ ...form, cwd: v })}
              />
              <label className="task-field">
                <span>{t('tasks.nameTemplate')}</span>
                <input
                  value={form.name_template}
                  onChange={(e) => setForm({ ...form, name_template: e.target.value })}
                  placeholder={`日报 {{date}}`}
                />
              </label>
              <label className="task-field">
                <span>{t('tasks.promptTemplate')} *</span>
                <textarea
                  value={form.prompt_template}
                  onChange={(e) => setForm({ ...form, prompt_template: e.target.value })}
                  rows={3}
                  placeholder={t('tasks.templateHint')}
                />
              </label>
            </>
          ) : (
            <>
              <label className="task-field">
                <span>{t('tasks.sessionId')} *</span>
                <input
                  value={form.session_id}
                  onChange={(e) => setForm({ ...form, session_id: e.target.value })}
                />
              </label>
              <label className="task-field">
                <span>{t('tasks.text')} *</span>
                <textarea
                  value={form.text}
                  onChange={(e) => setForm({ ...form, text: e.target.value })}
                  rows={3}
                  placeholder={t('tasks.templateHint')}
                />
              </label>
            </>
          )}

          {formError ? <p className="dialog-error">{t('tasks.formInvalid')}</p> : null}
          <div className="task-form-actions">
            <button type="button" className="btn-ghost" onClick={() => setForm(null)}>
              {t('tasks.cancel')}
            </button>
            <button type="button" className="btn-primary" onClick={() => void save()} disabled={busy}>
              {t('tasks.save')}
            </button>
          </div>
        </div>
      ) : null}

      {tasks.data != null && tasks.data.length === 0 ? (
        <p className="task-empty">{t('tasks.empty')}</p>
      ) : null}

      <div className="task-list">
        {(tasks.data ?? []).map((task) => {
          const actionLabel =
            task.action === 'spawn' ? t('tasks.action.spawn') : t('tasks.action.inject');
          const summary =
            task.action === 'spawn'
              ? `${task.cwd}`
              : `${t('tasks.sessionId')}: ${task.session_id}`;
          return (
            <div key={task.id} className="task-item">
              <div className="task-item-main">
                <span
                  className={`task-dot task-dot--${task.enabled ? 'enabled' : 'disabled'}`}
                  title={task.enabled ? t('tasks.enabled') : t('tasks.disabled')}
                />
                <span className="task-item-name" title={task.id}>
                  {task.name}
                </span>
                {task.source === 'assistant' ? (
                  <span
                    className="task-item-source"
                    title={t('tasks.sourceAssistant')}
                    data-testid={`task-source-${task.id}`}
                  >
                    {t('tasks.sourceAssistant')}
                  </span>
                ) : null}
                <span className="task-item-cron" title={task.cron}>
                  {task.cron}
                </span>
                <span className="task-item-action">{actionLabel}</span>
                <span className="task-item-summary" title={summary}>
                  {summary}
                </span>
                <span className="task-item-next" title={task.next_run_at ?? undefined}>
                  {task.enabled && task.next_run_at != null
                    ? `${t('tasks.nextRun')} ${task.next_run_at.slice(11, 16)}`
                    : t('tasks.disabled')}
                </span>
              </div>
              <div className="task-item-actions">
                <label className="task-switch" title={t('tasks.enabled')}>
                  <input
                    type="checkbox"
                    checked={task.enabled}
                    onChange={() => void toggleEnabled(task)}
                  />
                  <span>{t('tasks.enabled')}</span>
                </label>
                <button
                  type="button"
                  className="btn-ghost task-btn"
                  onClick={() => setExpanded(expanded === task.id ? null : task.id)}
                >
                  {t('tasks.recentRuns')}
                </button>
                <button
                  type="button"
                  className="btn-ghost task-btn"
                  onClick={() => void runNow(task.id)}
                  disabled={busy}
                >
                  {t('tasks.runNow')}
                </button>
                <button type="button" className="btn-ghost task-btn" onClick={() => setForm(draftOf(task))}>
                  {t('tasks.edit')}
                </button>
                <button
                  type="button"
                  className="btn-danger task-btn"
                  onClick={() => void remove(task.id)}
                >
                  {confirmDelete === task.id ? t('tasks.deleteConfirm') : t('tasks.delete')}
                </button>
              </div>
              {expanded === task.id ? (
                <TaskRuns id={task.id} instanceId={instanceId} scoped={scoped} onOpenSession={onOpenSession} />
              ) : null}
            </div>
          );
        })}
      </div>
    </section>
  );
}

function TaskRuns({
  id,
  instanceId,
  scoped,
  onOpenSession,
}: {
  id: string;
  instanceId: string;
  scoped: typeof api;
  onOpenSession?: (agentId: string) => void;
}): ReactElement {
  const t = useI18n();
  const runs = useQuery({ queryKey: ['taskRuns', instanceId, id], queryFn: () => scoped.taskRuns(id) });
  const list = runs.data ?? [];
  if (list.length === 0) return <p className="task-runs task-runs--empty">{t('tasks.noRuns')}</p>;
  return (
    <div className="task-runs">
      {list.map((r: TaskRun, i: number) => (
        <div key={`${r.fired_at}-${i}`} className="task-run">
          <span className={`task-dot task-dot--${r.status}`} />
          <span className="task-run-status">{t(`tasks.run.${r.status}`)}</span>
          <span className="task-run-time">{r.fired_at.slice(11, 19)}</span>
          {r.duration_ms != null ? (
            <span className="task-run-duration">{r.duration_ms}ms</span>
          ) : null}
          {r.session_id != null ? (
            onOpenSession != null ? (
              <button
                type="button"
                className="task-run-session"
                title={t('tasks.openSession')}
                onClick={() => onOpenSession(r.session_id as string)}
              >
                {r.session_id}
              </button>
            ) : (
              <span className="task-run-session">{r.session_id}</span>
            )
          ) : null}
          {r.error != null ? <span className="task-run-error">{r.error}</span> : null}
        </div>
      ))}
    </div>
  );
}
