/// 插件体系 B1 前端客户端（ADR-0002 第 1 条 / 技术设计 §5.1）。
///
/// 独立于 `api.ts` 的 per-instance 客户端：插件是 **hub 本地**概念（ADR-0009 单实例
/// 口径），不随实例切换，因此不复用实例路由；token 口径与 `api.ts` 一致
/// （sessionStorage `hub_token` → Bearer）。
import { AuthError } from './api';

const TOKEN_KEY = 'hub_token';

function authHeaders(): Record<string, string> {
  const t = sessionStorage.getItem(TOKEN_KEY);
  return t ? { authorization: `Bearer ${t}` } : {};
}

async function request<T>(input: string, init?: RequestInit): Promise<T> {
  const res = await fetch(input, {
    ...init,
    cache: 'no-store',
    headers: {
      ...authHeaders(),
      ...(init?.body != null ? { 'Content-Type': 'application/json' } : {}),
      ...(init?.headers ?? {}),
    },
  });
  if (res.status === 401) throw new AuthError();
  const text = await res.text();
  if (!res.ok) {
    // 服务端返回面向用户的文案（可定位错因），原样透出
    throw new Error(text || `HTTP ${res.status}`);
  }
  if (text === '') return undefined as T;
  return JSON.parse(text) as T;
}

// ===== 类型（与 server 侧 serde camelCase 逐字对齐）=====

export type PluginUiState = 'enabled' | 'disabled' | 'fault';

export interface PluginSource {
  kind: string;
  ref: string | null;
}

export interface Contribution {
  kind: 'tool' | 'skill' | 'mcp' | 'view';
  id: string;
  name: string;
  description?: string;
  risk: string;
  path?: string;
}

export interface CredentialStatus {
  name: string;
  configured: boolean;
}

export interface PermissionView {
  api: string[];
  data: string[];
  credentials: CredentialStatus[];
  highRisk: string[];
}

export interface ViewEntry {
  id: string;
  title: string;
  order?: number;
}

export interface BackendInfo {
  running: boolean;
  pid?: number;
  alwaysOn: boolean;
  idleTimeoutSecs: number;
}

export interface FaultRecord {
  at: string;
  kind: 'load_failure' | 'crash' | 'violation' | 'approval' | 'rejection';
  message: string;
}

export interface PluginView {
  id: string;
  name: string;
  version: string;
  description?: string;
  source: PluginSource;
  enabled: boolean;
  state: 'ready' | 'stopped' | 'fault';
  uiState: PluginUiState;
  installedAt: string;
  contentHash: string;
  contributions: Contribution[];
  permissions: PermissionView;
  views: ViewEntry[];
  backend: BackendInfo;
  lastFault?: FaultRecord;
  assetBytes: number;
}

export interface AuditIssue {
  field: string;
  rule: string;
  message: string;
  path?: string;
}

export interface AuditReport {
  pluginId: string;
  pluginDir: string;
  contentHash: string;
  contributions: Contribution[];
  reach: {
    api: string[];
    data: string[];
    credentials: CredentialStatus[];
    highRisk: { id: string; label: string }[];
    backend?: string[];
    mcp: string[][];
  };
  errors: AuditIssue[];
  warnings: AuditIssue[];
  assetBytes: number;
  assetFiles: number;
  installable: boolean;
}

export interface DiscoveredItem {
  id: string;
  dirName: string;
  path: string;
  sourceKind: string;
  status: 'new' | 'audit_failed' | 'update';
  installedContentHash?: string;
  report: AuditReport;
}

export interface RegressionCheck {
  name: string;
  passed: boolean;
  detail: string;
}

export interface RegressionReport {
  passed: boolean;
  checkedAt: string;
  checks: RegressionCheck[];
}

export interface UninstallReceipt {
  removed: boolean;
  id: string;
  dirKept: boolean;
  purgedData: boolean;
  regression: RegressionReport;
}

/// 侧栏导航条目（由已安装清单派生：仅启用/故障态且有视图贡献）。
export interface PluginNavItem {
  pluginId: string;
  pluginName: string;
  pluginVersion: string;
  viewId: string;
  title: string;
  order: number;
  fault: boolean;
  faultMessage?: string;
}

// ===== 客户端 =====

