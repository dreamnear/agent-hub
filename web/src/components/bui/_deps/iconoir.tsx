// Beautiful UI 备用组件依赖桩（批5）：iconoir-react 图标的最小等价实现——
// 统一 15×15 currentColor 描边轮廓（形近占位），启用时按官网原版替换。
import type { ReactElement } from 'react';

function make(path: ReactElement): (props: Record<string, unknown>) => ReactElement {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return function Icono(props: any) {
    return (
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" className={props.className ?? ""} aria-hidden="true">
        {path}
      </svg>
    );
  };
}

export const ArrowUp = make(<><path d="M12 19V5M5 12l7-7 7 7" /></>);
export const ChatBubbleQuestion = make(<><path d="M21 12a9 9 0 1 1-4-7.5L21 4l-1 4" /><path d="M9.5 9a2.5 2.5 0 0 1 5 .3c0 1.5-2.5 2-2.5 3.2M12 16h.01" /></>);
export const Check = make(<path d="M20 6L9 17l-5-5" />);
export const EmojiSatisfied = make(<><circle cx="12" cy="12" r="9" /><path d="M8.5 14a4.5 4.5 0 0 0 7 0M9 9.5h.01M15 9.5h.01" /></>);
export const NavArrowRight = make(<path d="M9 5l7 7-7 7" />);
export const Refresh = make(<><path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6" /></>);
export const Scissor = make(<><circle cx="6" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" /><path d="M8 7.5L20 18M8 16.5L20 6" /></>);
export const Spark = make(<path d="M12 3l2 6 6 2-6 2-2 6-2-6-6-2 6-2z" />);
export const TextBox = make(<><rect x="4" y="4" width="16" height="16" rx="2" /><path d="M8 9h8M8 13h8M8 17h5" /></>);
export const Xmark = make(<path d="M18 6L6 18M6 6l12 12" />);
