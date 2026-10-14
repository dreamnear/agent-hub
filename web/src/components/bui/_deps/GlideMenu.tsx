// Beautiful UI 备用组件依赖桩（批5）：官网 primitives/GlideMenu 的最小等价实现
// （滑动高亮容器；桩仅保留菜单语义，滑动高亮/音效为演示增强）。
import type { ReactElement, ReactNode } from 'react';

export default function GlideMenu({
  children,
  className = '',
}: {
  children?: ReactNode;
  className?: string;
  highlightClassName?: string;
}): ReactElement {
  return (
    <div role="menu" className={className}>
      {children}
    </div>
  );
}
