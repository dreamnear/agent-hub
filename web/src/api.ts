import type { AgentSummary, ChatMessage, DocEntry, DocFile, GitStatus, GitTreeGroup, InstanceConfig, SubagentEntry, SubmoduleInfo } from './types';
import { t } from './i18n';

/** 历史分页信封（P6 B12/B13）：firstLine 为向前翻页游标，hasMore 标识还有更早消息 */
export interface MessagePage {
  messages: ChatMessage[];
  firstLine: number;
  hasMore: boolean;
}

/** chat 分页配置（P6 B14）：与 /api/config/chat 的 camelCase 序列化逐字对齐（r39 教训） */
export interface ChatConfig {
  pageSize: number;
  bufferMax: number;
}

const TOKEN_KEY = 'hub_token';
// 持久化层（反馈轮 10）：localStorage 跨浏览器重启免输；隐私模式降级仅运行时
const PERSIST_KEY = 'agent_hub_token';

/// 401 全局回调（TokenGate 展示开关），App 初始化时注册。本机 instance 的 401 触发。
let unauthorizedCb: (() => void) | null = null;
export function onUnauthorized(cb: () => void): void {
  unauthorizedCb = cb;
}

/// 401 专用错误类型（任务10）：查询层用 instanceof 跳过重试——凭据失效非瞬态错误，
/// 重试只会白打请求。消息保持与原实现一致。
export class AuthError extends Error {
  constructor() {
    super('401 unauthorized');
    this.name = 'AuthError';
  }
}

// —— per-instance 凭据失效标记（任务10）——
// 远程实例 401 只标记该实例（侧栏「凭据失效」态），不影响本机 TokenGate 与其他实例；
// 该实例任何一次成功请求即解除（重配 token 后下一次轮询自动恢复）。
// 集合整体不可变替换（mark/clear 产生新引用），供 useSyncExternalStore 引用比较。

let authFailedSet: ReadonlySet<string> = new Set();
const authFailedListeners = new Set<() => void>();

function markAuthFailed(id: string): void {
  if (authFailedSet.has(id)) return;
  const next = new Set(authFailedSet);
  next.add(id);
  authFailedSet = next;
  authFailedListeners.forEach((l) => l());
}

function clearAuthFailed(id: string): void {
  if (!authFailedSet.has(id)) return;
  const next = new Set(authFailedSet);
  next.delete(id);
  authFailedSet = next;
  authFailedListeners.forEach((l) => l());
}

/** 订阅凭据失效集合变化（useSyncExternalStore 用）。返回退订函数。 */
export function subscribeAuthFailed(cb: () => void): () => void {
  authFailedListeners.add(cb);
  return () => {
    authFailedListeners.delete(cb);
  };
}

/** useSyncExternalStore 快照：凭据失效实例 id 集合。 */
export function authFailedSnapshot(): ReadonlySet<string> {
  return authFailedSet;
}

/// token 存储 key：远程实例 per-instance（`hub_token_<instanceId>`），本机沿用
/// `hub_token`（现状语义——TokenGate 只认本机）。
export function tokenStorageKeys(instanceId: string | null): { token: string; persist: string } {
  if (instanceId == null) return { token: TOKEN_KEY, persist: PERSIST_KEY };
  return { token: `hub_token_${instanceId}`, persist: `agent_hub_token_${instanceId}` };
}

/// 存 token：运行时（sessionStorage）+ 持久化（localStorage，不可用则降级不报错）
export function storeToken(t: string, instanceId: string | null = null): void {
  const { token, persist } = tokenStorageKeys(instanceId);
  sessionStorage.setItem(token, t);
  try {
    localStorage.setItem(persist, t);
  } catch {
    // 隐私模式等 localStorage 不可用：降级为每次输入
  }
}

/// 401 失效：两层全清（Gate 重输；防旧 token 自动重进死循环）
export function clearStoredToken(instanceId: string | null = null): void {
  const { token, persist } = tokenStorageKeys(instanceId);
  sessionStorage.removeItem(token);
  try {
    localStorage.removeItem(persist);
  } catch {
    // ignore
  }
}

