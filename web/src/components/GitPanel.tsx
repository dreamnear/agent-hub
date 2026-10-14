import { useQuery } from '@tanstack/react-query';
import { Fragment, useEffect, useMemo, useState, type ReactElement } from 'react';
import type { Api } from '../api';
import { useI18n } from '../i18n';
import type { GitStatus, GitStatusFile } from '../types';
import FileIcon from './FileIcon';
import './GitPanel.css';

interface Props {
  /// 当前活跃会话的工作路径（Git 面板跟随会话 cwd，不再由工程树选中驱动）
  cwd: string;
  /// 会话所属实例的 api（本机=api，远端=实例代理；git 端点在 Api 工厂里按 baseUrl 前缀路由）
  api: Api;
  /// queryKey 实例隔离：跨机同 cwd 路径不得共享缓存
  instanceId: string | null;
  /// 点击变更文件 → diff 视图由主区覆盖层渲染（GitDiffView），窄栏不再内嵌；
  /// path = 仓库路径（多仓聚合后父/子仓各自分区，diff 归属各自仓）；
  /// hash = Timeline 提交内文件（git-timeline-files 两级联动；缺省 = 工作区 diff）
  onOpenDiff: (f: { cwd: string; file: string; cached: boolean; hash?: string }) => void;
}

/** 路径拼接：子模块相对路径 → 当前层绝对路径（防双斜杠/尾斜杠） */
function joinPath(base: string, rel: string): string {
  return `${base.replace(/\/+$/, '')}/${rel.replace(/^\/+/, '')}`;
}

// —— 双视图 + 状态色标（用户给图定案工单）——

/// 视图持久化键（跟随 hub_* 键模式）：缺省平铺
export const GIT_VIEW_KEY = 'hub_git_view';
export type GitViewMode = 'flat' | 'tree';

export function loadGitView(): GitViewMode {
  return localStorage.getItem(GIT_VIEW_KEY) === 'tree' ? 'tree' : 'flat';
}

export function saveGitView(v: GitViewMode): void {
  try {
    localStorage.setItem(GIT_VIEW_KEY, v);
  } catch {
    // 存储不可用（隐私模式等）：仅内存生效
  }
}

// —— 分区折叠持久化（agent-hub-git-timeline：VSCode SCM 惯例跨会话记忆）——

export const GIT_SECTIONS_KEY = 'hub_git_sections';

function loadSectionMap(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(GIT_SECTIONS_KEY);
    return raw ? (JSON.parse(raw) as Record<string, boolean>) : {};
  } catch {
    return {};
  }
}

function saveSectionMap(m: Record<string, boolean>): void {
  try {
    localStorage.setItem(GIT_SECTIONS_KEY, JSON.stringify(m));
  } catch {
    // 存储不可用：仅内存生效
  }
}

/** 分区展开态：父仓「变更/Timeline」缺省展开（VSCode SCM 惯例），
 * 子仓分区缺省收起——大仓懒加载、不抢首屏（展开才挂载 RepoContent/Timeline）。 */
export function isSectionOpen(map: Record<string, boolean>, key: string): boolean {
  if (key in map) return map[key] as boolean;
  if (key.startsWith('sub:')) return false;
  return true;
}

/// x/y 状态码 → 展示字母：?→U（绿，未跟踪含 staged A）/ D→D（红+删除线）/
/// 子模块条目→S（灰蓝）/ 其余（M/R/C）→M（淡黄）。
export function statusKind(mark: string, isSubmodule: boolean): 'U' | 'D' | 'S' | 'M' {
  if (isSubmodule) return 'S';
  if (mark === '?' || mark === 'A') return 'U';
  if (mark === 'D') return 'D';
  return 'M';
}

/** 展示用状态字母（对齐既有 fileRow 口径：cached 取 x，否则 y 优先、空回退 x） */
export function markOf(f: { x: string; y: string }, cached: boolean): string {
  return cached ? f.x : f.y === ' ' ? f.x : f.y;
}

/// 树视图条目（路径去重后；cached 取首次出现——staged 优先=diff 基于索引）
export interface FileEntry {
  path: string;
  mark: string;
  cached: boolean;
  isSub: boolean;
  origPath: string | null;
}

