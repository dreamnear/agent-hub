/**
 * @agent-hub/plugin-types — agent-hub 插件 manifest 类型（hub apiVersion 1）。
 *
 * 单源说明（技术设计 §13.5）：本包是**参考实现**（不发布 npm）；权威面 =
 * hub 的 `GET /api/plugins/api-schema`（贡献物类型 / 数据域 / RPC 方法表 /
 * 高危清单由 `api_surface.rs` 单点驱动）。本文件的类型与 B4（v0.9）面一致：
 * 含 B3 十类贡献物中的 renderers / themeTokens / fileDecorations / services（占位）
 * 与 B4 的 hooks 观察档。
 */

export const PLUGIN_API_VERSION = '1';

/** 观察档钩子名集（ADR-0014，B4 开放；at-most-once，只读） */
export type ObserverHook =
  | 'onSessionCreate'
  | 'onSessionClose'
  | 'onMessageAppend'
  | 'onIdle'
  | 'onJobsChanged';

/** 拦截档钩子名（ADR-0014 第 1 条：v1 不开——出现在 manifest 任何字段即审计失败） */
export type InterceptHook = 'beforeMessageSend' | 'onToolCall' | 'afterToolResult';

export interface ToolImpl {
  kind: 'backend' | 'mcp';
  ref: string;
}

export interface ToolDecl {
  id: string;
  name: string;
  description?: string;
  /** 参数 schema 文件相对路径（禁内联大块内容） */
  paramsSchema?: string;
  /** `read` 免确认；`high` 须与 permissions.highRisk 逐条一致 */
  risk: 'read' | 'high';
  impl: ToolImpl;
}

export interface SkillDecl {
  id: string;
  path?: string;
}

export interface McpDecl {
  id: string;
  transport: 'stdio';
  command: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
}

export interface ViewDecl {
  id: string;
  title: string;
  entry: string;
  icon?: string;
  order?: number;
}

export interface CommandTarget {
  /** 打开插件视图：二选一 */
  view?: string;
  /** 触发只读工具（risk:"read"）：二选一 */
  tool?: string;
  args?: unknown;
}

export interface CommandDecl {
  /** 强制 `plug_<pluginId>_<cid>` 命名空间 */
  id: string;
  name: string;
  description?: string;
  /** v1 仅 "info"（进 /api/commands 的 hub-plugin 组） */
  kind: 'info';
  target: CommandTarget;
}

export interface RendererMatch {
  /** 裸扩展名（不带点）；全 hub 唯一（含内置占用，冲突 = 审计失败） */
  ext?: string[];
  /** 围栏语言名小写；全 hub 唯一 */
  fence?: string[];
}

export interface RendererDecl {
  /** 强制 `plug_<pluginId>_<rid>` */
  id: string;
  match: RendererMatch;
  /** 宿主内置引擎（api-schema RENDER_ENGINES）或 "plugin-iframe" */
  engine: string;
  /** 服务类引擎（plantuml-server）仅允许 {"service":"<配置名>"}——禁内嵌 URL */
  config?: { service: string };
  /** engine:"plugin-iframe" 必填（相对路径，禁逃逸） */
  entry?: string;
}

export interface ThemeTokensDecl {
  /** token 名 ∈ 宿主 --c-* 白名单；亮暗成对；对比度硬校验（4.5:1） */
  tokens: Record<string, { light: string; dark: string }>;
}

export interface FileDecorationDecl {
  id: string;
  match: { ext: string[] };
  icon: string;
  /** 成对值或 "$token:--c-*" 引用；3:1 非文字对比度校验 */
  color?: { light: string; dark: string } | string;
}

/** 服务声明（ADR-0011 终审：占位不落地——类型保留，v1 无运行时） */
export interface ServiceDecl {
  id: string;
  label: string;
  placeholder?: string;
  default?: string;
}

export interface Contributes {
  tools?: ToolDecl[];
  skills?: SkillDecl[];
  mcp?: McpDecl[];
  views?: ViewDecl[];
  commands?: CommandDecl[];
  renderers?: RendererDecl[];
  themeTokens?: ThemeTokensDecl;
  fileDecorations?: FileDecorationDecl[];
  services?: ServiceDecl[];
  /** 观察档钩子逐钩子声明；未声明的通知不投递 */
  hooks?: ObserverHook[];
}

export interface Permissions {
  /** v1 细粒度预留（hub.http.fetch 未进 v1），声明被接受但不生效 */
  api?: string[];
  /** 固定域枚举见 api-schema dataDomains（含 hooks:observe / sessions:read / messages:read） */
  data?: string[];
  /** 凭据**名**（值由 hub 代持，manifest 禁携带值） */
  credentials?: string[];
  /** 高危项归 hub 维护（固定四项），risk:"high" 工具须逐条声明 */
  highRisk?: string[];
}

export interface BackendDecl {
  command: string;
  args?: string[];
  cwd?: string;
  idleTimeoutSecs?: number;
  alwaysOn?: boolean;
  /** `$cred:<name>` 占位由 hub 替换 */
  env?: Record<string, string>;
}

export interface Author {
  name: string;
  url?: string;
}

export interface PluginManifest {
  schemaVersion: 1;
  /** 2–40 字符 kebab-slug */
  id: string;
  name: string;
  /** 严格 MAJOR.MINOR.PATCH */
  version: string;
  description?: string;
  author?: Author;
  homepage?: string;
  hub: { apiVersion: string; minHubVersion?: string };
  contributes?: Contributes;
  permissions?: Permissions;
  backend?: BackendDecl;
}

/** 钩子下行 notification 帧（hub → backend，无 id 不等待） */
export interface HookNotification<H extends ObserverHook = ObserverHook> {
  jsonrpc: '2.0';
  method: `hook/${H}`;
  params: HookParams[H];
}

export interface HookParams {
  onSessionCreate: { sessionId: string; driver: string; cwd?: string; name?: string };
  onSessionClose: { sessionId: string; driver: string; reason?: string };
  /** 未声明 messages:read 时不含 kind/text（隐私边界） */
  onMessageAppend: { sessionId: string; seq: number; kind?: string; text?: string };
  onIdle: { idle: true; runningCount: number };
  onJobsChanged: { id: string };
}
