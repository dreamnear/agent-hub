#!/usr/bin/env node
//! gitlab-issues-lite backend（ADR-0006：每插件一进程 sidecar，stdio 换行分隔 JSON-RPC 2.0）。
//!
//! 纪律（本文件即插件侧代码边界）：
//! - 一切 hub 访问经 stdio RPC（`hub.credentials.use` / `hub.projects.list`），
//!   **零直调 hub `/api/*`**（hub token 也不下发到本进程）；
//! - 凭据按名引用（gitlab_pat），值只在 RPC 应答载荷出现，不落盘、不打日志（零回显）；
//! - 本进程自行出站访问 GitLab（ADR-0006 拍板结论 5：v1 不要求出站经 hub）。
//!
//! 支持方法：initialize / tool.invoke(fetch_issues) / health；通知 shutdown 后退出。
//! 环境变量：GITLAB_API_BASE（缺省 https://gitlab.com/api/v4，E2E 可指本地 mock）、
//!           GITLAB_PROJECT（缺省取 hub 工程清单第一项的目录名）。

'use strict';

const readline = require('node:readline');

const API_BASE = (process.env.GITLAB_API_BASE || 'https://gitlab.com/api/v4').replace(/\/+$/, '');

let nextId = 1;
const pending = new Map(); // id -> resolve

function send(payload) {
  process.stdout.write(`${JSON.stringify(payload)}\n`);
}

/** backend → hub 请求（凭据/数据都经 hub 代执行，本进程无任何 hub 凭据）。 */
function callHub(method, params, timeoutMs = 15000) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`hub rpc timeout: ${method}`));
    }, timeoutMs);
    pending.set(id, (result, error) => {
      clearTimeout(timer);
      if (error) reject(new Error(`${error.message} (code ${error.code})`));
      else resolve(result);
    });
    send({ jsonrpc: '2.0', id, method, params });
  });
}

async function getGitlabPat() {
  const res = await callHub('hub.credentials.use', { name: 'gitlab_pat' });
  if (!res || typeof res.value !== 'string' || res.value === '') {
    throw new Error('credential-unconfigured: gitlab_pat 值为空（在 ~/.claude-view/plugin_credentials.json 配置）');
  }
  return res.value;
}

async function pickProject(explicit) {
  if (explicit) return explicit;
  if (process.env.GITLAB_PROJECT) return process.env.GITLAB_PROJECT;
  // 声明了 projects:read → 取 hub 工程清单第一项的目录名作缺省（只读，单次）
  try {
    const res = await callHub('hub.projects.list', {});
    const first = Array.isArray(res.projects) ? res.projects[0] : null;
    if (typeof first === 'string') return first.split('/').filter(Boolean).pop();
  } catch (e) {
    log(`hub.projects.list failed (non-fatal): ${e.message}`);
  }
  return null;
}

function log(msg) {
  // stdout 只走协议帧；日志一律 stderr（hub 采集脱敏）
  process.stderr.write(`[gitlab-issues-lite] ${msg}\n`);
}

/** 工具实现：hub.credentials.use 取凭据 → 直连 GitLab API → 压缩为视图/会话可消费的清单。 */
async function fetchIssues(args) {
  const pat = await getGitlabPat();
  const project = await pickProject(args && args.project);
  if (!project) {
    throw new Error('no-project: 未指定 project，且 hub 工程清单为空');
  }
  const encoded = encodeURIComponent(String(project));
  const url = `${API_BASE}/projects/${encoded}/issues?state=all&order_by=updated_at&per_page=30`;
  const resp = await fetch(url, {
    headers: { 'PRIVATE-TOKEN': pat, accept: 'application/json' },
  });
  if (resp.status === 401) {
    throw new Error('gitlab-auth-failed: 凭据被 GitLab 拒绝（401），请检查 gitlab_pat');
  }
  if (!resp.ok) {
    throw new Error(`gitlab-http-${resp.status}: ${url}`);
  }
  const raw = await resp.json();
  const issues = (Array.isArray(raw) ? raw : []).map((it) => ({
    iid: it.iid,
    title: it.title,
    state: it.state,
    labels: Array.isArray(it.labels) ? it.labels : [],
    assignee: it.assignee && it.assignee.name ? it.assignee.name : null,
    updatedAt: it.updated_at,
    webUrl: it.web_url,
  }));
  return {
    project: String(project),
    apiBase: API_BASE,
    fetchedAt: new Date().toISOString(),
    total: issues.length,
    issues,
  };
}

async function handleToolInvoke(params) {
  const toolId = params.toolId;
  const args = params.args || {};
  if (toolId !== 'fetch_issues') {
    return { content: [{ type: 'text', text: JSON.stringify({ error: `unknown tool: ${toolId}` }) }] };
  }
  try {
    const data = await fetchIssues(args);
    return { content: [{ type: 'text', text: JSON.stringify(data) }] };
  } catch (e) {
    log(`fetch_issues failed: ${e.message}`);
    return { content: [{ type: 'text', text: JSON.stringify({ error: e.message }) }] };
  }
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });
let shuttingDown = false;

rl.on('line', (line) => {
  const trimmed = line.trim();
  if (trimmed === '') return;
  let frame;
  try {
    frame = JSON.parse(trimmed);
  } catch {
    log(`non-JSON frame dropped (${trimmed.length} bytes)`);
    return;
  }
  // 应答：hub 对本进程请求的回包
  if (frame.id != null && frame.method == null) {
    const resolve = pending.get(frame.id);
    if (resolve) {
      pending.delete(frame.id);
      resolve(frame.result, frame.error);
    }
    return;
  }
  // 通知
  if (frame.id == null) {
    if (frame.method === 'shutdown') {
      log('shutdown received, exiting');
      shuttingDown = true;
      process.exit(0);
    }
    return;
  }
  // 请求
  const { id, method, params = {} } = frame;
  Promise.resolve()
    .then(() => {
      switch (method) {
        case 'initialize':
          // apiVersion 由 hub 下发；本插件按 v1 能力面应答
          return { ok: true, capabilities: ['tools'] };
        case 'tool.invoke':
          return handleToolInvoke(params);
        case 'health':
          return { ok: true };
        default:
          return Promise.reject(Object.assign(new Error(`unknown method: ${method}`), { code: -32601 }));
      }
    })
    .then(
      (result) => send({ jsonrpc: '2.0', id, result }),
      (err) =>
        send({
          jsonrpc: '2.0',
          id,
          error: { code: err.code === -32601 ? -32601 : -32003, message: err.message },
        }),
    );
});

rl.on('close', () => {
  if (!shuttingDown) log('stdin closed, exiting');
  process.exit(0);
});

log(`ready (apiBase=${API_BASE})`);