export type TreeNode =
  | {
      kind: 'dir';
      name: string;
      path: string;
      count: number;
      kinds: string[];
      children: TreeNode[];
    }
  | { kind: 'file'; name: string; path: string; letter: string; cached: boolean; origPath: string | null };

type FileNode = Extract<TreeNode, { kind: 'file' }>;

interface DirBuilder {
  dirs: Map<string, DirBuilder>;
  files: FileNode[];
  count: number;
  kinds: Set<string>;
  name: string;
  path: string;
}

/** 文件路径集 → 可折叠目录树：目录聚合计数与状态字母集（● 表示混多种），
 * 目录排文件前、同级字母序；同路径条目去重（staged/unstaged 双列时首见胜出）。 */
export function buildFileTree(entries: FileEntry[]): TreeNode[] {
  const root: DirBuilder = {
    dirs: new Map(),
    files: [],
    count: 0,
    kinds: new Set(),
    name: '',
    path: '',
  };
  const seen = new Set<string>();
  for (const e of entries) {
    if (seen.has(e.path)) continue;
    seen.add(e.path);
    const parts = e.path.split('/');
    const name = parts.pop() as string;
    let cur = root;
    let acc = '';
    for (const p of parts) {
      acc = acc ? `${acc}/${p}` : p;
      let next = cur.dirs.get(p);
      if (next == null) {
        next = { dirs: new Map(), files: [], count: 0, kinds: new Set(), name: p, path: acc };
        cur.dirs.set(p, next);
      }
      cur = next;
    }
    const letter = statusKind(e.mark, e.isSub);
    cur.files.push({
      kind: 'file',
      name,
      path: e.path,
      letter,
      cached: e.cached,
      origPath: e.origPath,
    });
  }
  const convert = (b: DirBuilder): TreeNode[] => {
    const dirNodes: TreeNode[] = [...b.dirs.values()].map((child) => {
      const children = convert(child);
      // count = 子树文件总数（file 各计 1，dir 递归其 count；勿再叠加 child.files.length 双算）
      const count = children.reduce((n, c) => n + (c.kind === 'dir' ? c.count : 1), 0);
      const kinds = new Set<string>();
      for (const c of children) {
        if (c.kind === 'dir') c.kinds.forEach((k) => kinds.add(k));
        else kinds.add(c.letter);
      }
      return {
        kind: 'dir',
        name: child.name,
        path: child.path,
        count,
        kinds: [...kinds],
        children,
      };
    });
    const fileNodes: TreeNode[] = [...b.files];
    return [...dirNodes, ...fileNodes];
  };
  return convert(root);
}

/** 仓库名（路径尾段）：分区标题用 */
function repoName(p: string): string {
  return p.replace(/\/+$/, '').split('/').pop() || p;
}

// —— 行组件统一复用（agent-hub-git-panel-row-unify）：各区行样式单一来源 ——
// 根因背景：主仓 Timeline / 子仓 Timeline / 变更区平铺 / 变更区树四区各自为政的
// 行样式，每次修复只打一个区。TimelineRow/FileRow 组件化 + 硬口径内建（单行
// nowrap、行首左锚 justify flex-start 覆盖全局 button 居中、行框 max-content
// 随内容撑开 hover 背景跟随横滚、min-width:100% 对齐线、hover title 全名）。

/// Timeline 提交行（主仓/子仓 Timeline 复用；三段式 hash 徽标 + subject +
/// author·时间）。纯展示：复制/选中交互由父级回调。
export function TimelineRow({
  hash,
  subject,
  meta,
  selected,
  copied,
  copiedLabel,
  copyTitle,
  onToggle,
  onCopy,
}: {
  hash: string;
  subject: string;
  meta: string;
  selected: boolean;
  copied: boolean;
  copiedLabel: string;
  copyTitle: string;
  onToggle: () => void;
  onCopy: () => void;
}): ReactElement {
  return (
    <button
      type="button"
      className={`git-commit${selected ? ' is-selected' : ''}`}
      aria-expanded={selected}
      title={subject}
      onClick={onToggle}
    >
      <span
        className="git-commit-hash"
        title={copyTitle}
        onClick={(e) => {
          e.stopPropagation();
          onCopy();
        }}
      >
        {copied ? copiedLabel : hash}
      </span>
      <span className="git-commit-main">
        <span className="git-commit-subject">{subject}</span>
        <span className="git-commit-meta">{meta}</span>
      </span>
    </button>
  );
}

