// @vitest-environment happy-dom
// 会话面板错误边界（#19 家族 MINOR 2）：详情端点畸形响应等渲染期异常兜成错误卡，
// 不再白屏整主区；重试清错重挂子树。
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ReactElement } from 'react';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import ErrorBoundary from './ErrorBoundary';

function Bomb(): never {
  throw new Error('malformed detail payload');
}

afterEach(() => {
  cleanup();
  vi.spyOn(console, 'error').mockRestore();
});

describe('ErrorBoundary（ChatTab 渲染兜底）', () => {
  it('子树渲染异常 → 错误卡（不白屏），重试后正常子树恢复', () => {
    // 静音边界内的 console.error（React 会打印捕获的异常）
    vi.spyOn(console, 'error').mockImplementation(() => {});
    let boom = true;
    // 子组件渲染期读外部态（children 元素初渲即定型，boom 开关必须在子组件内生效）
    function MaybeBomb(): ReactElement {
      if (boom) return <Bomb />;
      return <p data-testid="ok">fine</p>;
    }
    const { container } = render(
      <ErrorBoundary>
        <MaybeBomb />
      </ErrorBoundary>,
    );
    expect(container.textContent).toContain('会话渲染异常');
    expect(container.textContent).toContain('重试');
    // 重试：清错重挂——坏数据已不在（boom=false）→ 子树恢复
    boom = false;
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    expect(screen.getByTestId('ok')).not.toBeNull();
  });

  it('正常子树直通渲染（无边界痕迹）', () => {
    render(
      <ErrorBoundary>
        <p data-testid="ok">fine</p>
      </ErrorBoundary>,
    );
    expect(screen.getByTestId('ok')).not.toBeNull();
    expect(screen.queryByText('会话渲染异常')).toBeNull();
  });
});
