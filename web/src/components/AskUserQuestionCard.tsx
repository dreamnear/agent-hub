import { ReactElement } from 'react';
import { useI18n } from '../i18n';
import type { ChatMessage } from '../types';

interface ParsedQuestion {
  question?: unknown;
  header?: unknown;
  options?: unknown;
  multiSelect?: unknown;
}

interface ParsedOption {
  label?: unknown;
  description?: unknown;
}

/**
 * claude 驱动 AskUserQuestion 提问卡（2026-10-06 验收反馈）：结构化渲染
 * header/question/options，替代 ToolCallCardView 的裸 JSON dump。
 * - 待答（会话末尾无 tool_result）：选项只读列出 + TUI 作答指引——claude 驱动
 *   answer_question/answer_permission 均 unsupported（drivers/claude/mod.rs），
 *   web 远程作答需 PTY 按键注入，独立特性另立项；此处不放假按钮。
 * - 已答：result 文本（选项 label 或自填文本）作答案件展示。
 * 样式复用 dsh-question-*（同一设计语言，App.css 共享）。
 */
export default function AskUserQuestionCard({
  item,
  pending,
}: {
  item: ChatMessage;
  pending: boolean;
}): ReactElement {
  const t = useI18n();
  const input = (item.input ?? {}) as { questions?: unknown };
  const raw = Array.isArray(input.questions) ? (input.questions as ParsedQuestion[]) : [];
  const questions = raw
    .map((q) => ({
      question: typeof q.question === 'string' ? q.question : '',
      header: typeof q.header === 'string' ? q.header : '',
      multiSelect: q.multiSelect === true,
      options: (Array.isArray(q.options) ? (q.options as ParsedOption[]) : []).filter(
        (o): o is { label: string; description: string } =>
          typeof o.label === 'string' &&
          o.label !== '' &&
          (o.description == null || typeof o.description === 'string'),
      ),
    }))
    .filter((q) => q.question !== '' || q.options.length > 0);

  // 输入畸形（非 claude AskUserQuestion 形状）→ 退回 JSON 文本，不吞信息
  if (questions.length === 0) {
    return (
      <div className="dsh-question-card" role="alert">
        {item.text ?? JSON.stringify(item.input)}
      </div>
    );
  }

  const answer = item.result;
  const answerText =
    answer == null
      ? null
      : typeof answer === 'string'
        ? answer
        : typeof (answer as { text?: unknown }).text === 'string'
          ? (answer as { text: string }).text
          : JSON.stringify(answer);

  return (
    <div className="dsh-question-card" role="form" aria-label={t('chat.questionTitle')}>
      <div className="dsh-question-title">
        {t('chat.questionTitle')}
        {pending ? '' : ` · ${t('chat.questionAnswered')}`}
      </div>
      {questions.map((q, qi) => (
        <fieldset key={qi} className="dsh-question-item">
          {q.header ? <legend className="dsh-question-header">{q.header}</legend> : null}
          {q.question ? <div className="dsh-question-text">{q.question}</div> : null}
          {q.options.length > 0 ? (
            <ul className="dsh-question-options">
              {q.options.map((o) => (
                <li key={o.label} className="dsh-question-option is-static">
                  <span className="dsh-question-option-label">{o.label}</span>
                  {o.description ? (
                    <span className="dsh-question-option-desc">{o.description}</span>
                  ) : null}
                </li>
              ))}
            </ul>
          ) : null}
        </fieldset>
      ))}
      {pending ? (
        <div className="dsh-question-state">{t('chat.inputPendingAsk')}</div>
      ) : answerText != null ? (
        <div className="dsh-question-state">
          {t('chat.questionAnswerLabel')}：{answerText}
        </div>
      ) : null}
    </div>
  );
}