/// 启动回填：URL ?token= 优先；否则从持久层回填运行时（无感进入，失效由
/// 401 → clearStoredToken 兜底）。回填命中返回 true（调用方可自动解锁）。
/// 仅作用于本机 instance（TokenGate 语义；远程实例 token 由实例配置/存储管）。
export function captureUrlToken(): boolean {
  const t = new URLSearchParams(location.search).get('token');
  if (t) {
    storeToken(t);
    history.replaceState(null, '', location.pathname);
    return true;
  }
  if (!sessionStorage.getItem(TOKEN_KEY)) {
    let saved: string | null = null;
    try {
      saved = localStorage.getItem(PERSIST_KEY);
    } catch {
      // 隐私模式：读不到就当无持久 token
    }
    if (saved) {
      sessionStorage.setItem(TOKEN_KEY, saved);
      return true;
    }
  }
  return false;
}

// 启动即回填（2026-09-23 回归修复）：必须早于任何网络请求——react-query 首查
// 先于 App 挂载 effect 执行，回填晚了首个请求就无 token → 401 → clearStoredToken
// 误清持久层 → 每次刷新/深链都要重输（r66/r67 遗留观察项确认为真问题）
captureUrlToken();

/// 无法带 header 的资源请求（img src 等）token 走 query（auth 中间件支持 ?token=
/// 校验）；本机免认证（无 token 返回空串）。仅本机资源用（远程图片走 instance base）。
export function resourceTokenQuery(): string {
  const t = sessionStorage.getItem(TOKEN_KEY);
  return t ? `?token=${encodeURIComponent(t)}` : '';
}

/// harness 发现项（agent-hub-settings 批A 需求5/6）：镜像 server src/harness.rs
/// （camelCase）。alive=false = 路径失效/不可执行（默认过滤项）。
export interface HarnessEntry {
  name: string;
  path: string;
  version: string | null;
  alive: boolean;
  kind: 'cli' | 'acp';
  configured: boolean;
}

/// 远程安装探测/计划/执行（agent-hub-settings D2–D5）：镜像 server src/remote_install.rs
/// （camelCase）。reason 四态可区分（已装/未装/端口被占/SSH 不可达）。
export type RemoteProbeReason = 'installed' | 'not_installed' | 'not_agent_hub' | 'ssh_unreachable';

export interface RemoteProbe {
  installed: boolean;
  os: string | null;
  arch: string | null;
  reason: RemoteProbeReason;
  detail: string;
}

export interface PlanStep {
  desc: string;
  /** 将执行的命令原文（确认面板逐条展示的就是它，所见即所执） */
  display: string;
}

export interface InstallPlan {
  planId: string;
  planHash: string;
  steps: PlanStep[];
}

export interface InstallRequest {
  planId: string;
  planHash: string;
  /** 红线：非 true 服务端拒绝执行任何命令 */
  confirm: boolean;
}

export interface InstallResult {
  ok: boolean;
  logs: string[];
  error: string | null;
  tokenStored: boolean;
}

