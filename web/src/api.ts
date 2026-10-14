import type { AgentSummary, ChatMessage, DocEntry, DocFile, GitCommitFiles, GitLog, GitStatus, GitTreeGroup, InstanceConfig, SubagentEntry, SubmoduleInfo, TaskSpecWithAction, TaskRun, GitDiffResult , NodeView } from './types';
import { t } from './i18n';

/** 历史分页信封（P6 B12/B13）：firstLine 为向前翻页游标，hasMore 标识还有更早消息 */
/// 归一化模型条目（后端 ModelInfo 镜像）
export interface ModelInfo {
  id: string;
  provider: string;
  name: string;
  free: boolean;
  variants: string[];
}

/// 会话外观能力（acp 等全 false → 只显示「由会话进程管理」）
export interface ModelCaps {
  canSet: boolean;
  canRead: boolean;
  supportsThinking: boolean;
}

/// 会话当前外观读数（null = 驱动未记录，如实显示）
export interface SessionModel {
  id: string;
  provider: string;
  variant: string | null;
}

export interface ModelSettings {
  caps: ModelCaps;
  current: SessionModel | null;
  models: ModelInfo[];
}

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

/** pi（Hub 助手）设置（agent-hub-pi-hub-agent P3/P10）：null = 未配置，用 pi 默认。
 * providerConfig = models.json 中 provider 名下条目投影（ak 掩码态，明文不出 GET）。 */
export interface PiSettingsCfg {
  provider: string | null;
  model: string | null;
  thinking: string | null;
  providerConfig: {
    baseUrl: string | null;
    api: string | null;
    apiKeyMasked: string | null;
    models: string[];
  } | null;
  /** P10 增量：允许助手注入会话（授权双闸之一，默认关） */
  allowSessionInject: boolean;
  /** P13：允许助手控制定时任务（启停/手动触发，默认关） */
  allowTaskControl: boolean;
  /** P11：助手工作目录（null = 默认 $HOME/agent-hub） */
  workDir: string | null;
}