/// 文件行（变更区平铺 / 树视图 / Timeline 提交清单复用；icon + 文件名 + 目录小字 +
/// 状态徽标）。indent = 层级缩进 px（树视图以左为根；平铺/清单 0）。letter 为展示
/// 字母（原样渲染），色标由 statusKind 派生；deleted = 删除线（D 态）。
export function FileRow({
  path,
  name,
  dir,
  letter,
  title,
  indent = 0,
  deleted = false,
  onClick,
}: {
  path: string;
  name: string;
  dir: string | null;
  letter: string;
  title: string;
  indent?: number;
  deleted?: boolean;
  onClick: () => void;
}): ReactElement {
  const kind = statusKind(letter, false);
  return (
    <button
      type="button"
      className={`git-file-row${deleted ? ' is-deleted' : ''}`}
      style={indent > 0 ? { paddingLeft: `${indent}px` } : undefined}
      title={title}
      onClick={onClick}
    >
      <FileIcon name={path} />
      <span className={`git-file-row-name${deleted ? ' is-deleted' : ''}`}>{name}</span>
      {dir ? <span className="git-file-row-dir">{dir}</span> : null}
      <span className={`git-kind is-${kind}`}>{letter}</span>
    </button>
  );
}

// —— Timeline 提交历史（agent-hub-git-timeline T1/T2：限量 + 加载更多）——
// git-timeline-files：点提交行 → 展开/收起该提交的文件维变更清单（选中态高亮），
// hash 徽标仍点击复制（stopPropagation 不触发选中）

function Timeline({
  repoPath,
  api,
  instanceId,
  onOpenFile,
}: {
  repoPath: string;
  api: Api;
  instanceId: string | null;
  onOpenFile: (f: { hash: string; file: string }) => void;
}): ReactElement {
  const t = useI18n();
  const [limit, setLimit] = useState(50);
  const [copied, setCopied] = useState<string | null>(null);
  // F1 选中提交：再点同条收起；换条直接切换（清单随 selected 重挂）
  const [selected, setSelected] = useState<string | null>(null);
  const log = useQuery({
    queryKey: ['gitLog', instanceId, repoPath, limit],
    queryFn: () => api.gitLog(repoPath, limit, 0),
    staleTime: 30_000,
  });
  const copyHash = (h: string): void => {
    void navigator.clipboard?.writeText(h).catch(() => {});
    setCopied(h);
    setTimeout(() => setCopied((cur) => (cur === h ? null : cur)), 1500);
  };
  if (log.isLoading) return <p className="git-empty">{t('git.loading')}</p>;
  if (log.isError) return <p className="git-empty">{t('git.timelineEmpty')}</p>;
  const commits = log.data?.commits ?? [];
  if (commits.length === 0) return <p className="git-empty">{t('git.timelineEmpty')}</p>;
  return (
    <div className="git-timeline">
      {commits.map((c) => (
        <Fragment key={c.hash}>
          <TimelineRow
            hash={c.hash}
            subject={c.subject}
            meta={`${c.author} · ${c.relTime}`}
            selected={selected === c.hash}
            copied={copied === c.hash}
            copiedLabel={t('git.copied')}
            copyTitle={t('git.copyHashTitle')}
            onToggle={() => setSelected((cur) => (cur === c.hash ? null : c.hash))}
            onCopy={() => copyHash(c.hash)}
          />
          {selected === c.hash ? (
            <CommitFiles
              repoPath={repoPath}
              hash={c.hash}
              api={api}
              instanceId={instanceId}
              onOpenFile={onOpenFile}
            />
          ) : null}
        </Fragment>
      ))}
      {log.data?.hasMore ? (
        <button type="button" className="git-load-more" onClick={() => setLimit((n) => n + 50)}>
          {t('git.loadMore')}
        </button>
      ) : null}
    </div>
  );
}