export interface Api {
  /** 实例连接上下文：instanceId 用于 per-instance token 存储与 queryKey；baseUrl 为空=本机相对路径 */
  readonly instanceId: string | null;
  /** HTTP base url（不含尾斜杠）；'' = 本机相对路径 */
  readonly baseAppUrl: string;
  /** 多实例管理（本机 hub）：实例清单 CRUD —— 走该 hub 自身（相对路径）。
   * 本机实例的 makeApi('',null) 即可调用；远程实例的 api 不会用本方法。 */
  listInstances: () => Promise<InstanceConfig[]>;
  createInstance: (c: InstanceConfig) => Promise<InstanceConfig>;
  updateInstance: (c: InstanceConfig) => Promise<InstanceConfig>;
  deleteInstance: (id: string) => Promise<void>;
  instanceBaseUrl: (id: string) => Promise<{ baseUrl: string }>;
  tunnelStart: (id: string) => Promise<{ localPort: number }>;
  tunnelStop: (id: string) => Promise<{ stopped: boolean }>;
  tunnelStatus: (id: string) => Promise<{ running: boolean; localPort: number | null; state: string; retries: number }>;
  /** 远程自动安装（agent-hub-settings D2–D4）：探测只读；计划只生成不执行；执行需 confirm */
  remoteProbe: (id: string) => Promise<RemoteProbe>;
  installPlan: (id: string) => Promise<InstallPlan>;
  installExecute: (id: string, body: InstallRequest) => Promise<InstallResult>;
  /** direct 模式手动命令清单（与 ssh-tunnel 计划同一处定义，防漂移） */
  installManual: () => Promise<InstallPlan>;
  listAgents: (all: boolean) => Promise<AgentSummary[]>;
  listAcpAgents: () => Promise<{ agents: { name: string; command: string; args: string[]; cwd: string | null; model: string | null }[] }>;
  /** harness 发现（agent-hub-settings 批A）：本机探测清单 + 一键加入 ACP agent 配置 */
  listHarnesses: () => Promise<{ harnesses: HarnessEntry[] }>;
  addHarness: (name: string, path: string) => Promise<{ added: boolean; name: string }>;
  createAcpSession: (body: { agent: string; cwd: string; model?: string }) => Promise<{ sessionId: string; agent: string; cwd: string; model: string | null; status: string }>;
  acpMessages: (id: string) => Promise<MessagePage>;
  sendAcpPrompt: (id: string, text: string) => Promise<void>;
  cancelAcpSession: (id: string) => Promise<void>;
  answerAcpPermission: (id: string, permId: string, optionId: string | null) => Promise<void>;
  startAgent: (body: { cwd: string; prompt: string; name?: string; model?: string; effort?: string }) => Promise<{ id: string }>;
  stopAgent: (id: string) => Promise<void>;
  removeAgent: (id: string) => Promise<void>;
  logs: (id: string) => Promise<{ logs: string }>;
  messages: (id: string) => Promise<ChatMessage[]>;
  messagesPage: (id: string, before?: number) => Promise<MessagePage>;
  subagentMessagesPage: (id: string, subagentId: string, before?: number) => Promise<MessagePage>;
  chatConfig: () => Promise<ChatConfig>;
  subagents: (id: string) => Promise<SubagentEntry[]>;
  subagentMessages: (id: string, subagentId: string) => Promise<ChatMessage[]>;
  sessionActive: (id: string) => Promise<boolean>;
  agentTasks: (id: string) => Promise<{ taskId: string; subject: string; status: 'pending' | 'in_progress' | 'completed' }[] | null>;
  sendMessage: (id: string, text: string) => Promise<void>;
  respawnAgent: (id: string) => Promise<void>;
  interruptAgent: (id: string) => Promise<void>;
  uploadImage: (filename: string, dataBase64: string) => Promise<{ path: string }>;
  listCommands: () => Promise<{ name: string; source: string; description: string | null }[]>;
  listAgentConfigs: () => Promise<{ name: string; displayName: string | null; description: string | null; model: string | null; tools: string | null }[]>;
  getAgentConfig: (name: string) => Promise<{ name: string; displayName: string | null; description: string | null; model: string | null; tools: string | null; content: string }>;
  putAgentConfig: (name: string, content: string) => Promise<void>;
  listProjects: () => Promise<string[]>;
  projectTree: () => Promise<GitTreeGroup[]>;
  openDir: (path: string) => Promise<void>;
  gitStatus: (path: string) => Promise<GitStatus>;
  gitDiff: (path: string, file: string, cached?: boolean) => Promise<{ diff: string }>;
  createWorktree: (base: string, name: string) => Promise<{ path: string }>;
  submodules: (path: string) => Promise<SubmoduleInfo[]>;
  docsList: (path: string) => Promise<DocEntry[]>;
  docsFile: (path: string) => Promise<DocFile>;
  putProjects: (paths: string[]) => Promise<string[]>;
  getNote: (path: string) => Promise<{ content: string | null }>;
  putNote: (path: string, content: string) => Promise<{ content: string | null }>;
}