/** P9：pi 版本探测读数（null = 未安装/不可执行，容错降级显示） */
export interface PiVersionCfg {
  version: string | null;
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

/// 轮换失败已回滚（sweep S1）：节点 persist 失败/回执超时，网关已恢复旧哈希
/// ——UI 用 instanceof 映射 i18n 文案「轮换失败已回滚，请重试」（不透传
/// 服务端中文文案，i18n zh/en 各自成立）。
export class RotateRolledBackError extends Error {
  constructor() {
    super('rotate rolled back');
    this.name = 'RotateRolledBackError';
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

/** authFailed 自愈探测（凭据失效恢复修复）：标记态实例的低频直连轻量 GET。
 * 走原生 fetch——绕开实例读队列/singleFlight（查询停摆时那条链路可能正是堵点）；
 * 200 即认为凭据已恢复 → clearAuthFailed（调用方随后 invalidate 恢复正常轮询）；
 * 401/其他错误/网络失败一律保持标记返回 false（不升级不报错，下个周期再探）。 */
export async function probeInstanceAuth(baseUrl: string, instanceId: string): Promise<boolean> {
  try {
    const t = sessionStorage.getItem(tokenStorageKeys(instanceId).token);
    const res = await fetch(`${baseUrl}/api/agents`, {
      headers: t ? { authorization: `Bearer ${t}` } : {},
      cache: 'no-store',
    });
    if (!res.ok) return false;
    clearAuthFailed(instanceId);
    return true;
  } catch {
    return false;
  }
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
  /** 定时任务（agent-hub-daemon-capabilities）：本机 hub 自身端点 */
  listTasks: () => Promise<TaskSpecWithAction[]>;
  /** schedule-lock S2：本实例是否持有调度权（standby 时调度禁用） */
  schedulerStatus: () => Promise<{ primary: boolean }>;
  /** 节点管理（gateway-node-p3 M5）：配对列表 + 生命周期操作（仅本机 hub 语义） */
  listNodes: () => Promise<{ pending: NodeView[]; approved: NodeView[] }>;
  approveNode: (nodeId: string) => Promise<void>;
  rejectNode: (nodeId: string) => Promise<void>;
  renameNode: (nodeId: string, name: string) => Promise<void>;
  rotateNodeToken: (nodeId: string) => Promise<void>;
  createTask: (t: TaskSpecWithAction) => Promise<TaskSpecWithAction>;
  updateTask: (id: string, t: TaskSpecWithAction) => Promise<TaskSpecWithAction>;
  deleteTask: (id: string) => Promise<void>;
  runTask: (id: string) => Promise<void>;
  taskRuns: (id: string) => Promise<TaskRun[]>;
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
  listAgents: (all: boolean, init?: RequestInit) => Promise<AgentSummary[]>;
  listAcpAgents: () => Promise<{ agents: { name: string; command: string; args: string[]; cwd: string | null; model: string | null }[] }>;
  /** harness 发现（agent-hub-settings 批A）：本机探测清单 + 一键加入 ACP agent 配置 */
  listHarnesses: () => Promise<{ harnesses: HarnessEntry[] }>;
  addHarness: (name: string, path: string) => Promise<{ added: boolean; name: string }>;
  createAcpSession: (body: { agent: string; cwd: string; model?: string }) => Promise<{ sessionId: string; agent: string; cwd: string; model: string | null; status: string }>;
  acpMessages: (id: string) => Promise<MessagePage>;
  sendAcpPrompt: (id: string, text: string) => Promise<void>;
  cancelAcpSession: (id: string) => Promise<void>;
  answerAcpPermission: (id: string, permId: string, optionId: string | null) => Promise<void>;
  answerPermission: (id: string, permId: string, optionId: string | null, driver?: string) => Promise<void>;
  answerQuestion: (id: string, eventId: string, answers: unknown, driver?: string) => Promise<void>;
  startAgent: (body: { cwd: string; prompt: string; name?: string; model?: string; effort?: string }, driver?: string) => Promise<{ id: string }>;
  stopAgent: (id: string, driver?: string) => Promise<void>;
  removeAgent: (id: string, driver?: string) => Promise<void>;
  /** R8：interactive 前台会话从列表移除（持久忽略，不动进程） */
  unlistAgent: (id: string, driver: string, sessionId: string | null) => Promise<void>;
  logs: (id: string, driver?: string) => Promise<{ logs: string }>;
  messages: (id: string) => Promise<ChatMessage[]>;
  messagesPage: (id: string, before?: number, driver?: string) => Promise<MessagePage>;
  /** 会话外观（agent-hub-model-thinking）：读当前模型/思考 + 能力 + 清单 */
  getModelSettings: (id: string, driver?: string) => Promise<ModelSettings>;
  /** 设置会话模型/思考；message = 驱动收场文案（已切换/已排队） */
  setModelSettings: (id: string, body: { model: string; variant?: string }, driver?: string) => Promise<{ message: string }>;
  subagentMessagesPage: (id: string, subagentId: string, before?: number, driver?: string) => Promise<MessagePage>;
  chatConfig: () => Promise<ChatConfig>;
  /** pi（Hub 助手）provider/model 设置（P3）：读 / 写（写后助手重启生效） */
  getPiSettings: () => Promise<PiSettingsCfg>;
  /** P9：pi 版本（外部 CLI 探测，TTL 缓存） */
  getPiVersion: () => Promise<PiVersionCfg>;
  putPiSettings: (body: {
    provider?: string;
    model?: string;
    thinking?: string;
    allowSessionInject?: boolean;
    allowTaskControl?: boolean;
    workDir?: string;
    baseUrl?: string;
    api?: string;
    apiKey?: string;
    models?: string[];
  }) => Promise<PiSettingsCfg>;
  /** assistant-resume A5：助手手动新建会话（发首条消息后成为下次打开的默认会话） */
  newPiSession: () => Promise<{ status: string }>;
  /** restart-feedback：最近一次助手真实 spawn 的终态（null = 尚无记录） */
  getPiRestartStatus: () => Promise<{ lastResult: { ok: boolean; error: string | null; atMs: number } | null }>;
  subagents: (id: string, finished?: boolean, driver?: string) => Promise<SubagentEntry[]>;
  subagentMessages: (id: string, subagentId: string, driver?: string) => Promise<ChatMessage[]>;
  sessionActive: (id: string, driver?: string) => Promise<boolean>;
  agentTasks: (id: string, driver?: string) => Promise<{ taskId: string; subject: string; status: 'pending' | 'in_progress' | 'completed' }[] | null>;
  sendMessage: (id: string, text: string, driver?: string) => Promise<void>;
  respawnAgent: (id: string, driver?: string) => Promise<void>;
  interruptAgent: (id: string, driver?: string) => Promise<void>;
  uploadImage: (filename: string, dataBase64: string) => Promise<{ path: string }>;
  /** / 补全候选（agent-hub-slash-skills）：cwd 提供时追加项目级命令/技能（S2）；
   * 各实例 server 各自枚举本机文件系统，老版本实例忽略 cwd 自动降级内置集 */
  listCommands: (cwd?: string) => Promise<{ name: string; source: string; description: string | null }[]>;
  listAgentConfigs: () => Promise<{ name: string; displayName: string | null; description: string | null; model: string | null; tools: string | null }[]>;
  getAgentConfig: (name: string) => Promise<{ name: string; displayName: string | null; description: string | null; model: string | null; tools: string | null; content: string }>;
  putAgentConfig: (name: string, content: string) => Promise<void>;
  listProjects: () => Promise<string[]>;
  listDrivers: () => Promise<string[]>;
  getDriverStatus: () => Promise<Record<string, { authError: boolean }>>;
  projectTree: () => Promise<GitTreeGroup[]>;
  openDir: (path: string) => Promise<void>;
  gitStatus: (path: string) => Promise<GitStatus>;
  gitDiff: (path: string, file: string, cached?: boolean) => Promise<GitDiffResult>;
  createWorktree: (base: string, name: string) => Promise<{ path: string }>;
  submodules: (path: string) => Promise<SubmoduleInfo[]>;
  /** git-timeline：当前分支提交历史（只读 git log；limit 初始 50，skip 加载更多） */
  gitLog: (path: string, limit?: number, skip?: number) => Promise<GitLog>;
  /** git-timeline-files：单条提交的文件维变更清单（只读 diff-tree --name-status；
   * limit 缺省 200，hasMore 由「加载更多」递增重取） */
  commitFiles: (path: string, hash: string, limit?: number) => Promise<GitCommitFiles>;
  /** git-timeline-files：单条提交内单文件的只读 diff（diff-tree -p） */
  commitDiff: (path: string, hash: string, file: string) => Promise<GitDiffResult>;
  docsList: (path: string) => Promise<DocEntry[]>;
  docsFile: (path: string) => Promise<DocFile>;
  /** 纯工具终端 WS 地址（agent-hub-terminal）：cwd 参数化，token 走 query
   * （WS 无法自定义 header，与 docsRawUrl 同机制）；本机/远程实例按 wsEndpoint 三态 */
  terminalWsUrl: (cwd: string) => string;
  /** 图片/PDF 原始字节 URL（img/iframe 无法带 Authorization 头，token 走 query，
   * 与 resourceTokenQuery 同机制；本机实例口径，与 docsList/docsFile 一致） */
  docsRawUrl: (path: string) => string;
  putProjects: (paths: string[]) => Promise<string[]>;
  getNote: (path: string) => Promise<{ content: string | null }>;
  putNote: (path: string, content: string) => Promise<{ content: string | null }>;
}

/** `base` 为 URL 前缀拼接函数：本机（instanceId=null）相对路径，远程实例=绝对 baseUrl */
function baseUrlFor(baseUrl: string, path: string): string {
  return baseUrl ? `${baseUrl}${path}` : path;
}

/** 远端实例请求 deadline（消息/发送链路与 agents 层同口径共享，10s 超时按 AbortError
 * 降级——与 fetchAgentsWithDeadline 同机制同文案）；本机不设限。 */
export const REMOTE_TIMEOUT_MS = 10_000;

/// api 工厂（agent-hub-multi-instance 批1 任务4）：per-instance baseUrl + tokenKey。
/// 本机实例 = 现状相对路径 + hub_token（`api` 即由此构造，零破坏既有调用方）。
export function makeApi(baseUrl: string, instanceId: string | null): Api {
  const keys = tokenStorageKeys(instanceId);
  const tokenKey = keys.token;

  // —— 每实例通道隔离（消息/发送链路补口，H253 后续）——
  // 远端实例：GET 读请求按实例串行（在途合一个——慢实例不再吃满浏览器同源 6 连接，
  // 排队 = 各请求按序等待，不同实例互不阻塞）；写请求（POST/PUT/DELETE）不排队
  // （用户动作必须直达）但同样挂 10s deadline。本机（instanceId=null）一切路径零变化。
  // ponytail: 读队列无上限，最坏以 1 请求/10s 速率排空（超时兜底）；若实测排队长
  // 再加丢弃策略。
  let readChain: Promise<unknown> = Promise.resolve();
  function enqueueRead<T>(task: () => Promise<T>): Promise<T> {
    const turn = readChain.then(task, task); // 前序落定（无论成败）即放行下一个
    readChain = turn.catch(() => undefined); // 链条对错误免疫，继续接后续请求
    return turn;
  }

  /** 远端 10s deadline；调用方已带 signal（agents 层自带 deadline）则沿用不叠加；
   * 本机无 signal 无 timer。done() 在请求落定后清 timer。 */
  function deadlineOf(init: RequestInit | undefined): {
    signal: AbortSignal | undefined;
    done: () => void;
  } {
    if (init?.signal) return { signal: init.signal, done: () => {} };
    if (instanceId == null) return { signal: undefined, done: () => {} };
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), REMOTE_TIMEOUT_MS);
    return { signal: ac.signal, done: () => clearTimeout(timer) };
  }

