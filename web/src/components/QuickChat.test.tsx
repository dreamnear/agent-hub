// @vitest-environment happy-dom
// ⌘K 快速选择器（P5）：Esc 统一键控（#20）——焦点在 body 也生效（原 onKeyDown
// 挂 backdrop div，点过 backdrop 后焦点回 body，Esc 失效）。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import QuickChat from './QuickChat';
import type { AgentSummary } from '../types';

const mk = (id: string, name: string): AgentSummary => ({
  driver: 'claude',
  id,
  name,
  cwd: '/repo',
  kind: 'background',
  rawState: null,
  group: 'other',
  detail: null,
  tokens: null,
  context_tokens: null,
  startedAt: null,
  sessionId: null,
});

afterEach(cleanup);

describe('QuickChat（⌘K 切换器）', () => {
  it('Esc 关闭面板（焦点在 body），onClose 恰一次（#20）', () => {
    const onClose = vi.fn();
    render(<QuickChat agents={[mk('a1', '会话甲')]} onPick={vi.fn()} onClose={onClose} />);
    expect(screen.getByRole('dialog')).not.toBeNull();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('候选过滤 + 选中上抛', () => {
    const onPick = vi.fn();
    const onClose = vi.fn();
    render(
      <QuickChat agents={[mk('a1', '会话甲'), mk('a2', '会话乙')]} onPick={onPick} onClose={onClose} />,
    );
    fireEvent.click(screen.getByRole('option', { name: /会话乙/ }));
    expect(onPick).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