/// git-timeline-files F1/F2：单条提交的文件维变更清单。选文件 → onOpenFile 进
/// diff（只读链路）。限量 200 + 「加载更多」递增（F4 大 commit 不卡——名字态
/// 清单轻量，limit+200 内存重取）；F3 多仓：repoPath 由所在分区仓传入，不串仓。
function CommitFiles({
  repoPath,
  hash,
  api,
  instanceId,
  onOpenFile,
}: {
  repoPath: string;
  hash: string;
  api: Api;
  instanceId: string | null;
  onOpenFile: (f: { hash: string; file: string }) => void;
}): ReactElement {
  const t = useI18n();
  const [limit, setLimit] = useState(200);
  const files = useQuery({
    queryKey: ['commitFiles', instanceId, repoPath, hash, limit],
    queryFn: () => api.commitFiles(repoPath, hash, limit),
    staleTime: 30_000,
  });
  if (files.isError) return <p className="git-commit-files-note">{t('git.commitFilesError')}</p>;
  const list = files.data?.files ?? [];
  return (
    <div className="git-commit-files">
      {files.isLoading ? (
        <p className="git-commit-files-note">{t('git.loading')}</p>
      ) : list.length === 0 ? (
        <p className="git-commit-files-note">{t('git.commitNoFiles')}</p>
      ) : (
        <>
          <div className="git-commit-files-title">{t('git.commitFilesN', { n: list.length })}</div>
          {list.map((f) => {
            const sep = f.path.lastIndexOf('/');
            const name = sep >= 0 ? f.path.slice(sep + 1) : f.path;
            const dir = sep >= 0 ? f.path.slice(0, sep) : '';
            return (
              <FileRow
                key={f.path}
                path={f.path}
                name={name}
                dir={dir || null}
                letter={f.status[0] ?? 'M'}
                title={f.origPath ? `${f.origPath} → ${f.path}` : f.path}
                onClick={() => onOpenFile({ hash, file: f.path })}
              />
            );
          })}
          {files.data?.hasMore ? (
            <button type="button" className="git-load-more" onClick={() => setLimit((n) => n + 200)}>
              {t('git.loadMore')}
            </button>
          ) : null}
        </>
      )}
    </div>
  );
}

// —— 单仓内容（分支头 + 变更区双视图；agent-hub-git-timeline T3 参数化分区内容）——

/// git 状态查询结果的最小结构视图（父仓由 GitPanel 注入同 queryKey 的结果供
/// 分区头概要使用；子仓 undefined → 自持查询，懒加载语义不变）
interface StatusView {
  data?: GitStatus;
  isError: boolean;
  isLoading: boolean;
}

