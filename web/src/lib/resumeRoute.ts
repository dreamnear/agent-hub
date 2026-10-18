// 死会话原地恢复路由（agent-hub-resume-inplace #21）。
// 需求路由表的前端判定口径（唯一事实源）：恢复一条历史会话时，走「复用现有行 /
// 向当前选中 TUI 注入 /resume 原地装载 / 回退 spawn 新行」中的哪条路，由本模块
// 纯函数裁决——组件只执行，便于四分支单测锚定（运行中 / 死+idle / 死+忙 / 无活 agent）。
//
// 实测依据（2026-10-18 主会话探针，需求文档「技术事实」）：
// - 活 TUI 注入 `/resume <sessionId>`（既有 /message 管道）→ 原地装载，hub 的
//   sessionId 跟踪自动跟随（watcher ≤8s），行与任务 id 不变
// - 运行中会话注入被 CLI 拒绝（stop-first）→ 运行中只能复用切视图
// - TUI 忙时注入被静默吞 → 忙时禁注入，回退 spawn（busy 策略选型 b，见 coder 报告）

import type { AgentSummary } from '../types';

/// spawn 回退原因（toast 文案分流用；busy 有专属说明，其余走通用新行文案）：
/// - 'busy'：当前选中 TUI 不可注入——working（轮次进行中注入被吞）或
///   needs_input（AskUserQuestion/权限确认挂起中，注入会写进挂起框替用户应答）
/// - 'no-agent'：无选中行，或选中行不在活跃清单（historySel 快照兜底的死行，
///   无活 TUI 可装载）
/// - 'driver'：选中行或入口行非 claude 驱动（/resume 注入是 Claude Code TUI 能力；
///   omp 等走各自后端 resume 路径）
/// - 'cwd'：选中行与入口行不同工程（跨项目注入会把新轮次写进异工程 cwd 的转录，
///   污染原会话记录——可预期性优先，回退 spawn）
export type ResumeSpawnReason = 'busy' | 'no-agent' | 'driver' | 'cwd';

/// 注入 sessionId 白名单：claude 会话 id 恒为 UUID（转录文件名 = <uuid>.jsonl）。
/// 黑名单（空/`/`/`\`/`..`）封不住换行——注入文本按行处理，`/resume <sid>\nX`
/// 的第二行 X 会被 TUI 当独立命令执行（命令分隔向量），故收紧为 UUID 正则
export function isInjectableSessionId(sid: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(sid);
}

export type ResumeRoute =
  /// 目标会话活着（活跃清单 sessionId 命中，含入口行自己持有）→ 复用现有行，
  /// 后端 resume-session 三态判定（active/current）照旧
  | { kind: 'reuse' }
  /// 死会话 + 当前选中 agent 活着（活跃清单命中）且可注入（非 working/
  /// needs_input）且同工程 → 向选中 TUI 注入 `/resume`，行与任务 id 不变，
  /// sessionId 由 watcher 跟随
  | { kind: 'inplace'; agent: AgentSummary }
  /// 其余 → spawn `--bg --resume`（现状路径），reason 供 toast 说明
  | { kind: 'spawn'; reason: ResumeSpawnReason };

/// 路由裁决（纯函数，不发起任何请求）：
/// - sessionId：目标历史会话 id
/// - entry：弹窗入口行 agent（决定会话清单的工程与驱动）
/// - activeAgents：轮询活跃清单（活进程）
/// - selected：主视图当前选中行（null = 无选中）
export function resolveResumeRoute(params: {
  sessionId: string;
  entry: AgentSummary;
  activeAgents: AgentSummary[];
  selected: AgentSummary | null;
}): ResumeRoute {
  const { sessionId, entry, activeAgents, selected } = params;
  // 路由 1 运行中：活跃清单 sessionId 命中 → 复用（现状保留，不新开）。
  // 前端轮询数据判活；后端三态在竞态窗口内仍会兜底 active/current，行为收敛一致
  if (activeAgents.some((a) => a.sessionId === sessionId)) return { kind: 'reuse' };
  // 路由 4（兜底细分）：无选中行 → spawn（现状路径保留）
  if (selected == null) return { kind: 'spawn', reason: 'no-agent' };
  // 选中行活判（需求条目 2 前提「活着」）：App 的 selected 可能来自 historySel
  // 快照兜底（daemon 死后窗口不限时），快照的 group/cwd/driver 全陈旧——把
  // /message 打到死 agent 会触发后端 job_needs_respawn 静默 respawn 复活（路由
  // 表外重副作用）。不在活跃清单 = 无活 TUI 可装载，回退 spawn
  const selAlive = activeAgents.some(
    (a) => a.id === selected.id && (a.instanceId ?? null) === (selected.instanceId ?? null),
  );
  if (!selAlive) return { kind: 'spawn', reason: 'no-agent' };
  // /resume 注入是 Claude Code TUI 能力：入口或选中行非 claude 驱动 → 各走各的
  // 后端 resume 路径（omp = ACP session/load），不注入
  if (entry.driver !== 'claude' || selected.driver !== 'claude') {
    return { kind: 'spawn', reason: 'driver' };
  }
  // 路由 3 忙：两类不可注入态——working（轮次进行中，注入被静默吞，探针实证）
  // 与 needs_input（blocked = AskUserQuestion/权限确认挂起中，注入写进挂起框会
  // 替用户应答 agent 的问题/确认）→ 回退 spawn + toast 说明（busy 策略选型 b：
  // 立即、可预期；选型 a 的有界等待引入时延与二次超时分支，且超时后仍要回
  // spawn，路径不增益——取舍详见 coder 报告）
  if (selected.group === 'working' || selected.group === 'needs_input') {
    return { kind: 'spawn', reason: 'busy' };
  }
  // 同工程守卫：TUI 只该装载自己工程的历史会话（cwd 不同 = 另一工程/worktree，
  // 注入会让异工程 cwd 写进原转录）。cwd 缺失一律视为不可注入
  if (selected.cwd == null || selected.cwd !== entry.cwd) {
    return { kind: 'spawn', reason: 'cwd' };
  }
  // 路由 2 死会话 + 选中行活着且 idle → 原地注入
  return { kind: 'inplace', agent: selected };
}
