// 镜像 server/src/models.rs（camelCase）。避免魔法类型。

// 多实例（agent-hub-multi-instance）：镜像 server src/instances.rs（camelCase）
export type InstanceMode = 'direct' | 'ssh-tunnel' | 'node-link';
export type SshAuth = 'key_path' | 'authsock' | 'password';

export interface SshConfig {
  host: string;
  port: number;
  user: string;
  auth: SshAuth;
  keyPath: string | null;
  password: string | null;
}

export interface InstanceConfig {
  id: string;
  name: string;
  mode: InstanceMode;
  url: string | null;
  token: string | null;
  ssh: SshConfig | null;
  remotePort: number | null;
  localPort: number | null;
  /** node-link：节点元数据槽（gateway-node P1；非敏感） */
  nodeMeta: { platform: string; drivers: string[] } | null;
}

export type Group = 'needs_input' | 'working' | 'completed' | 'other';

// 节点管理（gateway-node-p3 M5）：镜像 server api/nodes.rs NodeView（camelCase）。
// stats 为 host presence（P3 任务 3）——旧版本节点不带 → null 如实缺失（N-1 窗口）。
export interface HostStats {
  os: string;
  arch: string;
  version: string;
  uptime_secs: number;
}

export type NodeStatus = 'pending' | 'approved' | 'rejected';

export interface NodeView {
  nodeId: string;
  nodeName: string;
  status: NodeStatus;
  platform: string;
  drivers: string[];
  createdAt: string;
  lastSeen: string | null;
  stats: HostStats | null;
}

export interface AgentSummary {
  driver: string;
  id: string;
  name: string | null;
  cwd: string | null;
  kind: string | null;
  rawState: string | null;
  group: Group;
  detail: string | null;
  tokens: number | null;
  /** 当前上下文占用（最后一条 assistant usage 的 input+cache 之和；仅 claude，其余 null） */
  context_tokens: number | null;
  startedAt: number | null;
  sessionId: string | null;
  /** 实例维度（multi-instance）：null = 本机；非空 = 远程实例 id */
  instanceId?: string | null;
}

export type ChatMessageKind =
  | 'user'
  | 'assistant'
  | 'tool_use'
  | 'tool_result'
  | 'thinking'
  | 'other';

export interface ChatMessage {
  kind: ChatMessageKind;
  rawType: string | null;
  text: string | null;
  toolUseId: string | null;
  toolName: string | null;
  input: unknown | null;
  result: unknown | null;
  error: boolean | null;
  ts: string | null;
}

export interface ChatEvent {
  sessionId: string;
  message: ChatMessage;
  seq: number;
}

/// subagent 会话条目（P5+，镜像 server SubagentEntry）。status 为 mtime 推断值。
export interface SubagentEntry {
  agentId: string;
  name: string;
  agentType: string;
  description: string | null;
  model: string | null;
  status: 'active' | 'completed';
  startedAt: string | null;
  lastActiveAt: string | null;
}

/// CLI 历史会话条目（agent-hub-session-history，镜像 server SessionListEntry）。
/// cwd 为会话发生时的工作目录：与所在 agent 的 cwd 不同即 worktree 会话。
export interface CliSessionEntry {
  sessionId: string;
  mtimeSecs: number;
  sizeBytes: number;
  branch: string | null;
  cwd: string | null;
  summary: string | null;
}

/// git 工程树节点（P6 B1，镜像 server GitTreeNode）。branch 为短分支名，detached 为 null。
export interface GitTreeNode {
  path: string;
  name: string;
  isMain: boolean;
  isGit: boolean;
  branch: string | null;
  head: string | null;
}

/// 一棵树：主仓 + 其 linked worktrees（无 worktree 时空数组，前端平铺）。
export interface GitTreeGroup {
  main: GitTreeNode;
  worktrees: GitTreeNode[];
}

/// git 节点变更文件（P6 B6，镜像 server StatusFile）。x=索引态 y=工作区态，
/// ' '=无；untracked 文件 x/y 均为 '?'。origPath 为重命名原路径。
/** git-timeline：单条提交（短 hash/作者/相对时间/subject） */
export interface GitCommit {
  hash: string;
  author: string;
  relTime: string;
  subject: string;
}

export interface GitLog {
  commits: GitCommit[];
  hasMore: boolean;
}

/// git-timeline-files：单条提交的文件维变更（镜像 server CommitFileDto）。
/// status 为 diff-tree 名字态（A/M/D/T/R/C；R/C 带相似度分数如 R100），
/// origPath 为改名/复制原路径（仅 R/C 条目）。
export interface GitCommitFile {
  status: string;
  path: string;
  origPath: string | null;
}

export interface GitCommitFiles {
  files: GitCommitFile[];
  hasMore: boolean;
}

export interface GitStatusFile {
  x: string;
  y: string;
  path: string;
  origPath: string | null;
}

/// git 节点状态（P6 B5，镜像 server GitStatusDto）
export interface GitStatus {
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  files: GitStatusFile[];
}

/// submodule 条目（P6 B7）。status：' ' 同步 / '-' 未初始化 / '+' SHA 漂移 / 'U' 冲突。
export interface SubmoduleInfo {
  status: string;
  sha: string;
  path: string;
  /** 工作区变更文件数（porcelain 非 ?? 行）；未初始化/探测失败 → 0 */
  changes: number;
  /** 未跟踪文件数（?? 行） */
  untracked: number;
}

/// diff 端点响应（agent-hub 热修）：untracked = no-index 全文添加；binary = 无文本 diff
export interface GitDiffResult {
  diff: string;
  untracked: boolean;
  binary: boolean;
}

/// 文档目录条目（P6 B8，镜像 server DocEntry）。kind：dir/markdown/html/text/binary。
export interface DocEntry {
  name: string;
  isDir: boolean;
  kind: string;
}

/// 文档预览内容（P6 B9，镜像 server DocFileDto）。binary/image/pdf 时 content 为 null
/// （image/pdf 走 /api/docs/raw 取原始字节）。
export interface DocFile {
  kind: string;
  name: string;
  content: string | null;
  /** 文件大小（字节；未知二进制空态展示用） */
  size: number;
}

// —— 定时任务（agent-hub-daemon-capabilities）：镜像 server tasks/mod.rs serde 形状 ——

export type TaskAction =
  | { action: 'spawn'; driver: string; cwd: string; name_template: string | null; prompt_template: string }
  | { action: 'inject'; driver: string; session_id: string; text: string };

export interface TaskSpec {
  id: string;
  name: string;
  cron: string;
  enabled: boolean;
  /** assistant-cron C4：来源标记——'assistant' = 助手经权限门创建；缺省 = hub UI 手动 */
  source?: string | null;
  /** 镜像后端：列表响应额外带下次触发（本地时区 RFC3339；禁用/非法 → null） */
  next_run_at?: string | null;
}

export type TaskSpecWithAction = TaskSpec & TaskAction;

export interface TaskRun {
  fired_at: string;
  status: 'running' | 'ok' | 'failed' | 'skipped' | 'missed';
  session_id?: string;
  duration_ms?: number;
  error?: string;
}
