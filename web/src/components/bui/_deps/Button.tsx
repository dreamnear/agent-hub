// Beautiful UI 备用组件依赖桩（批5）：官网 atoms/Button 的最小等价实现。
// 备用组件不上页面；启用时按批5 终验以官网原版替换。
import type { MouseEvent, ReactElement, ReactNode } from 'react';

export function Button({
  children,
  variant = 'accent',
  size = 'sm',
  type = 'button',
  disabled = false,
  className = '',
  onClick,
}: {
  children?: ReactNode;
  variant?: 'accent' | 'quiet' | 'secondary' | 'ghost';
  size?: 'xs' | 'sm';
  type?: 'button' | 'submit';
  disabled?: boolean;
  className?: string;
  onClick?: (e: MouseEvent<HTMLButtonElement>) => void;
}): ReactElement {
  const variantCls =
    variant === 'accent'
      ? 'bg-accent text-white hover:brightness-110'
      : variant === 'quiet'
        ? 'text-ink-2 hover:bg-hover hover:text-ink'
        : variant === 'secondary'
          ? 'border border-line text-ink-2 hover:bg-hover hover:text-ink'
          : 'text-ink-2 hover:text-ink';
  const sizeCls = size === 'xs' ? 'h-6 px-1.5 text-[11px]' : 'h-7 px-2.5 text-[12px]';
  return (
    <button
      type={type}
      disabled={disabled}
      onClick={onClick}
      className={`inline-flex items-center justify-center gap-1 rounded-full font-medium select-none transition-[background-color,color,transform] duration-150 active:scale-[0.96] disabled:pointer-events-none disabled:opacity-50 ${variantCls} ${sizeCls} ${className}`}
    >
      {children}
    </button>
  );
}