  function authHeaders(): Record<string, string> {
    const t = sessionStorage.getItem(tokenKey);
    return t ? { authorization: `Bearer ${t}` } : {};
  }

  async function fetchJson<T>(
    input: string,
    init: RequestInit | undefined,
    signal: AbortSignal | undefined,
  ): Promise<T> {
    const res = await fetch(baseUrlFor(baseUrl, input), {
      ...init,
      ...(signal ? { signal } : {}),
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

  async function json<T>(input: string, init?: RequestInit): Promise<T> {
    const isRead = init?.method == null || init.method === 'GET';
    const { signal, done } = deadlineOf(init);
    const exec = (): Promise<T> => fetchJson<T>(input, init, signal).finally(done);
    // 仅远端读请求入实例队列；本机一切请求与远端写请求直达
    if (instanceId != null && isRead) return enqueueRead(exec);
    return exec();
  }

  function post(input: string, body?: unknown): Promise<void> {
    // 写请求不排队（用户动作直达），远端挂 10s deadline
    const { signal, done } = deadlineOf(undefined);
    return fetch(baseUrlFor(baseUrl, input), {
      method: 'POST',
      ...(signal ? { signal } : {}),
      cache: 'no-store',
      headers: { ...authHeaders(), ...(body != null ? { 'Content-Type': 'application/json' } : {}) },
      ...(body != null ? { body: JSON.stringify(body) } : {}),
    })
      .finally(done)
      .then(async (r) => {
      if (r.status === 401) {
        if (instanceId == null) unauthorizedCb?.();
        else markAuthFailed(instanceId);
        throw new AuthError();
      }
      if (!r.ok) {
        // 错误透传响应 body（server 返回面向用户文案，如 pi 的
        // 「No API key found …」含 /login 指引）——状态文案会丢关键信息
        // （tester 走查 FAIL-2：只显示 "Send failed: Bad Request"）
        const detail = await r.text().catch(() => '');
        throw new Error(detail.trim() || r.statusText);
      }
      if (instanceId != null) clearAuthFailed(instanceId);
    });
  }

  /// DELETE：实例删除返回 204 空体，不解析 JSON（远端同样挂 deadline）
  function del(input: string): Promise<void> {
    const { signal, done } = deadlineOf(undefined);
    return fetch(baseUrlFor(baseUrl, input), {
      method: 'DELETE',
      ...(signal ? { signal } : {}),
      cache: 'no-store',
      headers: { ...authHeaders() },
    })
      .finally(done)
      .then(async (r) => {
      if (r.status === 401) {
        if (instanceId == null) unauthorizedCb?.();
        else markAuthFailed(instanceId);
        throw new AuthError();
      }
      if (!r.ok) {
        // 错误透传响应 body（server 返回面向用户文案，如 pi 的
        // 「No API key found …」含 /login 指引）——状态文案会丢关键信息
        // （tester 走查 FAIL-2：只显示 "Send failed: Bad Request"）
        const detail = await r.text().catch(() => '');
        throw new Error(detail.trim() || r.statusText);
      }
      if (instanceId != null) clearAuthFailed(instanceId);
    });
  }

  return {
    instanceId,
    baseAppUrl: baseUrl,
    listInstances: () => json<InstanceConfig[]>('/api/instances'),
    listTasks: () => json<TaskSpecWithAction[]>('/api/tasks'),
    schedulerStatus: () => json<{ primary: boolean }>('/api/tasks/scheduler-status'),
    listNodes: () =>
      json<{ pending: NodeView[]; approved: NodeView[] }>('/api/nodes'),
    approveNode: (id: string) =>
      json<unknown>(`/api/nodes/${encodeURIComponent(id)}/approve`, { method: 'POST' }).then(() => undefined),
    rejectNode: (id: string) =>
      json<unknown>(`/api/nodes/${encodeURIComponent(id)}/reject`, { method: 'POST' }).then(() => undefined),
    renameNode: (id: string, name: string) =>
      json<unknown>(`/api/nodes/${encodeURIComponent(id)}/rename`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      }).then(() => undefined),
    rotateNodeToken: (id: string) =>
      json<unknown>(`/api/nodes/${encodeURIComponent(id)}/rotate`, { method: 'POST' }).then(
        () => undefined,
        (e: unknown) => {
          // sweep S1：网关回滚错误体 {"error":"rotate_rolled_back"} → 专用类型
          //（其余错误原样上抛，沿用既有透传语义）
          const msg = e instanceof Error ? e.message : '';
          let code = '';
          try {
            code = (JSON.parse(msg) as { error?: string }).error ?? '';
          } catch {
            // 非 JSON body（代理/网关错误页等）——原样上抛
          }
          if (code === 'rotate_rolled_back') throw new RotateRolledBackError();
          throw e;
        },
      ),
    createTask: (t) => json<TaskSpecWithAction>('/api/tasks', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(t),
    }),
    updateTask: (id, t) => json<TaskSpecWithAction>(`/api/tasks/${encodeURIComponent(id)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(t),
    }),
    deleteTask: (id) =>
      json<unknown>(`/api/tasks/${encodeURIComponent(id)}`, { method: 'DELETE' }).then(() => undefined),
    runTask: (id) =>
      json<unknown>(`/api/tasks/${encodeURIComponent(id)}/run`, { method: 'POST' }).then(() => undefined),
    taskRuns: (id) => json<TaskRun[]>(`/api/tasks/${encodeURIComponent(id)}/runs`),
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
    listAgents: (all, init) => json<AgentSummary[]>(`/api/agents${all ? '?all=1' : ''}`, init),
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
    answerPermission: (id: string, permId: string, optionId: string | null, driver = 'claude') =>
      post(`/api/agents/${encodeURIComponent(driver)}/${encodeURIComponent(id)}/permission`, { permId, optionId }),
    answerQuestion: (id: string, eventId: string, answers: unknown, driver = 'claude') =>
      post(`/api/agents/${encodeURIComponent(driver)}/${encodeURIComponent(id)}/question`, { eventId, answers }),
    startAgent: (body, driver = 'claude') => json(`/api/agents/${encodeURIComponent(driver)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }),
    stopAgent: (id, driver = 'claude') => post(`/api/agents/${encodeURIComponent(driver)}/${id}/stop`),
    removeAgent: (id, driver = 'claude') => post(`/api/agents/${encodeURIComponent(driver)}/${id}/rm`),
    unlistAgent: (id, driver, sessionId) =>
      post(`/api/agents/${encodeURIComponent(driver)}/${encodeURIComponent(id)}/ignore`, { sessionId }),
    logs: (id, driver = 'claude') => json(`/api/agents/${encodeURIComponent(driver)}/${id}/logs`),
    // 无调用方（历史读已被 messagesPage 取代）；留旧路径，若有新调用方再参数化
    messages: (id) => json(`/api/agents/claude/${id}/messages`),
    messagesPage: (id, before, driver = 'claude') =>
      json(`/api/agents/${driver}/${id}/messages/page${before != null ? `?before=${before}` : ''}`),
    getModelSettings: (id, driver = 'claude') =>
      json(`/api/agents/${driver}/${id}/model-settings`),
    setModelSettings: (id, body, driver = 'claude') =>
      json(`/api/agents/${driver}/${id}/model-settings`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }),
    subagentMessagesPage: (id, subagentId, before, driver = 'claude') =>
      json(`/api/agents/${encodeURIComponent(driver)}/${id}/subagents/${subagentId}/messages/page${before != null ? `?before=${before}` : ''}`),
    chatConfig: () => json('/api/config/chat'),
    getPiSettings: () => json('/api/pi/settings'),
    getPiVersion: () => json('/api/pi/version'),
    putPiSettings: (body) =>
      json('/api/pi/settings', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }),
    subagents: (id, finished = false, driver = 'claude') => json(`/api/agents/${encodeURIComponent(driver)}/${id}/subagents${finished ? '?finished=1' : ''}`),
    newPiSession: () =>
      json('/api/pi/new-session', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({}),
      }),
    getPiRestartStatus: () => json('/api/pi/restart-status'),
    subagentMessages: (id, subagentId, driver = 'claude') => json(`/api/agents/${encodeURIComponent(driver)}/${id}/subagents/${subagentId}/messages`),
    sessionActive: (id, driver = 'claude') => json(`/api/agents/${encodeURIComponent(driver)}/${id}/session-active`),
    agentTasks: (id, driver = 'claude') => json(`/api/agents/${encodeURIComponent(driver)}/${id}/tasks`),
    sendMessage: (id, text, driver = 'claude') => post(`/api/agents/${driver}/${id}/message`, { text }),
    respawnAgent: (id, driver = 'claude') => post(`/api/agents/${encodeURIComponent(driver)}/${id}/respawn`),
    interruptAgent: (id, driver = 'claude') => post(`/api/agents/${driver}/${id}/interrupt`),
    uploadImage: (filename, dataBase64) => json('/api/upload', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ filename, data_base64: dataBase64 }),
    }),
    listCommands: (cwd) => json(`/api/commands${cwd ? `?cwd=${encodeURIComponent(cwd)}` : ''}`),
    listAgentConfigs: () => json('/api/agents-config'),
    getAgentConfig: (name) => json(`/api/agents-config/${encodeURIComponent(name)}`),
    putAgentConfig: (name, content) => json(`/api/agents-config/${encodeURIComponent(name)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content }),
    }),
    listProjects: () => json('/api/projects'),
    listDrivers: () => json('/api/drivers'),
    getDriverStatus: () => json<Record<string, { authError: boolean }>>('/api/drivers/status'),
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
    gitLog: (path, limit = 50, skip = 0) =>
      json(`/api/projects/git-log?path=${encodeURIComponent(path)}&limit=${limit}&skip=${skip}`),
    commitFiles: (path, hash, limit = 200) =>
      json(`/api/projects/git-commit-files?path=${encodeURIComponent(path)}&hash=${encodeURIComponent(hash)}&limit=${limit}`),
    commitDiff: (path, hash, file) =>
      json(`/api/projects/git-commit-diff?path=${encodeURIComponent(path)}&hash=${encodeURIComponent(hash)}&file=${encodeURIComponent(file)}`),
    docsList: (path) => json(`/api/docs/list?path=${encodeURIComponent(path)}`),
    docsFile: (path) => json(`/api/docs/file?path=${encodeURIComponent(path)}`),
    terminalWsUrl: (cwd) => {
      const { host, token } = wsEndpoint(baseUrl, instanceId);
      const q = new URLSearchParams({ cwd, ...(token ? { token } : {}) });
      return `${host}/ws/terminal?${q.toString()}`;
    },
    docsRawUrl: (path) => {
      const t = sessionStorage.getItem(TOKEN_KEY);
      return `/api/docs/raw?path=${encodeURIComponent(path)}${t ? `&token=${encodeURIComponent(t)}` : ''}`;
    },
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

/// WS 连接上下文（设备隔离修复）：返回「host 前缀 + query token」三态——
/// - 本机（''）与 ssh-tunnel 同源代理（'/api/instances/{id}/p'，相对路径）：
///   同源 host（协议升 ws/wss）+ hub token（代理上游的实例 token 由 server
///   按实例配置注入，浏览器无需预存 per-instance token——手机/LAN 设备关键）；
/// - direct 远程：baseUrl 协议升 ws/wss 直连 + per-instance token（语义不变）。
/// url = `${host}/ws/...`。
export function wsEndpoint(
  baseUrl: string,
  instanceId: string | null,
): { host: string; token: string | null } {
  const wsProto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  if (!baseUrl || baseUrl.startsWith('/')) {
    return {
      host: `${wsProto}//${location.host}${baseUrl}`,
      token: sessionStorage.getItem('hub_token'),
    };
  }
  const proto = baseUrl.startsWith('https:') ? 'wss:' : 'ws:';
  return {
    host: baseUrl.replace(/^https?:/, proto),
    token: instanceId == null ? null : sessionStorage.getItem(`hub_token_${instanceId}`),
  };
}

let chatCfgPromise: Promise<ChatConfig> | null = null;

/// chat 分页配置（P6 B14）：模块级记忆——启动取一次，重启才可能变化；失败回退默认
/// （本机实例口径）
export function chatConfig(): Promise<ChatConfig> {
  chatCfgPromise ??= api.chatConfig().catch(() => ({ pageSize: 20, bufferMax: 100 }));
  return chatCfgPromise;
}

/// WS 事件流订阅（任务4）：本机实例连相对 /ws/events（现状）；ssh-tunnel 实例经
/// 同源代理前缀 + hub token（设备隔离修复）；direct 远程连 `${baseUrl}/ws/events`
/// 带 per-instance token。每实例一条独立连接、各自 5s 重连。
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

  function connect(): void {
    if (closed) return;
    // 设备隔离修复：ssh-tunnel 实例走同源代理前缀（ctx.baseUrl=/api/instances/{id}/p），
    // 浏览器带 hub token 过 hub 认证；direct 仍直连远端 + per-instance token
    const { host, token } = wsEndpoint(baseWsBase ?? '', instanceId);
    const q = token ? `?token=${encodeURIComponent(token)}` : '';
    ws = new WebSocket(`${host}/ws/events${q}`);
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