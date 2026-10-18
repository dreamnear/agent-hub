// 原地恢复装载跟随（agent-hub-resume-inplace #21 路由 2 后半段）。
// `/resume` 注入成功后（/message 管道 204），TUI 原地装载历史对话，hub 侧的
// sessionId 跟踪由 watcher 刷新（实测 ≤8s）——本 hook 有界等待该刷新落地：
// - 命中：目标行 sessionId 变为被恢复会话 → 装载确认，静默收尾（消息视图按
//   sessionId 分页端点自然跟随，无需额外动作）
// - 超时：会话文件缺失等导致 TUI 装载失败，sessionId 永不变化 → 明确报错
//   （onTimeout 回调，需求「恢复失败 → 明确错误，视图不动」）
// - 心跳：等待期内周期性 refresh（App 侧 = 失效 agents 查询），加速 watcher
//   结果拾取，不让用户干等轮询周期
// 等待有界（禁无界等待）、结果可预期（命中或明确超时，禁静默吞）——busy/失败
// 纪律见需求边界节。

import { useCallback, useEffect, useRef, useState } from 'react';
import type { AgentSummary } from '../types';

/** 有界等待上限：watcher 实测 ≤8s，心跳 3s × 5 跳 + 余量 */
export const RESUME_WATCH_TIMEOUT_MS = 15_000;
/** 心跳间隔：每跳 refresh 一次活跃清单并复查命中 */
export const RESUME_WATCH_HEARTBEAT_MS = 3_000;

export interface ResumeWatchTarget {
  agentId: string;
  /** 复合键实例维（跨实例 agentId 可能碰撞，选中键同款口径） */
  instanceId: string | null;
  targetSessionId: string;
}

interface Params {
  /** 活跃清单（App 轮询派生数据） */
  agents: AgentSummary[];
  /** 超时回调（App 侧 toast 明确报错；用 ref 承接防回调引用变化重置心跳） */
  onTimeout: () => void;
  /** 加速拾取（App 侧失效 agents 查询；同样 ref 承接） */
  refresh: () => void;
  timeoutMs?: number;
  heartbeatMs?: number;
}

export function useInplaceResumeWatch(params: Params): (t: ResumeWatchTarget) => void {
  const { agents, timeoutMs = RESUME_WATCH_TIMEOUT_MS, heartbeatMs = RESUME_WATCH_HEARTBEAT_MS } =
    params;
  const [watch, setWatch] = useState<(ResumeWatchTarget & { deadline: number }) | null>(null);
  // 回调走 ref：调用方传入的 onTimeout/refresh 引用每渲染可能变化（如依赖 t），
  // 进 effect deps 会每跳重置 setTimeout 心跳——ref 只在触发瞬间取最新值
  const onTimeoutRef = useRef(params.onTimeout);
  onTimeoutRef.current = params.onTimeout;
  const refreshRef = useRef(params.refresh);
  refreshRef.current = params.refresh;

  useEffect(() => {
    if (watch == null) return;
    const hit = agents.find(
      (a) => a.id === watch.agentId && (a.instanceId ?? null) === watch.instanceId,
    );
    // 装载确认：watcher 已把该行 sessionId 跟随到目标会话，视图自然呈现，静默收尾
    if (hit?.sessionId === watch.targetSessionId) {
      setWatch(null);
      return;
    }
    // 有界超时：sessionId 未跟随 = TUI 未装载（会话文件缺失等）→ 明确报错
    if (Date.now() >= watch.deadline) {
      setWatch(null);
      onTimeoutRef.current();
      return;
    }
    // 心跳：refresh 拉新清单 + 自触发复查（新对象引用驱动 effect 重跑）
    const timer = setTimeout(() => {
      refreshRef.current();
      setWatch((w) => (w == null ? null : { ...w }));
    }, heartbeatMs);
    return () => clearTimeout(timer);
  }, [watch, agents, heartbeatMs]);

  return useCallback(
    (t: ResumeWatchTarget) => {
      setWatch({ ...t, deadline: Date.now() + timeoutMs });
      // 启动即拉一次基线（注入已在 Dialog 完成，尽快发现 watcher 刷新）
      refreshRef.current();
    },
    [timeoutMs],
  );
}