export const plugins = {
  list: (): Promise<PluginView[]> => request<PluginView[]>('/api/plugins'),

  discovered: (): Promise<{ items: DiscoveredItem[]; lastScanAt: string }> =>
    request('/api/plugins/discovered'),

  scan: (): Promise<{ lastScanAt: string; discovered: number }> =>
    request('/api/plugins/scan', { method: 'POST' }),

  audit: (target: { path?: string; id?: string }): Promise<AuditReport> =>
    request('/api/plugins/audit', { method: 'POST', body: JSON.stringify(target) }),

  install: (
    id: string,
    contentHash: string,
  ): Promise<{ plugin: PluginView; alreadyInstalled: boolean; effect: string }> =>
    request('/api/plugins/install', {
      method: 'POST',
      body: JSON.stringify({ id, contentHash }),
    }),

  enable: (id: string): Promise<{ plugin: PluginView; state: string; effect: string }> =>
    request(`/api/plugins/${encodeURIComponent(id)}/enable`, { method: 'POST' }),

  disable: (id: string): Promise<{ plugin: PluginView; state: string; effect: string }> =>
    request(`/api/plugins/${encodeURIComponent(id)}/disable`, { method: 'POST' }),

  retry: (id: string): Promise<{ plugin: PluginView; state: string; effect: string }> =>
    request(`/api/plugins/${encodeURIComponent(id)}/retry`, { method: 'POST' }),

  uninstall: (id: string, purgeData: boolean): Promise<UninstallReceipt> =>
    request(
      `/api/plugins/${encodeURIComponent(id)}?purgeData=${purgeData ? 'true' : 'false'}`,
      { method: 'DELETE' },
    ),

  records: (id: string, limit = 100): Promise<{ records: FaultRecord[] }> =>
    request(`/api/plugins/${encodeURIComponent(id)}/records?limit=${limit}`),

  viewTicket: (id: string): Promise<{ ticket: string; expiresInSecs: number }> =>
    request(`/api/plugins/${encodeURIComponent(id)}/view-ticket`, { method: 'POST' }),

  rpc: (
    id: string,
    method: string,
    params: unknown,
  ): Promise<{ ok: boolean; result?: unknown; code?: string; message?: string }> =>
    request(`/api/plugins/${encodeURIComponent(id)}/rpc`, {
      method: 'POST',
      body: JSON.stringify({ method, params }),
    }),

  apiSchema: (): Promise<{
    apiVersion: number;
    hubVersion: string;
    contributions: string[];
    dataDomains: string[];
    highRisk: { id: string; label: string }[];
    rpcMethods: { method: string; available: boolean; description: string }[];
    notes: string[];
  }> => request('/api/plugins/api-schema'),
};

/// 从已安装清单派生侧栏导航条目（BR-03：停用/卸载后入口消失；
/// 故障态保留入口 + 徽标，BR-07/08）。
export function navItemsOf(list: PluginView[]): PluginNavItem[] {
  const items: PluginNavItem[] = [];
  for (const p of list) {
    if (!p.enabled) continue;
    const fault = p.uiState === 'fault';
    for (const v of p.views) {
      items.push({
        pluginId: p.id,
        pluginName: p.name,
        pluginVersion: p.version,
        viewId: v.id,
        title: v.title,
        order: v.order ?? 1000,
        fault,
        faultMessage: fault ? p.lastFault?.message : undefined,
      });
    }
  }
  items.sort((a, b) => a.order - b.order || a.pluginId.localeCompare(b.pluginId));
  return items;
}

/// 视图资产 URL（票据门禁；票据 ≠ hub token，换不到任何 /api 能力）。
///
/// 票据走**路径段**形态 `/plugin-view/{id}/~<ticket>/<asset>`：静态 HTML 里的相对
/// 引用（app.js/style.css）由浏览器按文档 URL 目录解析时自动继承票据段——sandbox
/// iframe（opaque origin）下 SameSite cookie 不随发、query 无法被静态相对引用继承
/// （tester 1 轮 FAIL-1），路径段是唯一对浏览器透明的形态。
export function viewAssetUrl(pluginId: string, relPath: string, ticket: string): string {
  const clean = relPath.replace(/^\/+/, '') || 'index.html';
  return `/plugin-view/${encodeURIComponent(pluginId)}/~${encodeURIComponent(ticket)}/${clean}`;
}
