//! GitLab Issues Lite 视图逻辑（ADR-0004 第 5 条消息契约，原生 JS）。
//!
//! 流程：hub:ready → 收 hub:init（theme + tokens 字典）→ 应用 token → hub:rpc
//! (`hub.backend.fetch_issues`) 取数 → 渲染。宿主主题切换收 hub:theme 全量重发，
//! 1 帧内应用（BR-16）。一切 hub 访问经宿主代理，零直调 /api/*（CSP 已结构性禁止）。

'use strict';

(function () {
  var stateEl = document.getElementById('state');
  var boardEl = document.getElementById('board');
  var projectEl = document.getElementById('project');
  var themeEl = document.getElementById('theme');
  var metaEl = document.getElementById('meta');
  var refreshBtn = document.getElementById('refresh');

  var pluginId = null;
  var rpcSeq = 0;
  var pendingRpc = new Map();
  var ready = false;

  function post(msg) {
    // 目标 origin 为 null（sandbox 无 allow-same-origin），宿主按 event.source 校验
    window.parent.postMessage(msg, '*');
  }

  // ===== token 应用（1 帧内，BR-16） =====
  function applyTokens(tokens, theme) {
    if (!tokens) return;
    var rootStyle = document.documentElement.style;
    Object.keys(tokens).forEach(function (name) {
      // --c-bg → --bg（视图内短名，见 style.css）
      var short = name.replace(/^--c-/, '--');
      rootStyle.setProperty(short, tokens[name]);
    });
    if (theme) {
      themeEl.textContent = '主题：' + (theme === 'dark' ? '暗' : '亮');
      document.documentElement.dataset.theme = theme;
    }
  }

  // ===== hub:rpc 请求-应答 =====
  function callBackend(args) {
    return new Promise(function (resolve, reject) {
      var id = 'view-' + Date.now() + '-' + (++rpcSeq);
      pendingRpc.set(id, { resolve: resolve, reject: reject });
      post({ type: 'hub:rpc', payload: { id: id, method: 'hub.backend.fetch_issues', params: { args: args || {} } } });
      setTimeout(function () {
        if (pendingRpc.has(id)) {
          pendingRpc.delete(id);
          reject(new Error('取数超时（宿主未回执）'));
        }
      }, 45000);
    });
  }

  function esc(s) {
    // 不可信输入（工具返回内容 = BR-14）渲染前转义，不 innerHTML 原文
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function setState(text, isError) {
    stateEl.className = isError ? 'state error' : 'state';
    stateEl.textContent = text;
    stateEl.style.display = '';
  }

  function render(data) {
    stateEl.style.display = 'none';
    projectEl.textContent = data.project || '—';
    var list = document.createElement('ul');
    list.className = 'list';
    (data.issues || []).forEach(function (it) {
      var li = document.createElement('li');
      li.className = 'issue';
      var title = esc(it.title);
      var link = it.webUrl
        ? '<a href="' + esc(it.webUrl) + '" target="_blank" rel="noopener noreferrer">' + title + '</a>'
        : title;
      li.innerHTML =
        '<span class="issue-iid">!' + esc(it.iid) + '</span>' +
        '<span class="issue-title">' + link + '</span>' +
        '<span class="issue-state issue-state--' + esc(it.state) + '">' + esc(it.state) + '</span>' +
        (it.labels || []).map(function (l) { return '<span class="label">' + esc(l) + '</span>'; }).join('') +
        '<span class="issue-meta">' + esc((it.updatedAt || '').slice(0, 10)) + (it.assignee ? ' · ' + esc(it.assignee) : '') + '</span>';
      list.appendChild(li);
    });
    var old = boardEl.querySelector('.list');
    if (old) old.remove();
    boardEl.appendChild(list);
    if (!data.issues || data.issues.length === 0) {
      setState('该项目暂无 issue', false);
    }
    metaEl.textContent = '共 ' + data.total + ' 条 · 取数于 ' + (data.fetchedAt || '').replace('T', ' ').slice(0, 19);
  }

  function load() {
    refreshBtn.disabled = true;
    setState('取数中（票据 → rpc → 权限判定 → 懒启动 backend → 凭据注入 → GitLab）…', false);
    callBackend({})
      .then(function (res) {
        // 工具返回 content[{type:'text', text:<json>}]（BR-14：内容按不可信输入处理）
        var block = res && res.content && res.content[0];
        var data = block && typeof block.text === 'string' ? JSON.parse(block.text) : null;
        if (!data || data.error) throw new Error((data && data.error) || '空结果');
        render(data);
      })
      .catch(function (e) {
        setState('取数失败：' + e.message + (String(e.message).indexOf('credential-unconfigured') === 0 ? '\n提示：在 ~/.claude-view/plugin_credentials.json 配置 {"gitlab_pat": "<token>"} 后重试。' : ''), true);
      })
      .finally(function () {
        refreshBtn.disabled = false;
      });
  }

  refreshBtn.addEventListener('click', load);

  // ===== 宿主消息（来源校验在宿主侧；本侧只认 window.parent 来的回执） =====
  window.addEventListener('message', function (ev) {
    if (ev.source !== window.parent) return;
    var data = ev.data || {};
    if (data.type === 'hub:init') {
      var p = data.payload || {};
      pluginId = p.pluginId;
      applyTokens(p.tokens, p.theme);
      ready = true;
      load();
      return;
    }
    if (data.type === 'hub:theme') {
      applyTokens(data.payload && data.payload.tokens, data.payload && data.payload.theme);
      return;
    }
    if (data.type === 'hub:rpc-result' || data.type === 'hub:rpc-error') {
      var payload = data.payload || {};
      var waiting = pendingRpc.get(payload.id);
      if (!waiting) return;
      pendingRpc.delete(payload.id);
      if (data.type === 'hub:rpc-result') waiting.resolve(payload.result);
      else waiting.reject(new Error((payload.code || 'error') + ': ' + (payload.message || '')));
    }
  });

  // 就绪：宿主据此发 hub:init
  post({ type: 'hub:ready', payload: { pluginId: pluginId, viewId: 'issues' } });
})();
