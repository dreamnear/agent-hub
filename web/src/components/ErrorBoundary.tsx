import { Component, type ErrorInfo, type ReactElement } from 'react';
import { t } from '../i18n';

interface Props {
  /// 包裹的子树（数据形状异常时的崩溃隔离面）
  children: ReactElement;
}

interface State {
  error: Error | null;
}

/**
 * 会话面板错误边界（#19 家族 MINOR：详情端点响应形状错误时 ChatTab 崩溃白屏）。
 * 渲染期异常兜成可读错误卡 + 重试（重挂子树；若坏数据仍在，边界再兜住，不白屏）。
 * 只做渲染兜底，不做错误上报（hub 无上报通道）。
 */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error('chat pane render error:', error, info.componentStack);
  }

  private readonly retry = (): void => {
    this.setState({ error: null });
  };

  render(): ReactElement {
    if (this.state.error != null) {
      return (
        <div className="chat-session-error" role="alert">
          <span>{t('chat.renderFailed')}</span>
          <button type="button" onClick={this.retry}>
            {t('chat.retry')}
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
