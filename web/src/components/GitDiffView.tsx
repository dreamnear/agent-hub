import { Fragment, useEffect, useState, type ReactElement } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { Api } from '../api';
import { resourceTokenQuery } from '../api';
import { useI18n } from '../i18n';
import './GitDiffView.css';

interface Props {
  /// 会话工作路径（diff 查询基准）
  cwd: string;
  /// 会话所属实例的 api（本机=api，远端=实例代理）
  api: Api;
  /// git-timeline-files：hash 非空 = 提交内文件 diff（api.commitDiff，diff-tree 只读）；
  /// 缺省 = 工作区 diff（api.gitDiff 既有语义不变）
  file: { file: string; cached: boolean; hash?: string | null };
  /// 对话框形态关闭回调；embedded（F4 v2 diff 页签）下关闭走页签条，可缺省
  onClose?: () => void;
  /// agent-hub-files-panel F3 跳源文件：参数 = 仓库层绝对路径（cwd + 相对路径）。
  /// 调用方（App）收 diff + 开右面板文件树 + 主区渲染该文件；未传不渲染跳源钮
  onOpenSource?: (absPath: string) => void;
  /// embedded（F4 v2 diff 页签化）：分区内嵌形态——去遮罩定位/对话框语义/关闭钮
  /// （关闭由页签条承担），diff 内容渲染行为不变
  embedded?: boolean;
}

// —— GitLab 式左右对照解析：unified diff → 行对齐矩阵 ——

export interface DiffCell {
  no: number | null;
  text: string;
  kind: 'add' | 'del' | 'ctx' | 'empty';
}

export type DiffRow =
  | { kind: 'hunk'; text: string }
  | { kind: 'meta'; text: string }
  | { kind: 'pair'; left: DiffCell; right: DiffCell };

const EMPTY: DiffCell = { no: null, text: '', kind: 'empty' };

const cell = (no: number, text: string, kind: DiffCell['kind']): DiffCell => ({
  no: no > 0 ? no : null, // 0 = hunk 头缺失/畸形，行号未知
  text,
  kind,
});

/** unified diff → 左右行对照矩阵：context 双侧同码；纯增右格有码左格空占位；
 * 纯删反之；@@ 成横跨双栏的 hunk 带；\ No newline 成 meta 行；
 * 首个 @@ 之前的文件头（diff --git/index/---/+++）不渲染——尤其 ---/+++ 不得误判
 * 为删/增行（文件路径已在标题）。 */