/** `base` 为 URL 前缀拼接函数：本机（instanceId=null）相对路径，远程实例=绝对 baseUrl */
function baseUrlFor(baseUrl: string, path: string): string {
  return baseUrl ? `${baseUrl}${path}` : path;
}

/// api 工厂（agent-hub-multi-instance 批1 任务4）：per-instance baseUrl + tokenKey。
/// 本机实例 = 现状相对路径 + hub_token（`api` 即由此构造，零破坏既有调用方）。
export function makeApi(baseUrl: string, instanceId: string | null): Api {
  const keys = tokenStorageKeys(instanceId);
  const tokenKey = keys.token;

  function authHeaders(): Record<string, string> {
    const t = sessionStorage.getItem(tokenKey);
    return t ? { authorization: `Bearer ${t}` } : {};
  }

  async function json<T>(input: string, init?: RequestInit): Promise<T> {
    const res = await fetch(baseUrlFor(baseUrl, input), {
      ...init,
      cache: 'no-store', // 任务10：禁 HTTP 缓存——同 URL 不同 token 的实例请求会被缓存串号（错 token 实例读到别实例的 200 响应，401 检测也被绕过）
      headers: { ...authHeaders(), ...(init?.headers ?? {}) },
    });
    if (res.status === 401) {
      // 本机实例 401 走全局 TokenGate（现状）；远程实例 401 只标记该实例
      // 凭据失效（任务10：侧栏「凭据失效」态，不触发本机 TokenGate）
      if (instanceId == null) unauthorizedCb?.();
      else markAuthFailed(instanceId);
      throw new AuthError();
    }
    if (res.status === 413) {
      const serverMsg = await res.text();
      throw new Error(serverMsg.trim() || t('api.payloadTooLarge'));
    }
    if (!res.ok) throw new Error(await res.text());
    if (instanceId != null) clearAuthFailed(instanceId);
    return res.json() as Promise<T>;
  }

  function post(input: string, body?: unknown): Promise<void> {
    return fetch(baseUrlFor(baseUrl, input), {
      method: 'POST',
      cache: 'no-store',
      headers: { ...authHeaders(), ...(body != null ? { 'Content-Type': 'application/json' } : {}) },
      ...(body != null ? { body: JSON.stringify(body) } : {}),
    }).then((r) => {
      if (r.status === 401) {
        if (instanceId == null) unauthorizedCb?.();
        else markAuthFailed(instanceId);
        throw new AuthError();
      }
      if (!r.ok) throw new Error(r.statusText);
      if (instanceId != null) clearAuthFailed(instanceId);
    });
  }

  /// DELETE：实例删除返回 204 空体，不解析 JSON
  function del(input: string): Promise<void> {
    return fetch(baseUrlFor(baseUrl, input), {
      method: 'DELETE',
      cache: 'no-store',
      headers: { ...authHeaders() },
    }).then((r) => {
      if (r.status === 401) {
        if (instanceId == null) unauthorizedCb?.();
        else markAuthFailed(instanceId);
        throw new AuthError();
      }
      if (!r.ok) throw new Error(r.statusText);
      if (instanceId != null) clearAuthFailed(instanceId);
    });
  }

  return {
    instanceId,
    baseAppUrl: baseUrl,
    listInstances: () => json<InstanceConfig[]>('/api/instances'),
    createInstance: (c) => json<InstanceConfig>('/api/instances', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(c),
    }),
    updateInstance: (c) => json<InstanceConfig>('/api/instances', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(c),
    }),
    deleteInstance: (id) => del(`/api/instances/${encodeURIComponent(id)}`),
    instanceBaseUrl: (id) => json<{ baseUrl: string }>(`/api/instances/${encodeURIComponent(id)}/base-url`),
    tunnelStart: (id) => json<{ localPort: number }>(`/api/instances/${encodeURIComponent(id)}/tunnel/start`, { method: 'POST' }),
    tunnelStop: (id) => json<{ stopped: boolean }>(`/api/instances/${encodeURIComponent(id)}/tunnel/stop`, { method: 'POST' }),
    tunnelStatus: (id) => json<{ running: boolean; localPort: number | null; state: string; retries: number }>(`/api/instances/${encodeURIComponent(id)}/tunnel/status`),
    remoteProbe: (id) => json<RemoteProbe>(`/api/instances/${encodeURIComponent(id)}/remote-probe`),
    installPlan: (id) => json<InstallPlan>(`/api/instances/${encodeURIComponent(id)}/install-plan`, { method: 'POST' }),
    installExecute: (id, body) => json<InstallResult>(`/api/instances/${encodeURIComponent(id)}/install`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }),
    installManual: () => json<InstallPlan>('/api/instances/install-manual'),
    listAgents: (all) => json<AgentSummary[]>(`/api/agents${all ? '?all=1' : ''}`),
    listAcpAgents: () => json('/api/acp/agents'),
    listHarnesses: () => json<{ harnesses: HarnessEntry[] }>('/api/harness'),
    addHarness: (name, path) =>
      json<{ added: boolean; name: string }>(`/api/harness/${encodeURIComponent(name)}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path }),
      }),
    createAcpSession: (body) => json('/api/acp/sessions', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }),
    acpMessages: (id) => json(`/api/acp/sessions/${encodeURIComponent(id)}/messages`),
    sendAcpPrompt: (id, text) => post(`/api/acp/sessions/${encodeURIComponent(id)}/prompt`, { text }),
    cancelAcpSession: (id) => post(`/api/acp/sessions/${encodeURIComponent(id)}/cancel`),
    answerAcpPermission: (id, permId, optionId) => post(`/api/acp/sessions/${encodeURIComponent(id)}/permission`, { permId, optionId }),
    startAgent: (body) => json('/api/agents/claude', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }),
    stopAgent: (id) => post(`/api/agents/claude/${id}/stop`),
    removeAgent: (id) => post(`/api/agents/claude/${id}/rm`),
    logs: (id) => json(`/api/agents/claude/${id}/logs`),
    messages: (id) => json(`/api/agents/claude/${id}/messages`),
    messagesPage: (id, before) =>
      json(`/api/agents/claude/${id}/messages/page${before != null ? `?before=${before}` : ''}`),
    subagentMessagesPage: (id, subagentId, before) =>
      json(`/api/agents/claude/${id}/subagents/${subagentId}/messages/page${before != null ? `?before=${before}` : ''}`),
    chatConfig: () => json('/api/config/chat'),
    subagents: (id) => json(`/api/agents/claude/${id}/subagents`),
    subagentMessages: (id, subagentId) => json(`/api/agents/claude/${id}/subagents/${subagentId}/messages`),
    sessionActive: (id) => json(`/api/agents/claude/${id}/session-active`),
    agentTasks: (id) => json(`/api/agents/claude/${id}/tasks`),
    sendMessage: (id, text) => post(`/api/agents/claude/${id}/message`, { text }),
    respawnAgent: (id) => post(`/api/agents/claude/${id}/respawn`),
    interruptAgent: (id) => post(`/api/agents/claude/${id}/interrupt`),
    uploadImage: (filename, dataBase64) => json('/api/upload', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ filename, data_base64: dataBase64 }),
    }),
    listCommands: () => json('/api/commands'),
    listAgentConfigs: () => json('/api/agents-config'),
    getAgentConfig: (name) => json(`/api/agents-config/${encodeURIComponent(name)}`),
    putAgentConfig: (name, content) => json(`/api/agents-config/${encodeURIComponent(name)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content }),
    }),
    listProjects: () => json('/api/projects'),
    projectTree: () => json('/api/projects/tree'),
    openDir: (path) => post('/api/projects/open-dir', { path }),
    gitStatus: (path) => json(`/api/projects/git-status?path=${encodeURIComponent(path)}`),
    gitDiff: (path, file, cached = false) =>
      json(`/api/projects/diff?path=${encodeURIComponent(path)}&file=${encodeURIComponent(file)}&cached=${cached}`),
    createWorktree: (base, name) => json('/api/projects/worktree', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ base, name }),
    }),
    submodules: (path) => json(`/api/projects/submodules?path=${encodeURIComponent(path)}`),
    docsList: (path) => json(`/api/docs/list?path=${encodeURIComponent(path)}`),
    docsFile: (path) => json(`/api/docs/file?path=${encodeURIComponent(path)}`),
    putProjects: (paths) => json('/api/projects', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ paths }),
    }),
    getNote: (path) => json(`/api/notes?path=${encodeURIComponent(path)}`),
    putNote: (path, content) => json('/api/notes', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path, content }),
    }),
  };
}

