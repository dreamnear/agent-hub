import { useQuery } from '@tanstack/react-query';
import type { ReactElement } from 'react';
import { useState } from 'react';
import { api } from '../api';
import { useI18n } from '../i18n';
import type { AgentSummary, GitTreeNode } from '../types';
import GitPanel from './GitPanel';
import './ProjectSidebar.css';

interface Props {
  agents: AgentSummary[];
  projects: string[];
  checked: ReadonlySet<string>;
  pending: string;
  onPendingChange: (v: string) => void;
  onToggle: (path: string) => void;
  onAdd: () => void;
  onCollapse?: () => void;
}

/// 右工程栏（P6 B1-B3）：git 探测的主仓/worktree 树归组（可折叠），每节点可打开
/// 本地目录（Finder，服务端白名单校验）；非 git 目录平铺；树为空回退注册列表平铺。
export default function ProjectSidebar({
  agents,
  projects,
  checked,
  pending,
  onPendingChange,
  onToggle,
  onAdd,
  onCollapse,
}: Props): ReactElement {
  const t = useI18n();
  const tree = useQuery({
    queryKey: ['projectTree'],
    queryFn: api.projectTree,
    refetchInterval: 30_000,
  });
  // open-dir 失败提示（403/失败文案），next render 或下次成功清除
  const [openErr, setOpenErr] = useState('');
  const [openBusy, setOpenBusy] = useState<string | null>(null);
  // 选中节点（P6 B5）：点击节点名展示 git 面板
  const [selected, setSelected] = useState<GitTreeNode | null>(null);

  const counts = agents.reduce<Record<string, number>>((acc, a) => {
    const dir = a.cwd ?? '(untracked)';
    acc[dir] = (acc[dir] ?? 0) + 1;
    return acc;
  }, {});

  const open = async (path: string): Promise<void> => {
    setOpenBusy(path);
    setOpenErr('');
    try {
      await api.openDir(path);
    } catch (e) {
      setOpenErr(t('proj.openFailed', { err: String(e) }));
    } finally {
      setOpenBusy(null);
    }
  };

  const row = (node: GitTreeNode, sub: boolean): ReactElement => (
    <li key={node.path} className={sub ? 'tree-row tree-sub-row' : 'tree-row'}>
      <label>
        <input
          type="checkbox"
          checked={checked.has(node.path)}
          onChange={() => onToggle(node.path)}
        />
        <span
          className="tree-name tree-name--select"
          title={node.path}
          onClick={() => setSelected(node)}
        >
          {node.name}
        </span>
        {node.branch ? <span className="tree-branch">{node.branch}</span> : null}
        <span className="tree-count">{counts[node.path] ?? 0}</span>
        <button
          type="button"
          className="tree-open"
          title={t('proj.openTitle')}
          aria-label={t('proj.openAria', { name: node.name })}
          disabled={openBusy != null}
          onClick={() => void open(node.path)}
        >
          📂
        </button>
      </label>
    </li>
  );

  const groups = tree.data;
  // 注册列表中 git 探测未识别的路径（如大小写不符/已移动的死路径）：树模式也必须
  // 渲染成可勾选条目——projectKey 初始化会全勾注册列表，若这些 key 无 checkbox 可
  // 取消，过滤集合就会留下用户无法移除的幽灵勾选锁死列表（r51）
  const treePaths =
    groups != null ? new Set(groups.flatMap((g) => [g.main.path, ...g.worktrees.map((w) => w.path)])) : null;
  const unregistered = treePaths != null ? projects.filter((p) => !treePaths.has(p)) : [];
  return (
    <nav className="sidebar" aria-label="Projects">
      <div className="sidebar-head">
        <h2 className="sidebar-title">{t('proj.title')}</h2>
        {onCollapse ? (
          <button type="button" className="sidebar-collapse" onClick={onCollapse}>
            {t('proj.collapse')}
          </button>
        ) : null}
      </div>
      {openErr ? <p className="sidebar-open-err" role="alert">{openErr}</p> : null}
      {groups != null && groups.length > 0 ? (
        <ul className="sidebar-tree">
          {groups.map((g) =>
            g.worktrees.length > 0 ? (
              <li key={g.main.path} className="tree-group-item">
                <details className="tree-group" open>
                  <summary className="tree-row tree-row--main">
                    {/* 组头可勾（反馈轮 22）：主仓 path 由此进过滤集合——此前组头无
                        checkbox，主仓根 cwd 会话在全勾下仍被滤掉；click 不冒泡到
                        summary，勾选不触发折叠 */}
                    <input
                      type="checkbox"
                      checked={checked.has(g.main.path)}
                      onChange={() => onToggle(g.main.path)}
                      onClick={(e) => e.stopPropagation()}
                    />
                    <span className="tree-caret" aria-hidden="true">
                      ▸
                    </span>
                    <span
                      className="tree-name tree-name--select"
                      title={g.main.path}
                      onClick={() => setSelected(g.main)}
                    >
                      {g.main.name}
                    </span>
                    {g.main.branch ? <span className="tree-branch">{g.main.branch}</span> : null}
                    <span className="tree-count">{counts[g.main.path] ?? 0}</span>
                    <button
                      type="button"
                      className="tree-open"
                      title={t('proj.openTitle')}
                      aria-label={t('proj.openAria', { name: g.main.name })}
                      disabled={openBusy != null}
                      onClick={(e) => {
                        e.stopPropagation();
                        void open(g.main.path);
                      }}
                    >
                      📂
                    </button>
                  </summary>
                  <ul className="tree-sub">{g.worktrees.map((w) => row(w, true))}</ul>
                </details>
              </li>
            ) : (
              row(g.main, false)
            ),
          )}
          {unregistered.map((path) => (
            <li key={path} className="tree-row">
              <label>
                <input type="checkbox" checked={checked.has(path)} onChange={() => onToggle(path)} />
                <span className="tree-name" title={path}>
                  {path.split('/').pop() ?? path}
                </span>
                <span className="tree-count">{counts[path] ?? 0}</span>
              </label>
            </li>
          ))}
        </ul>
      ) : projects.length === 0 ? (
        <p className="sidebar-empty">{t('proj.empty')}</p>
      ) : (
        // 树未就绪/为空：回退注册列表平铺（旧行为）
        <ul className="sidebar-tree">
          {projects.map((path) => (
            <li key={path} className="tree-row">
              <label>
                <input
                  type="checkbox"
                  checked={checked.has(path)}
                  onChange={() => onToggle(path)}
                />
                <span className="tree-name" title={path}>
                  {path.split('/').pop() ?? path}
                </span>
                <span className="tree-count">{counts[path] ?? 0}</span>
              </label>
            </li>
          ))}
        </ul>
      )}
      <div className="sidebar-add">
        <input
          value={pending}
          onChange={(e) => onPendingChange(e.target.value)}
          placeholder="/absolute/path/to/project"
          size={18}
          aria-label="New project path"
        />
        <button type="button" className="btn-ghost" onClick={onAdd} disabled={!pending.trim()}>
          Add
        </button>
      </div>
      {selected ? (
        <GitPanel node={selected} />
      ) : (
        <div className="git-panel">
          <div className="git-title">Git · —</div>
          <p className="git-empty">{t('proj.gitEmpty')}</p>
        </div>
      )}
    </nav>
  );
}
