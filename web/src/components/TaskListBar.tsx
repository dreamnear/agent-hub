import './TaskListBar.css';
import type { ReactElement } from 'react';
import type { TaskItem } from '../hooks/combineToolCalls';
import { useI18n } from '../i18n';
import BuiTaskRows, { type TaskRow } from './bui/TaskRows';

// preview 反馈：未完成的在前（in_progress → pending），已完成沉底；同状态保持出现顺序（稳定排序）
const STATUS_ORDER: Record<TaskItem['status'], number> = {
  in_progress: 0,
  pending: 1,
  completed: 2,
};

/// 任务清单固定栏（preview 反馈二/三轮）：固定于会话窗口底部、不随消息滚动；
/// details 折叠默认收起，摘要行 = 排序后第一项标题…(总数)；展开完整清单。
/// 无任务时不渲染不占位（由调用方判空）。状态演进逻辑在 extractTaskList。
/// 视觉（Beautiful UI 批3）：清单行换 BUI TaskRows List 形态——
/// done=绿勾徽章+已完成 pill、in_progress=活跃 spinner（序号）、pending=静默环；
/// 排序/摘要/判空语义不变。
export default function TaskListBar({ tasks }: { tasks: TaskItem[] }): ReactElement {
  const t = useI18n();
  const sorted = [...tasks].sort((a, b) => STATUS_ORDER[a.status] - STATUS_ORDER[b.status]);
  const rows: TaskRow[] = sorted.map((t, i) => ({
    key: t.taskId,
    label: t.subject,
    amount: '',
    status: t.status === 'completed' ? 'done' : t.status === 'in_progress' ? 'running' : 'pending',
    step: i + 1,
    details: [],
  }));
  return (
    <details className="task-list-card task-bar" data-bui>
      <summary>
        <span className="task-bar-summary-text">
          {t('tasks.summary', { first: sorted[0].subject, n: tasks.length })}
        </span>
      </summary>
      <div role="list" aria-label={t('tasks.aria')}>
        <BuiTaskRows variant="List" rows={rows} labels={{ completed: t('tasks.completed'), failed: t('tasks.failed') }} className="!max-w-full" />
      </div>
    </details>
  );
}