function RepoContent({
  repoPath,
  api,
  instanceId,
  onOpenDiff,
  injectedStatus,
  excludePaths,
}: {
  repoPath: string;
  api: Api;
  instanceId: string | null;
  onOpenDiff: Props['onOpenDiff'];
  /// git-sections-polish U3：父仓分区头概要所需——GitPanel 以同 queryKey 持查询并
  /// 注入（折叠时 RepoContent 不挂载，头仍有数据）；子仓不注入（懒加载）
  injectedStatus?: StatusView;
  /// git-submodule-rows G1：排除出变更列表的仓库相对路径集（submodule 条目——
  /// 变更列表=本仓文件变更；子仓状态归各自聚合分区展示）。未传/空集 = 不过滤
  excludePaths?: ReadonlySet<string>;
}): ReactElement {
  const t = useI18n();
  const status = useQuery({
    queryKey: ['gitStatus', instanceId, repoPath],
    queryFn: () => api.gitStatus(repoPath),
    // 非 git 目录（含未初始化子模块）：请求报错 → 分区回落空态，不报错
    refetchInterval: 30_000,
    // 父仓路径由 GitPanel 注入（同 key 去重单请求）——自持查询关闭防双拉
    enabled: injectedStatus === undefined,
  });
  const rawSt = injectedStatus?.data ?? status.data;
  // G1：submodule 条目不作为本仓变更行（git status 把子仓当单行条目——与早期
  // 「目录不单列」定案同源）；计数同步排除（分区头概要/变更 N 均不含子仓）
  const st = rawSt
    ? {
        ...rawSt,
        files: excludePaths && excludePaths.size > 0
          ? rawSt.files.filter((f) => !excludePaths.has(f.path))
          : rawSt.files,
      }
    : undefined;
  // 错误态取注入侧（自持查询在 enabled:false 下恒 false，不能代表父仓真实状态）
  const viewError = injectedStatus ? injectedStatus.isError : status.isError;
  const staged = st?.files.filter((f) => f.x !== ' ' && f.x !== '?') ?? [];
  const unstaged = st?.files.filter((f) => f.y !== ' ' && f.y !== '?' && f.x !== '?') ?? [];
  const untracked = st?.files.filter((f) => f.x === '?' && f.y === '?') ?? [];
  const [view, setView] = useState<GitViewMode>(loadGitView);
  const switchView = (v: GitViewMode): void => {
    setView(v);
    saveGitView(v);
  };
  const fileRow = (f: GitStatusFile, cached: boolean): ReactElement => {
    const mark = markOf(f, cached);
    const kind = statusKind(mark, false);
    const sep = f.path.lastIndexOf('/');
    const name = sep >= 0 ? f.path.slice(sep + 1) : f.path;
    const dir = sep >= 0 ? f.path.slice(0, sep) : '';
    return (
      <FileRow
        key={f.path}
        path={f.path}
        name={name}
        dir={dir || null}
        letter={kind}
        title={f.origPath ? `${f.origPath} → ${f.path}` : f.path}
        onClick={() => onOpenDiff({ cwd: repoPath, file: f.path, cached })}
      />
    );
  };
  const treeNodes = useMemo(() => {
    if (st == null) return [] as TreeNode[];
    const entries: FileEntry[] = [
      ...staged.map((f) => ({ path: f.path, mark: markOf(f, true), cached: true, isSub: false, origPath: f.origPath })),
      ...unstaged.map((f) => ({ path: f.path, mark: markOf(f, false), cached: false, isSub: false, origPath: f.origPath })),
      ...untracked.map((f) => ({ path: f.path, mark: '?', cached: false, isSub: false, origPath: f.origPath })),
    ];
    return buildFileTree(entries);
  }, [st, staged, unstaged, untracked]);

  if (viewError) return <p className="git-empty">{t('proj.gitNotRepo')}</p>;
  return (
    <>
      <div className="git-title">
        Git · {st?.branch ?? '—'}
        {st?.upstream ? <span className="git-upstream"> → {st.upstream}</span> : null}
      </div>
      {st ? (
        <>
          {/* git-changes-meta-dedup M1：独立 meta 摘要行已删除——分支/↑↓/变更计数
              均在「变更」分区头概要（唯一计数入口），此处不再重复同文 */}
          {st.files.length > 0 ? (
            <>
              <div className="git-view-head">
                <span className="git-changes-label">{t('git.changes', { n: st.files.length })}</span>
                <div className="git-view-toggle" role="group" aria-label={t('git.viewToggle')}>
                  <button
                    type="button"
                    className={`git-view-btn ${view === 'flat' ? 'is-active' : ''}`}
                    aria-pressed={view === 'flat'}
                    title={t('git.viewFlat')}
                    onClick={() => switchView('flat')}
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
                      <line x1="4" y1="6" x2="20" y2="6" />
                      <line x1="4" y1="12" x2="20" y2="12" />
                      <line x1="4" y1="18" x2="20" y2="18" />
                    </svg>
                  </button>
                  <button
                    type="button"
                    className={`git-view-btn ${view === 'tree' ? 'is-active' : ''}`}
                    aria-pressed={view === 'tree'}
                    title={t('git.viewTree')}
                    onClick={() => switchView('tree')}
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                      <polyline points="9 18 15 12 9 6" />
                      <line x1="17" y1="6" x2="17" y2="18" />
                    </svg>
                  </button>
                </div>
              </div>
              {view === 'flat' ? (
                <div className="git-files">
                  {staged.length > 0 ? (
                    <>
                      <div className="git-group-title">{t('git.stagedN', { n: staged.length })}</div>
                      {staged.map((f) => fileRow(f, true))}
                    </>
                  ) : null}
                  {unstaged.length > 0 ? (
                    <>
                      <div className="git-group-title">{t('git.unstagedN', { n: unstaged.length })}</div>
                      {unstaged.map((f) => fileRow(f, false))}
                    </>
                  ) : null}
                  {untracked.length > 0 ? (
                    <>
                      <div className="git-group-title">{t('git.untrackedN', { n: untracked.length })}</div>
                      {untracked.map((f) => fileRow(f, false))}
                    </>
                  ) : null}
                </div>
              ) : (
                <div className="git-tree">
                  <TreeRows nodes={treeNodes} depth={0} onOpenDiff={(f) => onOpenDiff({ cwd: repoPath, ...f })} />
                </div>
              )}
            </>
          ) : (
            <p className="git-empty">{t('git.clean')}</p>
          )}
        </>
      ) : (
        <p className="git-empty">{t('git.loading')}</p>
      )}
    </>
  );
}

