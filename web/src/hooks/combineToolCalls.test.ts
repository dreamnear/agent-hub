// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../types';
import { combineToolCalls, extractAcpPlan, extractTaskList, mergeAcpChunks } from './combineToolCalls';

function msg(partial: Partial<ChatMessage>): ChatMessage {
  return {
    kind: 'user',
    rawType: null,
    text: null,
    toolUseId: null,
    toolName: null,
    input: null,
    result: null,
    error: null,
    ts: null,
    ...partial,
  };
}

describe('combineToolCalls', () => {
  it('merges tool_use with its tool_result into one card', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't1', input: { command: 'ls' } }),
      msg({ kind: 'tool_result', toolUseId: 't1', error: false }),
    ]);
    expect(items).toHaveLength(1);
    const card = items[0] as Extract<(typeof items)[number], { kind: 'tool_call' }>;
    expect(card.kind).toBe('tool_call');
    expect(card.toolName).toBe('Bash');
    expect(card.result).not.toBeNull();
    expect(card.isError).toBe(false);
  });

  it('marks error results red', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't1' }),
      msg({ kind: 'tool_result', toolUseId: 't1', error: true }),
    ]);
    const card = items[0] as Extract<(typeof items)[number], { kind: 'tool_call' }>;
    expect(card.isError).toBe(true);
  });

  it('keeps unmatched tool_use as running (no result)', () => {
    const items = combineToolCalls([msg({ kind: 'tool_use', toolName: 'Read', toolUseId: 't2' })]);
    const card = items[0] as Extract<(typeof items)[number], { kind: 'tool_call' }>;
    expect(card.result).toBeNull();
  });

  it('passes through plain messages', () => {
    const items = combineToolCalls([msg({ kind: 'user', text: 'hi' })]);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe('message');
  });
});

