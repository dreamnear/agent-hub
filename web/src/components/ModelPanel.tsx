import { useMemo, useState, type ReactElement } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api, type ModelInfo } from '../api';
import { useI18n } from '../i18n';
import './HistoryDialog.css';
import './ModelPanel.css';

interface Props {
  sessionId: string;
  /** 驱动路由（外观透传的目标驱动；面板不显示该名字） */
  driver: string;
  /** per-instance 路由（本机缺省）；与 ChatTab 的 api 同源 */
  onSelect: () => void;
  onClose: () => void;
  api?: typeof api;
}

/// 面板渲染上限（ceiling：opencode 目录数千条，全量渲染会卡——免费档前置已保
/// 首屏可用；搜索/虚拟滚动待会话量反馈再上）
const MAX_RENDER = 300;

/// 统一模型/思考选择面板（agent-hub-model-thinking）：用户面前只有 hub——
/// 只出现模型名与思考档位，不出现驱动字样；条目 = 模型（免费档前置），
/// 选中模型后展开其思考档位（模型无思考语义则只切模型）。
/// 设置即写驱动（opencode=会话侧持久；claude=/model 注入），收场文案如实展示
///（「已切换 / 已排队」）；hub 不存会话模型状态。
export default function ModelPanel({
  sessionId,
  driver,
  onSelect,
  onClose,
  api: apiOverride,
}: Props): ReactElement {
  const t = useI18n();
  const [pendingModel, setPendingModel] = useState<string | null>(null);
  const [expandedVariants, setExpandedVariants] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<string | null>(null);
  const effectiveApi = apiOverride ?? api;
  const list = useQuery({
    queryKey: ['model-settings', sessionId],
    queryFn: () => effectiveApi.getModelSettings(sessionId, driver),
    staleTime: 30_000,
  });

  const models = list.data?.models ?? [];
  const current = list.data?.current ?? null;
  const caps = list.data?.caps;

  // 免费档前置（后端已排），这里只做展示截断
  const visible = useMemo(() => models.slice(0, MAX_RENDER), [models]);
  const variantsOf = (m: ModelInfo | undefined): string[] => m?.variants ?? [];
  const currentInfo = useMemo(
    () => models.find((m) => m.id === current?.id && m.provider === current?.provider),
    [models, current],
  );

  const chooseModel = async (m: ModelInfo, variant?: string): Promise<void> => {
    setPendingModel(m.id);
    setOutcome(null);
    try {
      const res = await effectiveApi.setModelSettings(
        sessionId,
        {
          model: m.id,
          ...(variant != null ? { variant } : {}),
        },
        driver,
      );
      setOutcome(res.message);
      await list.refetch();
      onSelect(); // 通知外层刷新（芯片回读驱动事实）
    } catch (e) {
      setOutcome(String(e));
    } finally {
      setPendingModel(null);
    }
  };

  const setThinking = async (variant: string): Promise<void> => {
    if (current == null) return;
    await chooseModel(
      { id: current.id, provider: current.provider, name: current.id, free: false, variants: [] },
      variant,
    );
  };

  return (
    <div
      className="dialog-backdrop confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t('model.title')}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') onClose();
      }}
    >
      <div className="dialog-card history-card model-card">
        <div className="history-head">
          <h2 className="history-title">{t('model.title')}</h2>
          <span className="history-sub">
            {current != null
              ? `${currentInfo?.name ?? current.id}${current.variant != null ? ` · ${current.variant}` : ''}`
              : t('model.unmanaged')}
          </span>
        </div>
        {caps?.supportsThinking && current != null && variantsOf(currentInfo).length > 0 ? (
          <div className="model-variants" role="group" aria-label={t('model.thinking')}>
            <span className="model-variants-label">{t('model.thinking')}</span>
            {variantsOf(currentInfo).map((v) => (
              <button
                key={v}
                type="button"
                className={`model-chip ${current.variant === v ? 'model-chip--on' : ''}`}
                disabled={pendingModel != null}
                onClick={() => void setThinking(v)}
              >
                {v}
              </button>
            ))}
          </div>
        ) : null}
        {list.isLoading ? (
          <div className="history-empty">{t('sidebar.loading')}</div>
        ) : list.isError ? (
          <div className="history-empty">
            {String(list.error)}
            <button type="button" className="btn-ghost" onClick={() => void list.refetch()}>
              {t('history.retry')}
            </button>
          </div>
        ) : (
          <ul className="model-list">
            {visible.map((m) => {
              const isCurrent = current?.id === m.id && current?.provider === m.provider;
              const showVariants =
                (caps?.supportsThinking ?? false) &&
                expandedVariants === m.id &&
                variantsOf(m).length > 0 &&
                !isCurrent;
              return (
                <li key={`${m.provider}:${m.id}`}>
                  <div className="model-row-wrap">
                    <button
                      type="button"
                      className={`model-row ${isCurrent ? 'model-row--cur' : ''}`}
                      onClick={() => {
                        if (isCurrent) return;
                        if ((caps?.supportsThinking ?? false) && variantsOf(m).length > 0) {
                          // 有思考档位的模型：先展开档位（二段选择），再落设置
                          setExpandedVariants((v) => (v === m.id ? null : m.id));
                        } else {
                          void chooseModel(m);
                        }
                      }}
                      disabled={pendingModel != null}
                    >
                      <span className="model-name">
                        {m.name}
                        {m.free ? <span className="model-free">{t('model.free')}</span> : null}
                      </span>
                      <span className="model-provider">{m.provider}</span>
                    </button>
                    {showVariants ? (
                      <div className="model-variants model-variants--inline" role="group">
                        {variantsOf(m).map((v) => (
                          <button
                            key={v}
                            type="button"
                            className="model-chip"
                            disabled={pendingModel != null}
                            onClick={() => void chooseModel(m, v)}
                          >
                            {v}
                          </button>
                        ))}
                      </div>
                    ) : null}
                  </div>
                </li>
              );
            })}
            {models.length > visible.length ? (
              <li className="history-empty">{t('model.capped', { n: models.length })}</li>
            ) : null}
          </ul>
        )}
        {outcome != null ? <div className="model-outcome">{outcome}</div> : null}
      </div>
    </div>
  );
}