/// P6 B4-B7 git 面板 → agent-hub-git-timeline 分类化：可折叠分区（变更/Timeline）
/// + 多仓聚合（父仓与 submodule 各得一个分区，单视图总览所有仓；子仓懒加载——
/// 展开才拉数据，不串仓）。worktree 创建入口按只读立场移除（T4：写操作与
/// 「agent 完成所有任务，人只看」冲突；后端端点保留无 UI 消费方，盘点见报告）。
export default function GitPanel({ cwd, api, instanceId, onOpenDiff }: Props): ReactElement {
  const t = useI18n();
  // 子模块清单（父仓视角；T3b 聚合分区的数据源）
  const submodules = useQuery({
    queryKey: ['submodules', instanceId, cwd],
    queryFn: () => api.submodules(cwd),
  });
  // 父仓状态（git-sections-polish U3：变更分区头概要数据源）——与 RepoContent 同
  // queryKey 去重单请求；折叠时 RepoContent 不挂载，此查询保证头部概要常新
  const parentStatus = useQuery({
    queryKey: ['gitStatus', instanceId, cwd],
    queryFn: () => api.gitStatus(cwd),
    refetchInterval: 30_000,
  });
  // 分区折叠持久化（hub_git_sections；缺省展开）
  const [sections, setSections] = useState<Record<string, boolean>>(loadSectionMap);
  const toggle = (key: string): void => {
    setSections((prev) => {
      const next = { ...prev, [key]: !isSectionOpen(prev, key) };
      saveSectionMap(next);
      return next;
    });
  };
  // title：子仓分区悬停可见全相对路径（不同目录同名子仓可辨识）
  // summary：分区头右缘概要（U2 左对齐组名 + 右侧弱化概要，VSCode SCM 惯例）
  const sectionHead = (key: string, label: string, badge: string, title?: string, summary?: string): ReactElement => (
    <button
      type="button"
      className="git-section-head"
      aria-expanded={isSectionOpen(sections, key)}
      title={title}
      onClick={() => toggle(key)}
    >
      <span className={`git-section-arrow ${isSectionOpen(sections, key) ? '' : 'is-collapsed'}`} aria-hidden="true">
        ▸
      </span>
      {label}
      {badge ? <span className="git-section-badge">{badge}</span> : null}
      {summary ? <span className="git-section-summary">{summary}</span> : null}
    </button>
  );
  // 会话切换：分区折叠态保留（同一仓集合记忆有效；跨仓 key 不同天然隔离）
  useEffect(() => {
    setSections(loadSectionMap);
  }, [cwd]);
  // U3 概要：分支 + ahead/behind + 变更计数（收起态信息不丢；数据未到不渲染）。
  // git-submodule-rows G1：计数不含 submodule 行（变更列表=本仓文件变更）
  const subs = submodules.data ?? [];
  const subRelPaths = new Set(subs.map((s) => s.path));
  const pst = parentStatus.data;
  const changesSummary = pst
    ? `${pst.branch ?? '—'}${
        pst.upstream && (pst.ahead > 0 || pst.behind > 0) ? ` ↑${pst.ahead}↓${pst.behind}` : ''
      } · ${pst.files.filter((f) => !subRelPaths.has(f.path)).length}`
    : '';
  /// git-timeline-files F2：提交内文件 → 提交级 diff（hash 上报，App 转交 GitDiffView）
  const openCommitFile = (repoPath: string) => (f: { hash: string; file: string }): void => {
    onOpenDiff({ cwd: repoPath, file: f.file, cached: false, hash: f.hash });
  };
  /// G1/G3：某仓应排除的 submodule 相对路径集（递归清单相对顶层仓——子仓分区
  /// 需剥前缀换算成相对该子仓的路径；父仓即全量）。空集 = 无嵌套子仓不过滤
  const subRelFor = (repoPath: string): ReadonlySet<string> => {
    if (subRelPaths.size === 0) return subRelPaths;
    const prefix = repoPath === cwd ? '' : `${repoPath.slice(cwd.length + 1)}/`;
    return new Set(
      subs
        .filter((s) => s.path.startsWith(prefix))
        .map((s) => s.path.slice(prefix.length)),
    );
  };
  return (
    <div className="git-panel">
      {/* 父仓：变更分区（U3 收进可折叠组，头携概要）+ Timeline 分区 */}
      <div className="git-section">
        {sectionHead(`changes:${cwd}`, t('git.sectionChanges'), '', undefined, changesSummary)}
        {isSectionOpen(sections, `changes:${cwd}`) ? (
          <div className="git-section-body">
            <RepoContent
              repoPath={cwd}
              api={api}
              instanceId={instanceId}
              onOpenDiff={onOpenDiff}
              excludePaths={subRelPaths}
              injectedStatus={{
                data: parentStatus.data,
                isError: parentStatus.isError,
                isLoading: parentStatus.isLoading,
              }}
            />
          </div>
        ) : null}
      </div>
      <div className="git-section">
        {sectionHead(`timeline:${cwd}`, t('git.sectionTimeline'), '')}
        {isSectionOpen(sections, `timeline:${cwd}`) ? (
          <div className="git-section-body">
            <Timeline repoPath={cwd} api={api} instanceId={instanceId} onOpenFile={openCommitFile(cwd)} />
          </div>
        ) : null}
      </div>
      {/* 子仓分区（T3b 多仓聚合）：各含变更 + Timeline；懒加载（展开才 mount） */}
      {subs.map((s) => {
        const subPath = joinPath(cwd, s.path);
        const key = `sub:${subPath}`;
        const badge =
          s.changes > 0 ? `● ${s.changes}` : s.untracked > 0 ? `?${s.untracked}` : '';
        return (
          <div className="git-section" key={key}>
            {sectionHead(key, repoName(s.path), badge, s.path)}
            {isSectionOpen(sections, key) ? (
              <div className="git-section-body">
                <RepoContent repoPath={subPath} api={api} instanceId={instanceId} onOpenDiff={onOpenDiff} excludePaths={subRelFor(subPath)} />
                <Timeline repoPath={subPath} api={api} instanceId={instanceId} onOpenFile={openCommitFile(subPath)} />
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

/// 目录树渲染（按目录视图）：目录行点击折叠/展开（▸ 旋转 + 聚合状态 ● /单字母 +
/// 文件计数），文件行右对齐状态字母（U 绿/D 红删除线/S 灰蓝/M 淡黄），点击交主区 diff。
function TreeRows({
  nodes,
  depth,
  onOpenDiff,
}: {
  nodes: TreeNode[];
  depth: number;
  onOpenDiff: (f: { file: string; cached: boolean }) => void;
}): ReactElement {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const toggle = (p: string): void =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(p)) next.delete(p);
      else next.add(p);
      return next;
    });
  return (
    <>
      {nodes.map((node) =>
        node.kind === 'dir' ? (
          <Fragment key={`d:${node.path}`}>
            <button
              type="button"
              className="git-tree-dir"
              style={{ paddingLeft: `${8 + depth * 12}px` }}
              aria-expanded={!collapsed.has(node.path)}
              onClick={() => toggle(node.path)}
            >
              <span
                className={`git-tree-arrow ${collapsed.has(node.path) ? 'is-collapsed' : ''}`}
                aria-hidden="true"
              >
                ▸
              </span>
              <svg
                width="13"
                height="13"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
              </svg>
              <span className="git-tree-name">{node.name}</span>
              <span className="git-tree-kinds">
                {node.kinds.length > 1 ? '●' : (node.kinds[0] ?? '')}
              </span>
              <span className="git-tree-count">{node.count}</span>
            </button>
            {!collapsed.has(node.path) ? (
              <TreeRows nodes={node.children} depth={depth + 1} onOpenDiff={onOpenDiff} />
            ) : null}
          </Fragment>
        ) : (
          <FileRow
            key={`f:${node.cached ? 'i' : 'w'}:${node.path}`}
            path={node.path}
            name={node.name}
            dir={null}
            letter={node.letter}
            indent={20 + depth * 12}
            deleted={node.letter === 'D'}
            title={node.origPath != null ? `${node.origPath} → ${node.name}` : node.path}
            onClick={() => onOpenDiff({ file: node.path, cached: node.cached })}
          />
        ),
      )}
    </>
  );
}
