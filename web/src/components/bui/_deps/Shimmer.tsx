// Beautiful UI 备用组件依赖桩（批5）：官网 atoms/Shimmer 的最小等价实现
// （流光文案容器，keyframes shimmer-text 已在 src/bui/bui-motion.css）。
import type { ReactElement, ReactNode } from 'react';

export function Shimmer({ children, className = '' }: { children?: ReactNode; className?: string }): ReactElement {
  return (
    <span
      className={`bg-clip-text text-transparent ${className}`}
      style={{
        backgroundImage: 'linear-gradient(90deg, var(--ink-3) 35%, var(--ink) 50%, var(--ink-3) 65%)',
        backgroundSize: '200% 100%',
        animation: 'shimmer-text 1.4s linear infinite',
      }}
    >
      {children}
    </span>
  );
}
