// @vitest-environment happy-dom
// AskUserQuestion 提问卡：claude 驱动提问的结构化渲染（替代裸 JSON dump）。
// 待答=只读选项+TUI 指引（远程作答另立项）；已答=展示答案；畸形输入回退 JSON。
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render } from '@testing-library/react';
import AskUserQuestionCard from './AskUserQuestionCard';
import type { ChatMessage } from '../types';

afterEach(() => cleanup());

const msg = (over: Partial<ChatMessage>): ChatMessage => ({
  kind: 'tool_use',
  rawType: 'tool_use',
  text: null,
  toolUseId: 'tu-1',
  toolName: 'AskUserQuestion',
  input: null,
  result: null,
  error: false,
  ts: null,
  ...over,
});

const QUESTIONS_INPUT = {
  questions: [
    {
      question: 'EdgeOne 版 CI 模板删还是留？',
      header: '环境变量',
      options: [
        { label: '保留作历史', description: '留档供对比' },
        { label: '删除', description: '零引用直接清' },
      ],
      multiSelect: false,
    },
    {
      question: 'D5 是否清理残留行？',
      header: '清理',
      options: [{ label: '清理（推荐）' }],
      multiSelect: false,
    },
  ],
};

describe('AskUserQuestionCard', () => {
  it('待答：结构化渲染 header/question/options（含描述），无假按钮，出 TUI 指引', () => {
    const { container } = render(
      <AskUserQuestionCard item={msg({ input: QUESTIONS_INPUT })} pending />,
    );
    expect(container.textContent).toContain('模型向你提问');
    expect(container.textContent).toContain('EdgeOne 版 CI 模板删还是留？');
    expect(container.textContent).toContain('保留作历史');
    expect(container.textContent).toContain('留档供对比');
    // 两问各一 fieldset
    expect(container.querySelectorAll('fieldset')).toHaveLength(2);
    // 选项只读（span 列表，非 button——claude 驱动无远程作答通道，不放假交互）
    expect(container.querySelector('.dsh-question-options button')).toBeNull();
    expect(container.querySelectorAll('.dsh-question-option')).toHaveLength(3);
    // 指引 = 既有 inputPendingAsk 文案
    expect(container.textContent).toContain('TUI 模态中');
  });

  it('已答：标题带已答标记，result 文本作答案展示', () => {
    const { container } = render(
      <AskUserQuestionCard
        item={msg({ input: QUESTIONS_INPUT, result: '保留作历史/对比' })}
        pending={false}
      />,
    );
    expect(container.textContent).toContain('已回答');
    expect(container.textContent).toContain('回答');
    expect(container.textContent).toContain('保留作历史/对比');
  });

  it('result 为 {text} 包装时取 text 字段', () => {
    const { container } = render(
      <AskUserQuestionCard
        item={msg({ input: QUESTIONS_INPUT, result: { text: 'D3 token 处置+兼容读法' } })}
        pending={false}
      />,
    );
    expect(container.textContent).toContain('D3 token 处置+兼容读法');
  });

  it('畸形输入（无 questions 数组）→ 退回文本展示，不吞信息', () => {
    const { container } = render(
      <AskUserQuestionCard item={msg({ input: { foo: 1 }, text: 'raw fallback' })} pending />,
    );
    expect(container.textContent).toContain('raw fallback');
  });
});