describe('task list aggregation', () => {
  it('extractTaskList aggregates TaskCreate/TaskUpdate with evolving status', () => {
    const messages = [
      msg({ kind: 'assistant', text: '开始任务' }),
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'k1', input: { subject: '资料收集' } }),
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'k2', input: { subject: '写报告' } }),
      msg({ kind: 'tool_use', toolName: 'TaskUpdate', toolUseId: 'k3', input: { taskId: '1', status: 'in_progress' } }),
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 'k4', input: { command: 'ls' } }),
      msg({ kind: 'tool_use', toolName: 'TaskUpdate', toolUseId: 'k5', input: { taskId: '1', status: 'completed' } }),
    ];
    expect(extractTaskList(messages)).toEqual([
      { taskId: '1', subject: '资料收集', status: 'completed' },
      { taskId: '2', subject: '写报告', status: 'pending' },
    ]);
  });

  it('drops tasks whose TaskUpdate marks them deleted', () => {
    // 反馈轮 9：deleted = 任务已从 harness 移除，聚合剔除（即使此前已完成）
    const messages = [
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'k1', input: { subject: '被删除' } }),
      msg({ kind: 'tool_result', toolUseId: 'k1', text: 'Task #1 created successfully: 被删除' }),
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'k2', input: { subject: '保留' } }),
      msg({ kind: 'tool_result', toolUseId: 'k2', text: 'Task #2 created successfully: 保留' }),
      msg({ kind: 'tool_use', toolName: 'TaskUpdate', toolUseId: 'k3', input: { taskId: '1', status: 'completed' } }),
      msg({ kind: 'tool_use', toolName: 'TaskUpdate', toolUseId: 'k4', input: { taskId: '1', status: 'deleted' } }),
    ];
    expect(extractTaskList(messages)).toEqual([
      { taskId: '2', subject: '保留', status: 'pending' },
    ]);
  });

  it('keeps task list out of the message stream (fixed bar instead)', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'k1', input: { subject: '资料收集' } }),
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 'k4', input: { command: 'ls' } }),
    ]);
    // 消息流只剩 Bash 工具卡（任务进固定栏）
    expect(items.map((i) => i.kind)).toEqual(['tool_call']);
  });

  it('produces empty task list when no task tools present', () => {
    const messages = [msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 'x1', input: {} })];
    expect(extractTaskList(messages)).toEqual([]);
    expect(combineToolCalls(messages)).toHaveLength(1);
  });

  it('absorbs read-only Task calls (tool_use + result) when a task list is produced', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c1', input: { subject: '任务一' } }),
      msg({ kind: 'tool_result', toolUseId: 'c1' }),
      msg({ kind: 'tool_use', toolName: 'TaskList', toolUseId: 'L1' }),
      msg({ kind: 'tool_result', toolUseId: 'L1', text: '[pending] 任务一' }),
      msg({ kind: 'tool_use', toolName: 'TaskGet', toolUseId: 'G1', input: { taskId: '1' } }),
      msg({ kind: 'tool_result', toolUseId: 'G1', text: '任务一' }),
      msg({
        kind: 'tool_use',
        toolName: 'TaskUpdate',
        toolUseId: 'u1',
        input: { taskId: '1', status: 'completed' },
      }),
      msg({ kind: 'tool_result', toolUseId: 'u1' }),
    ]);
    // Task 系全部吸收，消息流零残留
    expect(items).toHaveLength(0);
  });

  it('dedupes re-created tasks with the same real taskId', () => {
    // compact/重放防御：同 realId 的 Create 合并进首个，不新增行
    const messages = [
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c1', input: { subject: 'P6 git 工作台' } }),
      msg({ kind: 'tool_result', toolUseId: 'c1', text: 'Task #28 created successfully: P6 git 工作台' }),
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c2', input: { subject: 'P6 git 工作台' } }),
      msg({ kind: 'tool_result', toolUseId: 'c2', text: 'Task #28 created successfully: P6 git 工作台' }),
    ];
    expect(extractTaskList(messages)).toHaveLength(1);
  });

  it('handles a8de32a1 real-shape fixture: global non-consecutive ids stay aligned', () => {
    // 真实形态（tester-r20 会话）：Create result 的 #N 为全局 id，非会话内连续序号
    const mk = (uid: string, n: number, subject: string): ChatMessage[] => [
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: uid, input: { subject } }),
      msg({ kind: 'tool_result', toolUseId: uid, text: `Task #${n} created successfully: ${subject}` }),
    ];
    const messages = [
      ...mk('a1', 5, '落 src/ 源码工程'),
      ...mk('a2', 28, 'P6 git 工作台 + P7 桌面打包（排队）'),
      ...mk('a3', 33, 'P5 preview r4 注记复测'),
      msg({ kind: 'tool_use', toolName: 'TaskUpdate', toolUseId: 'u1', input: { taskId: '5', status: 'completed' } }),
      msg({ kind: 'tool_use', toolName: 'TaskUpdate', toolUseId: 'u2', input: { taskId: '33', status: 'completed' } }),
    ];
    // #28 从未 Update → 保持 pending；#5/#33 精确关联 completed
    expect(extractTaskList(messages)).toEqual([
      { taskId: '5', subject: '落 src/ 源码工程', status: 'completed' },
      { taskId: '28', subject: 'P6 git 工作台 + P7 桌面打包（排队）', status: 'pending' },
      { taskId: '33', subject: 'P5 preview r4 注记复测', status: 'completed' },
    ]);
  });

  it('keeps TaskList as a normal tool card when no task card is produced', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'TaskList', toolUseId: 'L1' }),
      msg({ kind: 'tool_result', toolUseId: 'L1', text: '(empty)' }),
    ]);
    expect(items).toHaveLength(1);
    const card = items[0] as Extract<(typeof items)[number], { kind: 'tool_call' }>;
    expect(card.kind).toBe('tool_call');
    expect(card.toolName).toBe('TaskList');
    expect(card.result?.text).toBe('(empty)');
  });

  it('uses real harness taskId from TaskCreate result (globally incrementing ids)', () => {
    // review-ui-r2 MINOR 实测：harness taskId 全局递增（如 #33），非会话内从 1 起
    const messages = [
      msg({
        kind: 'tool_use',
        toolName: 'TaskCreate',
        toolUseId: 'c1',
        input: { subject: '非首个任务会话' },
      }),
      msg({ kind: 'tool_result', toolUseId: 'c1', result: { taskId: 33 } }),
      msg({
        kind: 'tool_use',
        toolName: 'TaskUpdate',
        toolUseId: 'u1',
        input: { taskId: '33', status: 'in_progress' },
      }),
    ];
    expect(extractTaskList(messages)).toEqual([
      { taskId: '33', subject: '非首个任务会话', status: 'in_progress' },
    ]);
  });

  it('extracts real taskId from result text when result is plain text', () => {
    const messages = [
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c1', input: { subject: 'A' } }),
      msg({ kind: 'tool_result', toolUseId: 'c1', text: 'Created task #34' }),
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c2', input: { subject: 'B' } }),
      msg({ kind: 'tool_result', toolUseId: 'c2' }),
      msg({
        kind: 'tool_use',
        toolName: 'TaskUpdate',
        toolUseId: 'u1',
        input: { taskId: '34', status: 'completed' },
      }),
    ];
    // c1 回填真实 id #34 并被 Update 命中；c2 无 result id → 位置回退 '2'
    expect(extractTaskList(messages)).toEqual([
      { taskId: '34', subject: 'A', status: 'completed' },
      { taskId: '2', subject: 'B', status: 'pending' },
    ]);
  });

  it('falls back to positional ids when results carry no taskId (legacy shape)', () => {
    const messages = [
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c1', input: { subject: 'X' } }),
      msg({ kind: 'tool_result', toolUseId: 'c1' }),
      msg({
        kind: 'tool_use',
        toolName: 'TaskUpdate',
        toolUseId: 'u1',
        input: { taskId: '1', status: 'in_progress' },
      }),
    ];
    expect(extractTaskList(messages)).toEqual([
      { taskId: '1', subject: 'X', status: 'in_progress' },
    ]);
  });

  it('ignores TaskUpdate for unknown taskIds without crashing or misfiring', () => {
    const messages = [
      msg({ kind: 'tool_use', toolName: 'TaskCreate', toolUseId: 'c1', input: { subject: 'X' } }),
      msg({ kind: 'tool_result', toolUseId: 'c1', result: { taskId: 33 } }),
      msg({
        kind: 'tool_use',
        toolName: 'TaskUpdate',
        toolUseId: 'u1',
        input: { taskId: '99', status: 'completed' },
      }),
    ];
    expect(extractTaskList(messages)).toEqual([
      { taskId: '33', subject: 'X', status: 'pending' },
    ]);
  });
});

