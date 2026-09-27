// Beautiful UI 备用组件依赖桩（批5）：liveline 图表库的最小等价实现——
// Liveline 为 React canvas 图表组件；桩渲染 no-op 空舞台，props 宽容收下
// （启用时以官网原版 npm liveline 替换）。 /* ponytail: 备用桩，精确类型无价值 */
import type { ReactElement } from 'react';

export type LivelinePoint = { time: number; value: number };

export type LivelineSeries = {
  id: string;
  label: string;
  data: LivelinePoint[];
  value?: number;
  color?: string;
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function Liveline(_: any): ReactElement {
  return (
    <div
      aria-hidden="true"
      style={{ width: '100%', height: '100%', display: 'flex', alignItems: 'flex-end', justifyContent: 'center', color: 'var(--ink-3)', fontSize: 11 }}
    >
      [chart]
    </div>
  );
}
