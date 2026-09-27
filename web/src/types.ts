// 镜像 server/src/models.rs（camelCase）。避免魔法类型。

// 多实例（agent-hub-multi-instance）：镜像 server src/instances.rs（camelCase）
export type InstanceMode = 'direct' | 'ssh-tunnel';
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
}

export type Group = 'needs_input' | 'working' | 'completed' | 'other';

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
}

/// 文档目录条目（P6 B8，镜像 server DocEntry）。kind：dir/markdown/html/text/binary。
export interface DocEntry {
  name: string;
  isDir: boolean;
  kind: string;
}

/// 文档预览内容（P6 B9，镜像 server DocFileDto）。binary 时 content 为 null。
export interface DocFile {
  kind: string;
  name: string;
  content: string | null;
}
