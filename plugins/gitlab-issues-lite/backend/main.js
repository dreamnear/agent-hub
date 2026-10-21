#!/usr/bin/env node
//! gitlab-issues-lite backend（ADR-0006：每插件一进程 sidecar，stdio 换行分隔 JSON-RPC 2.0）。
//!
//! 纪律（本文件即插件侧代码边界）：
//! - 一切 hub 访问经 stdio RPC（`hub.credentials.use` / `hub.projects.list`），
//!   **零直调 hub `/api/*`**（hub token 也不下发到本进程）；
//! - 凭据按名引用（gitlab_pat），值只在 RPC 应答载荷出现，不落盘、不打日志（零回显）；
//! - 本进程自行出站访问 GitLab（ADR-0006 拍板结论 5：v1 不要求出站经 hub）；
//! - 绑定到工程（预览反馈第 2 轮）：选中 hub 工程后用 `git remote get-url origin`
//!   （只读）反解 GitLab host + 项目路径，PAT 天然对上自建实例。
//!
//! 支持方法：initialize / tool.invoke(fetch_issues | add_issue_note) / health；通知 shutdown 后退出。
//! add_issue_note（B2 预览反馈增量）：高危写工具演示——risk:"high" 经 hub 高危审批门
//! （逐次确认 + 留痕），本进程只在审批通过后被调用，自身不做二次判定（闸门单点在 hub）。
//! 环境变量：GITLAB_API_BASE（最高优先——E2E mock 链路；设置后跳过 git 反解）、
//!           GITLAB_PROJECT（仅 env 形态下的缺省项目标识）。

'use strict';

const readline = require('node:readline');
const { execFileSync } = require('node:child_process');

// env 显式设置（含空串视为未设）才启用 env 形态；真实使用走 git 反解
const ENV_API_BASE = (process.env.GITLAB_API_BASE || '').trim();
const ENV_PROJECT = (process.env.GITLAB_PROJECT || '').trim();

// ===== git remote 反解（预览反馈第 2 轮） =====

/** 读 origin remote URL（只读；非 git 目录 / 无 origin → null）。 */
function readOriginRemote(projectPath) {
  try {
    return execFileSync('git', ['-C', projectPath, 'remote', 'get-url', 'origin'], {
      encoding: 'utf8',
      timeout: 5000,
    }).trim();
  } catch {
    return null;
  }
}

/**
 * 反解 remote URL → { scheme, host, projectPath }。
 * 支持：ssh 短形态 `git@host:group/repo.git`、`ssh://[user@]host[:port]/group/repo.git`、
 * `https://host/group/repo.git`（http 同理）。解析不了返回 null。
 */
function parseGitRemote(url) {
  const u = url.trim();
  let m = u.match(/^git@([^:/]+):(.+)$/i);
  if (m) {
    return { scheme: 'https', host: m[1], projectPath: m[2].replace(/\.git$/i, '') };
  }
  m = u.match(/^ssh:\/\/(?:[^@/]+@)?([^/]+)\/(.+)$/i);
  if (m) {
    return { scheme: 'https', host: m[1], projectPath: m[2].replace(/\.git$/i, '') };
  }
  if (/^https?:\/\//i.test(u)) {
    try {
      const parsed = new URL(u);
      const path = parsed.pathname.replace(/^\/+/, '').replace(/\.git$/i, '');
      if (parsed.host && path) {
        return { scheme: parsed.protocol.replace(':', ''), host: parsed.host, projectPath: path };
      }
    } catch {
      /* fallthrough */
    }
  }
  return null;
}

