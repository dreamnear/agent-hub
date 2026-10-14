import { useEffect, useRef, useState, type ReactElement } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, type PiSettingsCfg } from '../api';
import { useI18n } from '../i18n';

/// Hub 助手（pi）分区（agent-hub-pi-hub-agent P3/P5/P9/P10）：完整 provider
/// 配置面——provider/model/thinking（pi 原生 settings.json 三键）+ 自定义
/// provider 条目（baseUrl/协议/ak/模型，models.json，条目名 = provider 值）+
/// 助手生命周期（启动/重启）+ pi 版本显示。
/// 脱敏红线：ak 只回显掩码（服务端），输入框留空 = 保留既有 key。
/// 保存即重启助手生效（P3 语义不变，服务端异步执行）。

const THINKING_LEVELS = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh'] as const;
const PROVIDER_APIS = [
  'anthropic-messages',
  'openai-completions',
  'openai-responses',
  'google-generative-ai',
] as const;

export default function SettingsPiHub(): ReactElement {
  const t = useI18n();
  const queryClient = useQueryClient();
  const settings = useQuery({
    queryKey: ['pi-settings'],
    queryFn: api.getPiSettings,
  });
  // P9：pi 版本（探测失败 → 容错降级提示，不阻塞分区其余功能）
  const version = useQuery({
    queryKey: ['pi-version'],
    queryFn: api.getPiVersion,
    staleTime: 60_000,
  });
  const [provider, setProvider] = useState('');
  const [model, setModel] = useState('');
  const [thinking, setThinking] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [apiType, setApiType] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [models, setModels] = useState('');
  const [allowInject, setAllowInject] = useState(false);
  // P13：允许助手控制定时任务（服务端闸，默认关）
  const [allowTasks, setAllowTasks] = useState(false);
  const [workDir, setWorkDir] = useState('');
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  // restart-feedback：保存触发的异步重启窗口——轮询 /api/pi/restart-status 收敛到
  // 成功/失败终态（C1/C2），窗口内禁用保存/重启（C3 防重点）。base = 保存前置
  // 快照（atMs 判「新记录」——与客户端时钟无关，spawn 多快都不会被快照吞掉）。
  const [restarting, setRestarting] = useState(false);
  const baseResultRef = useRef<number | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 服务端取数回填（仅在用户未编辑时——dirty 后不覆盖输入；ak 永远清空输入框，
  // 掩码走 placeholder——明文不出服务端，前端也不回填）
  useEffect(() => {
    const d = settings.data;
    if (d == null || dirty) return;
    setProvider(d.provider ?? '');
    setModel(d.model ?? '');
    setThinking(d.thinking ?? '');
    setBaseUrl(d.providerConfig?.baseUrl ?? '');
    setApiType(d.providerConfig?.api ?? '');
    setModels((d.providerConfig?.models ?? []).join(', '));
    setAllowInject(d.allowSessionInject);
    setAllowTasks(d.allowTaskControl);
    setWorkDir(d.workDir ?? '');
  }, [dirty, settings.data]);

  const edit = (apply: () => void): void => {
    apply();
    setDirty(true);
    setNote(null);
  };

  // restart-feedback：等待服务端异步重启收敛终态。PUT 的重启是 fire-and-forget
  // （tokio::spawn，成败静默）——bug 根因即「savedNote 过渡文案写入后永无更新」。
  // 前置快照在 PUT 前取（spawn 完成晚于 PUT 返回，新记录必然新于快照）；30s 无
  // 新记录降级为超时失败提示（不静默常驻「重启中」）。
  const waitRestart = async (): Promise<void> => {
    setRestarting(true);
    setNote(t('settings.piHub.savedNote'));
    try {
      for (let i = 0; i < 30; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        const st = await api.getPiRestartStatus();
        const lr = st.lastResult;
        if (lr != null && lr.atMs > (baseResultRef.current ?? Number.MIN_SAFE_INTEGER)) {
          if (lr.ok) {
            setNote(t('settings.piHub.restartedOkNote'));
          } else {
            setNote(null);
            setError(
              t('settings.piHub.restartedFailNote', { reason: lr.error ?? t('settings.piHub.restartNoReason') }),
            );
          }
          return;
        }
      }
      setNote(null);
      setError(t('settings.piHub.restartedTimeoutNote'));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRestarting(false);
      // 终态后侧栏/会话条目刷新（pi 条目回来/状态更新）
      void queryClient.invalidateQueries({ queryKey: ['agents'] });
      void queryClient.invalidateQueries({ queryKey: ['pi-settings'] });
    }
  };

  const save = async (): Promise<void> => {
    setSaving(true);
    setError(null);
    try {
      // 前置快照（PUT 前）：确保 spawn 结果记录必然新于快照
      const base = await api.getPiRestartStatus();
      baseResultRef.current = base.lastResult?.atMs ?? null;
      // 表单全量提交：apiKey 仅在用户输入了新值时携带（留空 = 保留既有）
      const body: Parameters<typeof api.putPiSettings>[0] = {
        provider: provider.trim(),
        model: model.trim(),
        thinking: thinking,
        allowSessionInject: allowInject,
        allowTaskControl: allowTasks,
        workDir: workDir.trim(),
        baseUrl: baseUrl.trim(),
        api: apiType,
      };
      const modelIds = models
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      if (modelIds.length > 0) body.models = modelIds; // 空数组不带（None=未用自定义段）
      if (apiKey.trim() !== '') body.apiKey = apiKey.trim();
      const saved: PiSettingsCfg = await api.putPiSettings(body);
      setProvider(saved.provider ?? '');
      setModel(saved.model ?? '');
      setThinking(saved.thinking ?? '');
      setBaseUrl(saved.providerConfig?.baseUrl ?? '');
      setApiType(saved.providerConfig?.api ?? '');
      setModels((saved.providerConfig?.models ?? []).join(', '));
      setAllowInject(saved.allowSessionInject);
      setAllowTasks(saved.allowTaskControl);
      setWorkDir(saved.workDir ?? '');
      setApiKey('');
      setDirty(false);
      // 助手重启中：等终态（成功/失败），窗口内 restarting 禁用按钮（C3）
      await waitRestart();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const restart = async (): Promise<void> => {
    setSaving(true);
    setError(null);
    try {
      // P5 重开入口：未运行 = 启动；运行中 = 幂等返回（配置变更走「保存并重启」）
      await api.startAgent({ cwd: '/', prompt: '' }, 'pi');
      setNote(t('settings.piHub.restartedNote'));
      void queryClient.invalidateQueries({ queryKey: ['agents'] });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="set-group" aria-label={t('settings.piHub.aria')}>
      <div className="set-row">
        <span className="set-label">{t('settings.piHub.piVersion')}</span>
        <span className="set-value">
          {version.isLoading
            ? '…'
            : (version.data?.version ?? t('settings.piHub.piVersionMissing'))}
        </span>
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-provider">
          {t('settings.piHub.provider')}
        </label>
        <input
          id="set-pi-provider"
          className="set-select set-text-input"
          type="text"
          value={provider}
          placeholder={t('settings.piHub.providerPlaceholder')}
          onChange={(e) => edit(() => setProvider(e.target.value))}
        />
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-model">
          {t('settings.piHub.model')}
        </label>
        <input
          id="set-pi-model"
          className="set-select set-text-input"
          type="text"
          value={model}
          placeholder={t('settings.piHub.modelPlaceholder')}
          onChange={(e) => edit(() => setModel(e.target.value))}
        />
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-thinking">
          {t('settings.piHub.thinking')}
        </label>
        <select
          id="set-pi-thinking"
          className="set-select"
          value={thinking}
          onChange={(e) => edit(() => setThinking(e.target.value))}
        >
          <option value="">{t('settings.piHub.thinkingDefault')}</option>
          {THINKING_LEVELS.map((lv) => (
            <option key={lv} value={lv}>
              {lv}
            </option>
          ))}
        </select>
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-baseurl">
          {t('settings.piHub.baseUrl')}
        </label>
        <input
          id="set-pi-baseurl"
          className="set-select set-text-input"
          type="text"
          value={baseUrl}
          placeholder={t('settings.piHub.baseUrlPlaceholder')}
          onChange={(e) => edit(() => setBaseUrl(e.target.value))}
        />
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-api">
          {t('settings.piHub.apiType')}
        </label>
        <select
          id="set-pi-api"
          className="set-select"
          value={apiType}
          onChange={(e) => edit(() => setApiType(e.target.value))}
        >
          <option value="">{t('settings.piHub.apiTypeDefault')}</option>
          {PROVIDER_APIS.map((a) => (
            <option key={a} value={a}>
              {a}
            </option>
          ))}
        </select>
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-apikey">
          {t('settings.piHub.apiKey')}
        </label>
        <input
          id="set-pi-apikey"
          className="set-select set-text-input"
          type="password"
          value={apiKey}
          autoComplete="off"
          placeholder={settings.data?.providerConfig?.apiKeyMasked ?? t('settings.piHub.apiKeyPlaceholder')}
          onChange={(e) => edit(() => setApiKey(e.target.value))}
        />
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-models">
          {t('settings.piHub.models')}
        </label>
        <input
          id="set-pi-models"
          className="set-select set-text-input"
          type="text"
          value={models}
          placeholder={t('settings.piHub.modelsPlaceholder')}
          onChange={(e) => edit(() => setModels(e.target.value))}
        />
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-allowinject">
          {t('settings.piHub.allowInject')}
        </label>
        <select
          id="set-pi-allowinject"
          className="set-select"
          value={allowInject ? 'on' : 'off'}
          onChange={(e) => edit(() => setAllowInject(e.target.value === 'on'))}
        >
          <option value="off">{t('settings.piHub.allowInjectOff')}</option>
          <option value="on">{t('settings.piHub.allowInjectOn')}</option>
        </select>
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-allowtasks">
          {t('settings.piHub.allowTasks')}
        </label>
        <select
          id="set-pi-allowtasks"
          className="set-select"
          value={allowTasks ? 'on' : 'off'}
          onChange={(e) => edit(() => setAllowTasks(e.target.value === 'on'))}
        >
          <option value="off">{t('settings.piHub.allowTasksOff')}</option>
          <option value="on">{t('settings.piHub.allowTasksOn')}</option>
        </select>
      </div>
      <div className="set-row">
        <label className="set-label" htmlFor="set-pi-workdir">
          {t('settings.piHub.workDir')}
        </label>
        <input
          id="set-pi-workdir"
          className="set-select set-text-input"
          type="text"
          value={workDir}
          placeholder={t('settings.piHub.workDirPlaceholder')}
          onChange={(e) => edit(() => setWorkDir(e.target.value))}
        />
      </div>
      <div className="set-row">
        <span className="set-label">{t('settings.piHub.apply')}</span>
        <span className="set-value">
          {/* C3：保存触发的异步重启窗口内双按钮禁用（防双重启）；saving 覆盖
              PUT 在途与手动重启在途（既有语义） */}
          <button
            type="button"
            className="btn-ghost"
            disabled={saving || restarting}
            onClick={() => void save()}
          >
            {t('settings.piHub.save')}
          </button>
          <button
            type="button"
            className="btn-ghost"
            disabled={saving || restarting}
            onClick={() => void restart()}
          >
            {t('settings.piHub.restartAssistant')}
          </button>
        </span>
      </div>
      <p className="set-hint">{t('settings.piHub.hint')}</p>
      {note != null && (
        <p className="set-hint" role="status">
          {note}
        </p>
      )}
      {error != null && (
        <p className="set-hint" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
