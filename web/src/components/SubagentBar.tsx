import './SubagentBar.css';
import type { ReactElement } from 'react';
import type { SubagentEntry } from '../types';

/// subagent 折叠条（P5+ 反馈轮 6：形态对齐 TaskListBar，与会话底部任务清单并列）：
/// details/summary 默认收起，摘要 = 图标 + 总数（+ active 数）；展开为 chip 列表
/// （状态点 + 名字 + 描述 tooltip），点击切换只读查看，再点当前 chip 切回。
/// 空列表不渲染不占位（TaskListBar 同款判空）。
export default function SubagentBar({
  subagents,
  activeId,
  onSelect,
}: {
  subagents: SubagentEntry[];
  activeId: string | null;
  onSelect: (agentId: string) => void;
}): ReactElement | null {
  if (subagents.length === 0) return null;
  const activeCount = subagents.filter((s) => s.status === 'active').length;
  return (
    <details className="task-list-card subagent-card">
      <summary>
        <span className="subagent-summary-text">
          🤖 Subagent {subagents.length}
          {activeCount > 0 ? ` · ${activeCount} active` : ''}
        </span>
      </summary>
      <div className="subagent-chip-list" role="tablist" aria-label="subagent 会话">
        {subagents.map((s) => (
          <button
            key={s.agentId}
            type="button"
            role="tab"
            aria-selected={s.agentId === activeId}
            className={`subagent-chip ${s.agentId === activeId ? 'subagent-chip--active' : ''}`}
            title={s.description ?? s.agentType}
            onClick={() => onSelect(s.agentId)}
          >
            <span className={`subagent-dot subagent-dot--${s.status}`} />
            <span className="subagent-chip-name">{s.name}</span>
          </button>
        ))}
      </div>
    </details>
  );
}
