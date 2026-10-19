'use strict';

/* GitLab Issues Lite 视图（原生 JS）：
 * - 单一标题层（宿主 chrome 提供标题，视图不自绘标题；反馈 #3）；
 * - 先选项目再列 issues（反馈 #4）：工程下拉（hub.api.projects.list → 新增的
 *   hub.projects.list 对齐，下拉直达新 data 域）→ 选后调用 hub.backend.fetch_issues →
 *   渲染；错误态（凭据未配置 / 无项目）文案做清楚。 */

(function () {
  var stateEl = document.getElementById('state');
  var boardEl = document.getElementById('board');
  var pickerEl = document.getElementById('picker');
  var metaEl = document.getElementById('meta');

  var pluginId = null;
  var rpcSeq = 0;
  var pendingRpc = new Map();

  function post(msg) { window.parent.postMessage(msg, '*'); }
  function esc(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  function applyTokens(tokens, theme) {
    if (!tokens) return;
    var rootStyle = document.documentElement.style;
    Object.keys(tokens).forEach(function (n) { rootStyle.setProperty(n.replace(/^--c-/, '--'), tokens[n]); });
    if (theme) document.documentElement.dataset.theme = theme;
  }

  function callHub(method, params) {
    return new Promise(function (resolve, reject) {
      var id = 'view-' + Date.now() + '-' + (++rpcSeq);
      pendingRpc.set(id, { resolve: resolve, reject: reject });
      post({ type: 'hub:rpc', payload: { id: id, method: method, params: params || {} } });
      setTimeout(function () {
        if (pendingRpc.has(id)) { pendingRpc.delete(id); reject(new Error('取数超时（宿主未回执）')); }
      }, 45000);
    });
  }

  var projectPollTimer = null;
  var currentProject = '';

  function renderPicker(projects) {
    pickerEl.innerHTML = '';
    var label = document.createElement('span');
    label.className = 'project-label';
    label.textContent = '项目';
    var input = document.createElement('input');
    input.className = 'project-input';
    input.placeholder = '输入或选择项目';
    input.value = currentProject;
    input.addEventListener('input', function () { currentProject = input.value.trim(); filterPickerList(input.value); });
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'btn';
    btn.textContent = '查询';
    btn.addEventListener('click', loadIssues);
    pickerEl.appendChild(label);
    pickerEl.appendChild(input);
    pickerEl.appendChild(btn);
    allProjects = (Array.isArray(projects) ? projects : []).map(function (p) { return String(p); });
    buildPickerList(input);
  }

  var allProjects = [];
  var pickerListEl = null;

  function buildPickerList(inputEl) {
    if (pickerListEl) pickerListEl.remove();
    pickerListEl = document.createElement('div');
    pickerListEl.className = 'project-list';
    pickerListEl.hidden = true;
    renderPickerOptions('', inputEl);
    inputEl.insertAdjacentElement('afterend', pickerListEl);
    inputEl.addEventListener('focus', function () { pickerListEl.hidden = false; });
    document.addEventListener('click', function (e) {
      if (!pickerListEl) return;
      if (e.target === inputEl || pickerListEl.contains(e.target)) return;
      pickerListEl.hidden = true;
    });
  }

  function filterPickerList(q) {
    renderPickerOptions(q, null);
    if (pickerListEl) pickerListEl.hidden = false;
  }

  function renderPickerOptions(q, inputEl) {
    if (!pickerListEl) return;
    pickerListEl.innerHTML = '';
    var needle = (q || '').toLowerCase();
    var items = allProjects.filter(function (p) { return !needle || p.toLowerCase().indexOf(needle) !== -1; }).slice(0, 20);
    if (items.length === 0) {
      var empty = document.createElement('div');
      empty.className = 'project-item';
      empty.textContent = allProjects.length === 0 ? '暂无工程（先在 hub 注册工程目录）' : '无匹配项目（可直接输入项目名）';
      pickerListEl.appendChild(empty);
      return;
    }
    items.forEach(function (name) {
      var it = document.createElement('div');
      it.className = 'project-item';
      it.textContent = name;
      it.addEventListener('click', function () {
        currentProject = name;
        var inp = pickerEl.querySelector('.project-input');
        if (inp) inp.value = name;
        pickerListEl.hidden = true;
        loadIssues();
      });
      pickerListEl.appendChild(it);
    });
  }

  function setState(text, isError) {
    stateEl.className = isError ? 'state error' : 'state';
    stateEl.textContent = text;
    stateEl.style.display = '';
  }

  function renderIssues(data) {
    stateEl.style.display = 'none';
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
    if (!data.issues || data.issues.length === 0) setState('该项目暂无 issue', false);
    metaEl.textContent = data.project + ' · 共 ' + data.total + ' 条 · 取数于 ' + (data.fetchedAt || '').replace('T', ' ').slice(0, 19);
  }

  function failText(e) {
    var msg = String(e && e.message || e);
    if (msg.indexOf('credential-unconfigured') === 0 || /凭据.*未配置/.test(msg) || /401/.test(msg)) {
      return '凭据未配置或无效：请在「插件管理」中设置 gitlab_pat（凭据值只存 hub 侧、零回显），再回来按「查询」重试。';
    }
    if (msg.indexOf('no-project') === 0 || /未指定 project/.test(msg)) {
      return '未指定项目：请在上方输入框输入或选择一个项目，再按「查询」。';
    }
    if (msg.indexOf('未声明') !== -1 || msg.indexOf('violation') !== -1) {
      return '请求被拒：' + msg + '（请检查插件权限声明）';
    }
    return '取数失败：' + msg;
  }

  function loadIssues() {
    var proj = (currentProject || pickerEl.querySelector('.project-input').value || '').trim();
    if (!proj) { setState('请先选择或输入一个项目，再按「查询」。', false); return; }
    currentProject = proj;
    setState('取数中…', false);
    callHub('hub.backend.fetch_issues', { args: { project: proj } }).then(function (res) {
      var block = res && res.content && res.content[0];
      var data = block && typeof block.text === 'string' ? JSON.parse(block.text) : null;
      if (!data || data.error) throw new Error((data && data.error) || '空结果');
      renderIssues(data);
    }).catch(function (e) { setState(failText(e), true); });
  }

  function loadProjects() {
    setState('正在拉取工程清单…', false);
    callHub('hub.projects.list', {}).then(function (res) {
      var list = res && Array.isArray(res.projects) ? res.projects : [];
      renderPicker(list);
      if (list.length > 0) currentProject = String(list[0]).split('/').filter(Boolean).pop() || '';
      var inp = pickerEl.querySelector('.project-input');
      if (inp) inp.value = currentProject;
      setState('请选择项目后按「查询」拉取 issue。', false);
    }).catch(function () {
      renderPicker([]);
      setState('工程清单获取失败，可直接输入项目名后查询。', true);
    });
  }

  window.addEventListener('message', function (ev) {
    if (ev.source !== window.parent) return;
    var data = ev.data || {};
    if (data.type === 'hub:init') {
      var p = data.payload || {};
      pluginId = p.pluginId;
      applyTokens(p.tokens, p.theme);
      loadProjects();
      return;
    }
    if (data.type === 'hub:theme') { applyTokens(data.payload && data.payload.tokens, data.payload && data.payload.theme); return; }
    if (data.type === 'hub:rpc-result' || data.type === 'hub:rpc-error') {
      var payload = data.payload || {};
      var w = pendingRpc.get(payload.id);
      if (!w) return;
      pendingRpc.delete(payload.id);
      if (data.type === 'hub:rpc-result') w.resolve(payload.result);
      else w.reject(new Error((payload.code || 'error') + ': ' + (payload.message || '')));
    }
  });

  post({ type: 'hub:ready', payload: { pluginId: pluginId, viewId: 'issues' } });
})();
