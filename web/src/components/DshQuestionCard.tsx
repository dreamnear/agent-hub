import { ReactElement, useState } from 'react';
import { useI18n } from '../i18n';
import { api } from '../api';
import type { ChatMessage } from '../types';

/** 单题作答状态：选项 label 集合 + 自由文本 */
interface AnswerDraft {
  selected: string[];
  custom: string;
}

interface Props {
  item: ChatMessage;
  agentId: string;
  driver: string;
}

/**
 * dsh ask_user_question 提问卡（2026-10-03）：问题 + 选项按钮（可多选）+ 自由
 * 文本 → 提交 → POST /question → hub 走 $events/result 回包，turn 继续。
 * dsh fail-closed 超时由 cancellation 帧 → hub 推收场条，卡本体不自带倒计时。
 */
export default function DshQuestionCard({ item, agentId, driver }: Props): ReactElement {
  const t = useI18n();
  const input = (item.input ?? {}) as {
    questions?: Array<{
      id: string;
      question: string;
      detail?: string;
      header?: string;
      options?: Array<{ label: string; description?: string }>;
      multiSelect?: boolean;
    }>;
    eventId?: string;
  };
  const questions = Array.isArray(input.questions) ? input.questions : [];
  const eventId = String(input.eventId ?? '');

  const [drafts, setDrafts] = useState<Record<string, AnswerDraft>>({});
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');
  const [done, setDone] = useState(false);

  const toggle = (qid: string, label: string, multi: boolean): void => {
    setDrafts((cur) => {
      const d = cur[qid] ?? { selected: [], custom: '' };
      const has = d.selected.includes(label);
      const selected = multi
        ? has
          ? d.selected.filter((x) => x !== label)
          : [...d.selected, label]
        : has
          ? []
          : [label];
      return { ...cur, [qid]: { ...d, selected } };
    });
  };
  const setCustom = (qid: string, text: string): void => {
    setDrafts((cur) => ({
      ...cur,
      [qid]: { selected: cur[qid]?.selected ?? [], custom: text },
    }));
  };

  const submit = async (): Promise<void> => {
    setBusy(true);
    setErr('');
    try {
      const answers = questions.map((q) => {
        const d = drafts[q.id] ?? { selected: [], custom: '' };
        return {
          id: q.id,
          selected: d.selected,
          ...(d.custom.trim() ? { custom: d.custom.trim() } : {}),
        };
      });
      await api.answerQuestion(agentId, eventId, answers, driver);
      setDone(true);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const anyDraft = questions.some((q) => {
    const d = drafts[q.id];
    return d && (d.selected.length > 0 || d.custom.trim() !== '');
  });

  if (questions.length === 0) {
    return (
      <div className="dsh-question-card" role="alert">
        {String(item.text ?? '')}
      </div>
    );
  }

  return (
    <div className="dsh-question-card" role="form" aria-label={t('chat.questionTitle')}>
      <div className="dsh-question-title">{t('chat.questionTitle')}</div>
      {questions.map((q) => {
        const d = drafts[q.id] ?? { selected: [], custom: '' };
        return (
          <fieldset key={q.id} className="dsh-question-item">
            {q.header ? <legend className="dsh-question-header">{q.header}</legend> : null}
            <div className="dsh-question-text">{q.question}</div>
            {q.detail ? <div className="dsh-question-detail">{q.detail}</div> : null}
            {Array.isArray(q.options) && q.options.length > 0 ? (
              <div className="dsh-question-options">
                {q.options.map((o) => (
                  <button
                    key={o.label}
                    type="button"
                    className={
                      d.selected.includes(o.label)
                        ? 'dsh-question-option is-selected'
                        : 'dsh-question-option'
                    }
                    disabled={done || busy}
                    onClick={() => toggle(q.id, o.label, q.multiSelect === true)}
                    aria-pressed={d.selected.includes(o.label)}
                  >
                    {o.label}
                    {o.description ? (
                      <span className="dsh-question-option-desc">{o.description}</span>
                    ) : null}
                  </button>
                ))}
              </div>
            ) : null}
            <label className="dsh-question-custom">
              <span>{t('chat.questionOther')}</span>
              <input
                type="text"
                value={d.custom}
                disabled={done || busy}
                onChange={(e) => setCustom(q.id, e.target.value)}
              />
            </label>
          </fieldset>
        );
      })}
      {done ? (
        <div className="dsh-question-state">{t('chat.questionAnswered')}</div>
      ) : (
        <div className="dsh-question-actions">
          <button
            type="button"
            className="btn-primary"
            disabled={!anyDraft || busy}
            onClick={() => void submit()}
          >
            {busy ? '…' : t('chat.questionSubmit')}
          </button>
          {err ? <span className="dsh-question-err">{err}</span> : null}
        </div>
      )}
    </div>
  );
}