export function parseSplitDiff(text: string): DiffRow[] {
  const rows: DiffRow[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  for (const ln of text.split('\n')) {
    if (ln.startsWith('@@')) {
      inHunk = true;
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(ln);
      if (m) {
        oldNo = Number(m[1]);
        newNo = Number(m[2]);
      }
      rows.push({ kind: 'hunk', text: ln });
    } else if (!inHunk) {
      continue; // 文件头元信息行
    } else if (ln.startsWith('+')) {
      rows.push({ kind: 'pair', left: EMPTY, right: cell(newNo, ln.slice(1), 'add') });
      newNo += 1;
    } else if (ln.startsWith('-')) {
      rows.push({ kind: 'pair', left: cell(oldNo, ln.slice(1), 'del'), right: EMPTY });
      oldNo += 1;
    } else if (ln.startsWith(' ')) {
      rows.push({
        kind: 'pair',
        left: cell(oldNo, ln.slice(1), 'ctx'),
        right: cell(newNo, ln.slice(1), 'ctx'),
      });
      oldNo += 1;
      newNo += 1;
    } else if (ln.startsWith('\\')) {
      rows.push({ kind: 'meta', text: ln });
    }
  }
  return rows;
}

/// Git diff 主区覆盖视图（GitLab split 版）：覆盖中央会话区（pane-main 相对定位锚），
/// 文件路径标题 + 关闭钮/ESC 关闭。单容器网格双栏——横向滚动天然同步，勿拆两个滚动容器。
/// 覆盖层卸载即回原视图，聊天状态不受影响（ChatTab 保持挂载）。
export default function GitDiffView({
  cwd,
  api,
  file,
  onClose,
  onOpenSource,
  embedded = false,
}: Props): ReactElement {
  const t = useI18n();
  const diff = useQuery({
    queryKey: ['gitDiff', cwd, file.file, file.cached, file.hash ?? null],
    queryFn: () =>
      file.hash ? api.commitDiff(cwd, file.hash, file.file) : api.gitDiff(cwd, file.file, file.cached),
    // diff 请求失败是确定性结果（不存在/二进制等），重试只会拖长加载态
    retry: false,
  });

  // ESC 关闭——仅对话框形态挂 window 监听。embedded（页签形态）面板随 Tab 组
  // 全组常挂（隐藏态监听仍注册），若在此挂监听，多会话各开 diff 后单次 ESC 会
  // 批量误关所有会话的 diff（reviewer 终审 BLOCKER）——页签形态的 ESC 由 App
  // 全局层按「当前聚焦组」收敛处理
  useEffect(() => {
    if (embedded) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose?.();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- onClose 由父组件箭头函数持有，语义稳定
  }, [onClose, embedded]);

  const binary = diff.data?.binary === true;
  const untracked = diff.data?.untracked === true;
  const text = diff.data?.diff;
  const rows = text != null && !binary ? parseSplitDiff(text) : null;
  // 图片对照（diff 热修追加）：binary+图片扩展 → 左旧右新双窗格。
  // img src 直指 file 端点（SVG 走 <img> 不 inline，防脚本注入）。
  // 提交内文件（hash）不适用 rev=head/work 语义 → 走 binary 占位
  const isImage = file.hash == null && /\.(png|jpe?g|gif|svg|webp|bmp)$/i.test(file.file);
  const fileUrl = (rev: 'head' | 'work'): string => {
    const params = new URLSearchParams({
      path: cwd,
      file: file.file,
      rev,
    });
    return `${api.baseAppUrl}/api/projects/file?${params}${resourceTokenQuery()}`;
  };
  const [headImgFailed, setHeadImgFailed] = useState(false);
  useEffect(() => {
    setHeadImgFailed(false); // 换文件重置旧图失败态
  }, [file.file]);

  // 单占位态（错误/加载/无变更/二进制）：body 切居中布局（验收反馈：错误文案
  // 曾贴网格左上角）；diff 行/图片对照仍走网格
  const emptyState =
    diff.isError || (binary && !isImage) || rows == null || rows.length === 0;

  const head = (
      <div className="git-diff-overlay-head">
        <span className="git-diff-overlay-title">
          {file.hash
            ? `${t('git.commitPrefix')} ${file.hash} · ${file.file}`
            : `${file.cached ? t('git.staged') : t('git.worktree')} · ${file.file}`}
        </span>
        {/* F3 跳源文件：收 diff + 右面板文件树 + 主区渲染（App 接线；二进制无源渲染跳转无意义，隐藏） */}
        {onOpenSource != null && !binary ? (
          <button
            type="button"
            className="git-diff-open-source"
            title={t('git.openSource')}
            onClick={() => onOpenSource(`${cwd}/${file.file}`)}
          >
            <span aria-hidden="true">⇲</span>
            <span className="git-diff-open-source-text">{t('git.openSource')}</span>
          </button>
        ) : null}
        {!embedded ? (
          <button
            type="button"
            className="git-diff-close"
            onClick={onClose}
            aria-label={t('git.closeDiff')}
          >
            ×
          </button>
        ) : null}
      </div>
  );
  const body = (
    <div className={`git-diff-overlay-body${emptyState ? ' is-empty-state' : ''}`}>
        {diff.isError ? (
          // 错误态必须显式渲染：缺这里出错后加载转圈永不消失（热修工单实证）
          <p className="git-empty">
            {t('git.diffError')}
            {diff.error != null ? `（${String(diff.error)}）` : ''}
          </p>
        ) : binary && isImage ? (
          // 图片对照：左旧（rev=head；新文件无旧版占位）右新（rev=work），各按容器缩放
          <div className="git-img-compare">
            <figure className="git-img-pane">
              <figcaption>{t('git.old')}</figcaption>
              {untracked || headImgFailed ? (
                <p className="git-empty">{t('git.noOld')}</p>
              ) : (
                <img
                  src={fileUrl('head')}
                  className="git-img"
                  alt={t('git.old')}
                  onError={() => setHeadImgFailed(true)}
                />
              )}
            </figure>
            <figure className="git-img-pane">
              <figcaption>{t('git.new')}</figcaption>
              <img src={fileUrl('work')} className="git-img" alt={t('git.new')} />
            </figure>
          </div>
        ) : binary ? (
          <p className="git-empty">{t('git.binaryFile')}</p>
        ) : rows == null ? (
          <p className="git-empty">{t('git.loading')}</p>
        ) : rows.length === 0 ? (
          <p className="git-empty">{t('git.noDiff')}</p>
        ) : (
          rows.map((row, i) =>
            row.kind === 'pair' ? (
              <Fragment key={i}>
                <span className="git-diff-no">{row.left.no ?? ''}</span>
                <span className={`git-diff-cell is-${row.left.kind}`}>
                  {row.left.text === '' ? ' ' : row.left.text}
                </span>
                <span className="git-diff-no">{row.right.no ?? ''}</span>
                <span className={`git-diff-cell is-${row.right.kind}`}>
                  {row.right.text === '' ? ' ' : row.right.text}
                </span>
              </Fragment>
            ) : (
              <div key={i} className={row.kind === 'hunk' ? 'git-diff-hunk' : 'git-diff-meta'}>
                {row.text}
              </div>
            ),
          )
        )}
      </div>
  );

  if (embedded) {
    // F4 v2 diff 页签形态：pane-slot 内 flex 填充，去遮罩定位与对话框语义
    return (
      <div className="git-diff-tab" aria-label={t('git.closeDiff')}>
        {head}
        {body}
      </div>
    );
  }
  return (
    <div className="git-diff-overlay" role="dialog" aria-label={t('git.closeDiff')}>
      {head}
      {body}
    </div>
  );
}