/** 工程绑定目标：env 优先（mock），否则 git remote 反解；失败给可指引错误。 */
function resolveTarget(args) {
  if (ENV_API_BASE) {
    const project = (args && args.project) || ENV_PROJECT;
    if (!project) {
      throw new Error('no-project: 未指定项目标识（env 形态需 GITLAB_PROJECT 或 project 参数）');
    }
    return {
      apiBase: ENV_API_BASE.replace(/\/+$/, ''),
      project: String(project),
      host: new URL(ENV_API_BASE).host,
      via: 'env',
    };
  }
  const dir = (args && args.project) || '';
  if (!dir) {
    throw new Error('no-project: 请先在上方选择一个工程（绑定到工程后按其 git remote 取数）');
  }
  const remote = readOriginRemote(dir);
  if (remote == null) {
    throw new Error(`no-git-remote: ${dir} 不是 git 仓库或没有 origin remote——请改选带 GitLab remote 的工程目录`);
  }
  const target = parseGitRemote(remote);
  if (target == null || !target.projectPath) {
    throw new Error(`git-remote-unrecognized: 无法从 origin remote 反解 GitLab 地址——请改用标准形态 remote（git@host:group/repo.git 或 https://host/group/repo.git）`);
  }
  return {
    apiBase: `${target.scheme}://${target.host}/api/v4`,
    project: target.projectPath,
    host: target.host,
    via: 'git',
  };
}

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

function log(msg) {
  // stdout 只走协议帧；日志一律 stderr（hub 采集脱敏）
  process.stderr.write(`[gitlab-issues-lite] ${msg}\n`);
}

/** 工具实现：选工程 → git remote 反解 host/项目 → hub.credentials.use 取凭据 → 直连该实例 → 压缩清单。 */
async function fetchIssues(args) {
  const target = resolveTarget(args);
  const pat = await getGitlabPat();
  const encoded = encodeURIComponent(target.project);
  const url = `${target.apiBase}/projects/${encoded}/issues?state=all&order_by=updated_at&per_page=30`;
  const resp = await fetch(url, {
    headers: { 'PRIVATE-TOKEN': pat, accept: 'application/json' },
  });
  if (resp.status === 401) {
    throw new Error(
      `gitlab-auth-failed: 凭据被 ${target.host} 拒绝（401）——请确认 gitlab_pat 对该 GitLab 实例有效`,
    );
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
    project: target.project,
    host: target.host,
    resolvedVia: target.via,
    apiBase: target.apiBase,
    fetchedAt: new Date().toISOString(),
    total: issues.length,
    issues,
  };
}

/** 工具实现（高危）：目标 issue → POST notes 写备注；调用前 hub 侧已走高危审批门。 */
async function addIssueNote(args) {
  const iid = Number(args && args.issue_iid);
  if (!Number.isInteger(iid) || iid <= 0) {
    throw new Error('bad-arg: issue_iid 必须是正整数（目标 issue 的 iid）');
  }
  const note = typeof (args && args.note) === 'string' ? args.note.trim() : '';
  if (!note) {
    throw new Error('bad-arg: note 不能为空（要写入的备注正文）');
  }
  const target = resolveTarget(args);
  const pat = await getGitlabPat();
  const encoded = encodeURIComponent(target.project);
  const url = `${target.apiBase}/projects/${encoded}/issues/${iid}/notes`;
  const resp = await fetch(url, {
    method: 'POST',
    headers: { 'PRIVATE-TOKEN': pat, 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({ body: note }),
  });
  if (resp.status === 401) {
    throw new Error(
      `gitlab-auth-failed: 凭据被 ${target.host} 拒绝（401）——请确认 gitlab_pat 对该 GitLab 实例有效`,
    );
  }
  if (!resp.ok) {
    throw new Error(`gitlab-http-${resp.status}: POST ${target.host}/${target.project}#${iid} notes`);
  }
  const raw = await resp.json();
  return {
    project: target.project,
    host: target.host,
    resolvedVia: target.via,
    issueIid: iid,
    noteId: raw && raw.id != null ? raw.id : null,
    createdAt: raw && raw.created_at ? raw.created_at : null,
    wroteAt: new Date().toISOString(),
  };
}

async function handleToolInvoke(params) {
  const toolId = params.toolId;
  const args = params.args || {};
  const impls = { fetch_issues: fetchIssues, add_issue_note: addIssueNote };
  const impl = impls[toolId];
  if (!impl) {
    return { content: [{ type: 'text', text: JSON.stringify({ error: `unknown tool: ${toolId}` }) }] };
  }
  try {
    const data = await impl(args);
    return { content: [{ type: 'text', text: JSON.stringify(data) }] };
  } catch (e) {
    log(`${toolId} failed: ${e.message}`);
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

log(ENV_API_BASE ? `ready (env apiBase=${ENV_API_BASE})` : 'ready (git-remote resolution mode)');