describe('mergeAcpChunks（批2 任务8：omp 真实流式形态）', () => {
  // fixture 形态取自 r73 实机抓包：omp 逐词推 thinking/assistant chunk
  const acp = (kind: ChatMessage['kind'], text: string): ChatMessage =>
    msg({ kind, rawType: 'acp_chunk', text });

  it('相邻同 kind chunk 合并为一条流式增长消息', () => {
    const merged = mergeAcpChunks([
      acp('thinking', 'The'),
      acp('thinking', ' user'),
      acp('thinking', ' wants'),
      acp('assistant', 'he'),
      acp('assistant', 'llo'),
    ]);
    expect(merged).toHaveLength(2);
    expect(merged[0].kind).toBe('thinking');
    expect(merged[0].text).toBe('The user wants');
    expect(merged[1].kind).toBe('assistant');
    expect(merged[1].text).toBe('hello');
  });

  it('kind 切换（thinking→assistant）与中间插入消息打断合并', () => {
    const merged = mergeAcpChunks([
      acp('assistant', 'a'),
      acp('thinking', 'b'), // kind 不同不合
      msg({ kind: 'tool_use', rawType: 'acp_tool_call', toolUseId: 't1', toolName: 'Read' }),
      acp('assistant', 'c'), // 中间隔了非 chunk 消息不合
    ]);
    expect(merged).toHaveLength(4);
    expect(merged[0].text).toBe('a');
    expect(merged[1].text).toBe('b');
  });

  it('claude 消息（rawType 非 acp_chunk）不受合并影响', () => {
    const claudePairs = [
      msg({ kind: 'assistant', rawType: 'assistant', text: 'part1' }),
      msg({ kind: 'assistant', rawType: 'assistant', text: 'part2' }),
    ];
    expect(mergeAcpChunks(claudePairs)).toHaveLength(2);
  });

  it('combineToolCalls 内部走合并：ACP chunk 流渲染为单条', () => {
    const items = combineToolCalls([
      msg({ kind: 'user', rawType: 'acp_user', text: 'hi' }),
      acp('assistant', 'he'),
      acp('assistant', 'llo'),
    ]);
    expect(items).toHaveLength(2); // user + 合并后 assistant
  });
});

describe('extractAcpPlan（批2 任务10：plan → TaskRows）', () => {
  const planMsg = (entries: unknown): ChatMessage =>
    msg({ kind: 'other', rawType: 'acp_plan', result: { sessionUpdate: 'plan', entries } });

  it('取最后一条 plan（原地刷新），entries → TaskItem 顺序映射', () => {
    const messages = [
      planMsg([{ content: '旧任务', status: 'pending' }]),
      planMsg([
        { content: '探索代码', status: 'completed' },
        { content: '实现桥接', status: 'in_progress' },
        { content: '写测试', status: 'pending' },
      ]),
    ];
    expect(extractAcpPlan(messages)).toEqual([
      { taskId: '1', subject: '探索代码', status: 'completed' },
      { taskId: '2', subject: '实现桥接', status: 'in_progress' },
      { taskId: '3', subject: '写测试', status: 'pending' },
    ]);
  });

  it('无 plan → null；未知 status 归 pending；缺 content 兜底', () => {
    expect(extractAcpPlan([msg({ kind: 'assistant', text: 'x' })])).toBeNull();
    expect(extractAcpPlan([])).toBeNull();
    expect(extractAcpPlan([planMsg([{ status: 'weird' }, {}])])).toEqual([
      { taskId: '1', subject: '(untitled)', status: 'pending' },
      { taskId: '2', subject: '(untitled)', status: 'pending' },
    ]);
  });

  it('acp_plan 不进消息流（固定栏原地刷新，流内零噪音）', () => {
    const items = combineToolCalls([
      msg({ kind: 'user', rawType: 'acp_user', text: 'hi' }),
      planMsg([{ content: 'step', status: 'pending' }]),
    ]);
    expect(items).toHaveLength(1);
    expect((items[0] as { msg: ChatMessage }).msg.text).toBe('hi');
  });
});
