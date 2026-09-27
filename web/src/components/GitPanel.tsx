import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, type ReactElement } from 'react';
import { api } from '../api';
import type { GitStatusFile, GitTreeNode } from '../types';
import './GitPanel.css';

interface Props {
  node: GitTreeNode;
}

/// P6 B4-B7 git 面板：选中节点的分支/upstream/ahead-behind、变更文件三组
/// （staged/unstaged/untracked，点击看只读 diff）、submodule 清单（前端按需拉
/// 子模块 git-status 即递归展开），以及基于当前节点创建 worktree 的表单。
export default function GitPanel({ node }: Props): ReactElement {
  const qc = useQueryClient();
  const status = useQuery({
    queryKey: ['gitStatus', node.path],
    queryFn: () => api.gitStatus(node.path),
    // 非 git 目录（r41 观察项）：不发请求，面板回落空态
    enabled: node.isGit,
    refetchInterval: 30_000,
  });
  const submodules = useQuery({
    queryKey: ['submodules', node.path],
    queryFn: () => api.submodules(node.path),
    enabled: node.isGit,
  });
  const [diffFile, setDiffFile] = useState<{ file: string; cached: boolean } | null>(null);
  const diff = useQuery({
    queryKey: ['gitDiff', node.path, diffFile?.file, diffFile?.cached],
    queryFn: () => api.gitDiff(node.path, diffFile!.file, diffFile!.cached),
    enabled: diffFile != null,
  });
  const [wtName, setWtName] = useState('');
  const [wtMsg, setWtMsg] = useState('');

  const st = status.data;
  const staged = st?.files.filter((f) => f.x !== ' ' && f.x !== '?') ?? [];
  const unstaged = st?.files.filter((f) => f.y !== ' ' && f.y !== '?' && f.x !== '?') ?? [];
  const untracked = st?.files.filter((f) => f.x === '?' && f.y === '?') ?? [];

  const createWt = async (): Promise<void> => {
    setWtMsg('创建中…');
    try {
      const r = await api.createWorktree(node.path, wtName.trim());
      setWtName('');
      setWtMsg(`✓ 已创建 ${r.path}`);
      await qc.invalidateQueries({ queryKey: ['projectTree'] });
    } catch (e) {
      setWtMsg(String(e));
    }
  };

  const fileRow = (f: GitStatusFile, cached: boolean): ReactElement => {
    const mark = cached ? f.x : f.y === ' ' ? f.x : f.y;
    return (
      <button
        key={f.path}
        type="button"
        className="git-file"
        onClick={() => setDiffFile({ file: f.path, cached })}
      >
        <span className="git-file-mark">{mark === '!' ? '?' : mark}</span>
        <span className="git-file-path" title={f.origPath ? `${f.origPath} → ${f.path}` : f.path}>
          {f.origPath ? `${f.origPath} → ${f.path}` : f.path}
        </span>
      </button>
    );
  };

  return (
    <div className="git-panel">
      <div className="git-title">
        Git · {st?.branch ?? node.branch ?? '—'}
        {st?.upstream ? <span className="git-upstream"> → {st.upstream}</span> : null}
      </div>
      {st ? (
        <div className="git-meta">
          {st.upstream && (st.ahead > 0 || st.behind > 0) ? (
            <span className="git-ab">
              ↑{st.ahead} ↓{st.behind}
            </span>
          ) : null}
          <span>变更 {st.files.length}</span>
        </div>
      ) : (
        <p className="git-empty">状态加载中…</p>
      )}

      {st && st.files.length > 0 ? (
        <div className="git-files">
          {staged.length > 0 ? (
            <>
              <div className="git-group-title">已暂存 ({staged.length})</div>
              {staged.map((f) => fileRow(f, true))}
            </>
          ) : null}
          {unstaged.length > 0 ? (
            <>
              <div className="git-group-title">未暂存 ({unstaged.length})</div>
              {unstaged.map((f) => fileRow(f, false))}
            </>
          ) : null}
          {untracked.length > 0 ? (
            <>
              <div className="git-group-title">未跟踪 ({untracked.length})</div>
              {untracked.map((f) => fileRow(f, false))}
            </>
          ) : null}
        </div>
      ) : st ? (
        <p className="git-empty">工作区干净</p>
      ) : null}

      {diffFile ? (
        <div className="git-diff-box">
          <div className="git-diff-head">
            <span>{diffFile.cached ? '已暂存' : '工作区'} diff · {diffFile.file}</span>
            <button type="button" onClick={() => setDiffFile(null)} aria-label="关闭 diff">
              ×
            </button>
          </div>
          <pre className="git-diff">{diff.data?.diff || '（无差异）'}</pre>
        </div>
      ) : null}

      {submodules.data && submodules.data.length > 0 ? (
        <div className="git-submods">
          <div className="git-group-title">Submodules</div>
          {submodules.data.map((s) => (
            <div key={s.path} className="git-submod">
              <span className="git-submod-mark">{s.status === ' ' ? '·' : s.status}</span>
              <span className="git-file-path" title={s.path}>
                {s.path}
              </span>
              <span className="git-submod-sha">{s.sha}</span>
            </div>
          ))}
        </div>
      ) : null}

      <div className="git-wt-form">
        <input
          value={wtName}
          onChange={(e) => setWtName(e.target.value)}
          placeholder="新分支名（基于当前节点）"
          aria-label="New worktree branch name"
        />
        <button type="button" disabled={!wtName.trim()} onClick={() => void createWt()}>
          + worktree
        </button>
      </div>
      {wtMsg ? <p className="git-wt-msg">{wtMsg}</p> : null}
    </div>
  );
}
