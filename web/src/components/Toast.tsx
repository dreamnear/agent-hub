import type { ReactElement } from 'react';
import './Toast.css';

interface Props {
  /** 提示文案（调用方已翻译，含参数插值后的成品） */
  text: string;
}

/// 一次性提示条（agent-hub-pick-switch #18）：切换成功后的轻确认，不阻塞操作。
/// 定位固定屏幕下方居中、pointer-events 全禁（纯告知，不吃点击）；自动消失由
/// 挂载方（App 的 toast 状态 effect）计时卸载——组件本身无定时器，避免父级
/// 轮询重渲时计时器被反复重置而永不消失。
export default function Toast({ text }: Props): ReactElement {
  return (
    <div className="toast" role="status" aria-live="polite">
      {text}
    </div>
  );
}