/// 本机实例 api（现状语义零破坏）：相对路径 + hub_token。
export const api = makeApi('', null);

/// 从 instance api 推导 WS base host（协议升 ws/wss）：''（本机）→ location.host。
export function wsHostForApi(apiLike: { baseAppUrl: string }): string {
  if (!apiLike.baseAppUrl) return `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}`;
  const proto = apiLike.baseAppUrl.startsWith('https:') ? 'wss:' : 'ws:';
  return apiLike.baseAppUrl.replace(/^https?:/, proto);
}

let chatCfgPromise: Promise<ChatConfig> | null = null;

/// chat 分页配置（P6 B14）：模块级记忆——启动取一次，重启才可能变化；失败回退默认
/// （本机实例口径）
export function chatConfig(): Promise<ChatConfig> {
  chatCfgPromise ??= api.chatConfig().catch(() => ({ pageSize: 20, bufferMax: 100 }));
  return chatCfgPromise;
}

/// WS 事件流订阅（任务4）：本机实例连相对 /ws/events（现状）；远程实例连
/// `${baseUrl}/ws/events` 并带 per-instance token。每实例一条独立连接、各自 5s 重连。
/// onState（任务10）：连通性变化回调——open→true（并触发一次 onEvent 失效重取，
/// 恢复后立刻刷数据）；close→false（离线信号源：HTTP 有缓存数据时查询不会重发，
/// WS 断开是实例死亡的第一手信号）。
export function openEventStream(
  onEvent: () => void,
  instanceId: string | null = null,
  baseWsBase?: string,
  onState?: (up: boolean) => void,
): () => void {
  let closed = false;
  let ws: WebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const keys = tokenStorageKeys(instanceId);

  function wsTokenQuery(): string {
    const t = sessionStorage.getItem(keys.token);
    return t ? `?token=${encodeURIComponent(t)}` : '';
  }

  function connect(): void {
    if (closed) return;
    let url: string;
    if (instanceId == null) {
      url = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws/events${wsTokenQuery()}`;
    } else {
      const base = baseWsBase ?? '';
      const proto = base.startsWith('https:') ? 'wss:' : 'ws:';
      // baseUrl（http://127.0.0.1:port 或 https://host:port）→ ws 同源升级
      const wsBase = base.replace(/^https?:/, proto);
      url = `${wsBase}/ws/events${wsTokenQuery()}`;
    }
    ws = new WebSocket(url);
    ws.onmessage = (): void => {
      onEvent();
    };
    ws.onopen = (): void => {
      onState?.(true);
      onEvent(); // 重连成功即失效重取——恢复后数据立刻刷新，不等下一条事件
    };
    ws.onclose = (): void => {
      onState?.(false);
      if (!closed) timer = setTimeout(connect, 5_000);
    };
    ws.onerror = (): void => {
      ws?.close();
    };
  }

  connect();
  return () => {
    closed = true;
    if (timer != null) clearTimeout(timer);
    ws?.close();
  };
}