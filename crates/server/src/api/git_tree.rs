//! git 工程树（P6 B1-B3）：注册工程/agent cwd 的 git 探测 → 主仓/worktree 归组，
//! 以及打开本地目录（Finder）——仅有的两个非纯展示行为之一，open 有路径白名单硬校验。

use std::path::{Path, PathBuf};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{error::AppError, projects::ProjectsStore};

use super::SharedState;

/// open-dir 统一拒绝文案：不区分不存在/白名单外，消除路径存在性探测信号（r40 观察）
const DENY_MSG: &str = "路径不在注册工程白名单内";

/// 树节点：主仓 / worktree / 非 git 目录（isGit=false 时仅 path/name 有意义）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitTreeNode {
    pub path: String,
    pub name: String,
    pub is_main: bool,
    pub is_git: bool,
    /// 短分支名（refs/heads/x → x）；detached HEAD 为 None
    pub branch: Option<String>,
    /// 短 commit sha（8 位）
    pub head: Option<String>,
}

/// 一棵树：主仓 + 其 linked worktrees（无 worktree 时空数组，前端平铺）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitTreeGroup {
    pub main: GitTreeNode,
    pub worktrees: Vec<GitTreeNode>,
}

/// `git worktree list --porcelain` 单条：worktree 行必有，HEAD/branch 行可选（detached 无 branch）。
#[derive(Debug, PartialEq)]
struct WorktreeEntry {
    path: String,
    head: Option<String>,
    branch: Option<String>,
}

fn parse_worktree_list(raw: &str) -> Vec<WorktreeEntry> {
    let mut out: Vec<WorktreeEntry> = Vec::new();
    for line in raw.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.push(WorktreeEntry {
                path: p.trim().to_string(),
                head: None,
                branch: None,
            });
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            if let Some(last) = out.last_mut() {
                last.head = Some(h.trim().to_string());
            }
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(last) = out.last_mut() {
                let short = b.trim().strip_prefix("refs/heads/").unwrap_or(b.trim());
                last.branch = Some(short.to_string());
            }
        }
    }
    out
}

fn node(
    path: &str,
    is_main: bool,
    is_git: bool,
    head: Option<&str>,
    branch: Option<&str>,
) -> GitTreeNode {
    GitTreeNode {
        path: path.to_string(),
        name: Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string(),
        is_main,
        is_git,
        branch: branch.map(str::to_string),
        head: head.map(|h| h.chars().take(8).collect()),
    }
}

/// `git -C <cwd> worktree list --porcelain`（5s 超时；非 git 仓/失败 → None）。
async fn git_worktree_list(cwd: &Path) -> Option<Vec<WorktreeEntry>> {
    let output = tokio::process::Command::new("git")
        .args(["-C", cwd.to_str()?, "worktree", "list", "--porcelain"])
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), output)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_worktree_list(&String::from_utf8_lossy(&out.stdout)))
}

fn group_from(list: &[WorktreeEntry]) -> Option<GitTreeGroup> {
    let main = list.first()?;
    Some(GitTreeGroup {
        main: node(
            &main.path,
            true,
            true,
            main.head.as_deref(),
            main.branch.as_deref(),
        ),
        worktrees: list[1..]
            .iter()
            .map(|w| node(&w.path, false, true, w.head.as_deref(), w.branch.as_deref()))
            .collect(),
    })
}

/// 探测候选 = 注册工程（projects.json）+ 当前 agent cwd（B1 数据源）；
/// 已不存在的目录（agent 死亡/注册项被删）直接剔除，不上树。
/// driver-unify 2.7：agent 清单经 DriverRegistry 取（数据源保持批前口径：
/// 仅 claude 侧——ACP 会话 cwd 不进 git 树，行为等价优先）。
async fn candidate_cwds(state: &SharedState) -> Vec<String> {
    let mut cwds: Vec<String> = ProjectsStore::new(&state.cfg).load().await;
    if let Ok(d) = state.drivers.get("claude") {
        if let Ok(agents) = d.list(true).await {
            for a in agents {
                if let Some(cwd) = a.cwd {
                    cwds.push(cwd);
                }
            }
        }
    }
    cwds.sort();
    cwds.dedup();
    let mut out = Vec::new();
    for c in cwds {
        let is_dir = tokio::fs::metadata(&c)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            out.push(c);
        }
    }
    out
}

/// B1+B2：候选 cwd 逐个 git 探测 → 归组（同主仓只出一棵树，porcelain 自带全量 worktrees）；
/// 非 git 目录平铺为单节点组。
async fn build_tree_uncached(state: &SharedState) -> Vec<GitTreeGroup> {
    let mut groups: Vec<GitTreeGroup> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cwd in candidate_cwds(state).await {
        if seen.contains(&cwd) {
            continue;
        }
        match git_worktree_list(Path::new(&cwd)).await {
            Some(list) if !list.is_empty() => {
                // porcelain 首条恒主仓；同主仓的多个注册 cwd 只出一棵树
                if seen.insert(list[0].path.clone()) {
                    if let Some(g) = group_from(&list) {
                        groups.push(g);
                    }
                }
            }
            _ => {
                if seen.insert(cwd.clone()) {
                    groups.push(GitTreeGroup {
                        main: node(&cwd, true, false, None, None),
                        worktrees: Vec::new(),
                    });
                }
            }
        }
    }
    groups
}

/// 缓存 TTL：对齐前端工程栏 30s 轮询，探测成本（N 工程 × git 子进程）摊平（MINOR-2）
const TREE_TTL: std::time::Duration = std::time::Duration::from_secs(15);

/// 带缓存建树（P6 批次 3）：15s TTL 命中直接返回；写操作后调用 invalidate_tree_cache。
pub async fn build_tree(state: &SharedState) -> Vec<GitTreeGroup> {
    {
        let cache = state.tree_cache.read().await;
        if let Some((at, tree)) = cache.as_ref() {
            if at.elapsed() < TREE_TTL {
                return tree.clone();
            }
        }
    }
    let tree = build_tree_uncached(state).await;
    *state.tree_cache.write().await = Some((std::time::Instant::now(), tree.clone()));
    tree
}

/// 树缓存失效（worktree add 等写操作后调用）。
async fn invalidate_tree_cache(state: &SharedState) {
    *state.tree_cache.write().await = None;
}

/// open-dir 白名单：注册工程 ∪ agent cwd ∪ git 树节点（注册 worktree 的主仓/兄弟
/// worktree 属同一仓库群，树 UI 可见即应可打开）。全部 canonicalize 后比较。
///
/// 安全边界（review-r1 MINOR-1）：树节点路径派生自 `git worktree list` 输出，恶意仓库
/// 可伪造 worktree 元数据扩充白名单——当前 open-dir 仅本机 Finder 弹窗，增量风险≈0；
/// 若未来本端点升级为回传目录内容/读文件类行为，白名单必须收敛为仅注册项 ∪ 主仓
/// （diff 端点已属此类：root 内部 symlink 未消解，file 可经 symlink 读白名单外文件，
/// 依赖「已认证=本人」信任；升级时 symlink 须一并消解）。
pub(crate) async fn allowed_roots(state: &SharedState) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = candidate_cwds(state)
        .await
        .into_iter()
        .map(PathBuf::from)
        .collect();
    for g in build_tree(state).await {
        roots.push(PathBuf::from(g.main.path));
        for w in g.worktrees {
            roots.push(PathBuf::from(w.path));
        }
    }
    let mut canon: Vec<PathBuf> = Vec::new();
    for r in roots {
        if let Ok(c) = tokio::fs::canonicalize(&r).await {
            canon.push(c);
        }
    }
    canon.sort();
    canon.dedup();
    canon
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/projects/tree", axum::routing::get(projects_tree))
        .route("/api/projects/open-dir", axum::routing::post(open_dir))
        .route("/api/projects/git-status", axum::routing::get(git_status))
        .route("/api/projects/git-log", axum::routing::get(git_log))
        .route(
            "/api/projects/git-commit-files",
            axum::routing::get(git_commit_files),
        )
        .route(
            "/api/projects/git-commit-diff",
            axum::routing::get(git_commit_diff),
        )
        .route("/api/projects/diff", axum::routing::get(git_diff))
        .route("/api/projects/file", axum::routing::get(project_file))
        .route(
            "/api/projects/worktree",
            axum::routing::post(create_worktree),
        )
        .route(
            "/api/projects/submodules",
            axum::routing::get(list_submodules),
        )
}

/// 树内路径白名单命中判定（canonicalize 过的目标 vs 全部 canonicalize 过的根）。
async fn path_allowed(state: &SharedState, target: &Path) -> bool {
    allowed_roots(state)
        .await
        .iter()
        .any(|root| target == *root || target.starts_with(root))
}

/// 树内路径白名单校验（open-dir 同源）：canonicalize 消解后 == 或前缀命中任一根。
/// 统一 403 DENY_MSG（不存在/白名单外不可区分）。
pub(crate) async fn ensure_tree_path(state: &SharedState, raw: &str) -> Result<PathBuf, AppError> {
    if raw.contains("..") {
        return Err(AppError::forbidden(DENY_MSG));
    }
    let target = match tokio::fs::canonicalize(raw).await {
        Ok(t) if t.is_dir() => t,
        _ => return Err(AppError::forbidden(DENY_MSG)),
    };
    if !path_allowed(state, &target).await {
        return Err(AppError::forbidden(DENY_MSG));
    }
    Ok(target)
}

/// diff/file 的 file 参数与 git-status 行同源——porcelain v1 行路径恒为仓库根
/// （toplevel）相对且覆盖全仓，故先把节点目录解析到 toplevel 再拼接（节点即
/// toplevel 时行为不变，修复 cwd≠仓库根 agent 的双重前缀 400）。toplevel 是节点
/// 祖先、可能超出白名单：canonicalize 后复检，不通过 → 403（读面不扩大）；
/// 非 git 目录（rev-parse 失败）→ 回退节点目录（fail-soft，本地 no-index 行为不变）。
async fn resolve_repo_root(state: &SharedState, node: &Path) -> Result<PathBuf, AppError> {
    let out = tokio::process::Command::new("git")
        .args([
            "-C",
            node.to_str().unwrap_or_default(),
            "rev-parse",
            "--show-toplevel",
        ])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), out).await {
        Ok(Ok(out)) if out.status.success() => out,
        _ => return Ok(node.to_path_buf()),
    };
    let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let canon = tokio::fs::canonicalize(&top)
        .await
        .map_err(|_| AppError::forbidden(DENY_MSG))?;
    if !path_allowed(state, &canon).await {
        return Err(AppError::forbidden(DENY_MSG));
    }
    Ok(canon)
}

/// B5+B6：`git -C <path> status --porcelain=v1 -b`——首行分支/upstream/ahead/behind，
/// 其余行 XY path（X=索引态 Y=工作区态，`??`=未跟踪）。分组展示由前端按 x/y 判定。
async fn git_status(
    State(state): State<SharedState>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<GitStatusDto>, AppError> {
    let target = ensure_tree_path(&state, &q.path).await?;
    let output = tokio::process::Command::new("git")
        .args([
            "-C",
            target.to_str().unwrap_or_default(),
            "status",
            "--porcelain=v1",
            "-b",
            // agent-hub-git-flat-dirs：展开未跟踪目录为逐文件（默认折叠 `?? dir/`
            // 会以目录条目混进文件列表，点击无法生成 diff）；已跟踪行为不变
            "--untracked-files=all",
        ])
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), output)
        .await
        .ok()
        .and_then(|r| r.ok())
        .ok_or_else(|| AppError::bad("git status 执行失败"))?;
    if !out.status.success() {
        return Err(AppError::bad(format!(
            "git status 失败: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
        )));
    }
    Ok(Json(parse_status(&String::from_utf8_lossy(&out.stdout))))
}

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    pub path: String,
}

// ---- git-timeline：提交历史（只读 git log，T5 只读红线——零写操作）----

/// Timeline 查询：path 白名单校验同 status；limit 初始 50（前端「加载更多」递增），
/// skip 递增分页；hasMore = 多取 1 条判定（limit clamp 1..=200）。
#[derive(Debug, Deserialize)]
pub struct LogQuery {
    pub path: String,
    pub limit: Option<u32>,
    #[serde(default)]
    pub skip: u32,
}

fn default_log_limit() -> u32 {
    50
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDto {
    /// 短 hash（%h）
    pub hash: String,
    pub author: String,
    /// 相对时间（%ar，如「3 days ago」）
    pub rel_time: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLogDto {
    pub commits: Vec<GitCommitDto>,
    /// 后端还有更早提交（前端「加载更多」按钮显隐）
    pub has_more: bool,
}

/// 只读 `git log --pretty=format:%h%x1f%an%x1f%ar%x1f%s`：单行内 \x1f 分四列，
/// 规避 subject 含管道符/制表符的解析歧义。空仓库（无 commit）→ 空列表非错误。
async fn git_log(
    State(state): State<SharedState>,
    Query(q): Query<LogQuery>,
) -> Result<Json<GitLogDto>, AppError> {
    let target = ensure_tree_path(&state, &q.path).await?;
    let limit = q.limit.unwrap_or_else(default_log_limit).clamp(1, 200);
    let out = git_output(
        target.as_path(),
        &[
            "log",
            "--pretty=format:%h%x1f%an%x1f%ar%x1f%s",
            "--max-count",
            &(limit + 1).to_string(),
            "--skip",
            &q.skip.to_string(),
        ],
    )
    .await?;
    if !out.success {
        // 空仓库：分支存在但无任何 commit → 空时间线非错误
        if out.stderr.contains("does not have any commits yet") {
            return Ok(Json(GitLogDto {
                commits: Vec::new(),
                has_more: false,
            }));
        }
        return Err(AppError::bad(format!(
            "git log 失败: {}",
            out.stderr.lines().last().unwrap_or("")
        )));
    }
    let mut commits: Vec<GitCommitDto> = out
        .stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.split('\u{1f}');
            GitCommitDto {
                hash: it.next().unwrap_or_default().into(),
                author: it.next().unwrap_or_default().into(),
                rel_time: it.next().unwrap_or_default().into(),
                subject: it.next().unwrap_or_default().into(),
            }
        })
        .collect();
    let has_more = commits.len() > limit as usize;
    commits.truncate(limit as usize);
    Ok(Json(GitLogDto { commits, has_more }))
}

// ---- git-timeline-files：提交的文件维变更（F1-F5，只读红线）----

/// hash 白名单：短/全 hex（防 `-` 开头被 git 解析成 flag——参数虽数组传递无 shell
/// 注入，但 `--upload-pack=<cmd>` 类 flag 注入同理须杜绝）。
fn valid_commit_hash(h: &str) -> bool {
    (7..=40).contains(&h.len()) && h.chars().all(|c| c.is_ascii_hexdigit())
}

#[derive(Debug, Deserialize)]
pub struct CommitFilesQuery {
    pub path: String,
    pub hash: String,
    /// 文件数限量（缺省 200；大 commit F4——名字态清单廉价，全量后内存截断即可）
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitFileDto {
    /// diff-tree 名字态字母（A/M/D/T/R/C；R/C 带相似度分数如 R100）
    pub status: String,
    pub path: String,
    /// 重命名/复制原路径（R/C 条目才有）
    pub orig_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitFilesDto {
    pub files: Vec<CommitFileDto>,
    /// 截断后仍有更多（前端「加载更多」递增 limit 重取）
    pub has_more: bool,
}

/// 只读 `git diff-tree --no-commit-id --name-status -r`：单条提交的文件维变更清单。
/// `-r` 递归目录；`--root` 根提交按空树比对；`-M` 改名探测（R/C 双路径）；
/// `-m --first-parent` 合并提交取第一父差异（非合并提交为 no-op）。
async fn git_commit_files(
    State(state): State<SharedState>,
    Query(q): Query<CommitFilesQuery>,
) -> Result<Json<CommitFilesDto>, AppError> {
    if !valid_commit_hash(&q.hash) {
        return Err(AppError::bad("hash 须为 7-40 位十六进制"));
    }
    let target = ensure_tree_path(&state, &q.path).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 2000);
    let out = git_output(
        target.as_path(),
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "-r",
            "--root",
            "-M",
            "-m",
            "--first-parent",
            &q.hash,
        ],
    )
    .await?;
    if !out.success {
        return Err(AppError::bad(format!(
            "git diff-tree 失败: {}",
            out.stderr.lines().last().unwrap_or("")
        )));
    }
    let mut files: Vec<CommitFileDto> = Vec::new();
    for line in out.stdout.lines().filter(|l| !l.is_empty()) {
        let mut it = line.split('\t');
        let status = it.next().unwrap_or_default().to_string();
        let (path, orig_path) = match (it.next(), it.next()) {
            // R/C（改名/复制）：原路径 → 新路径
            (Some(orig), Some(new)) => (new.to_string(), Some(orig.to_string())),
            (Some(p), None) => (p.to_string(), None),
            _ => continue, // 畸形行跳过
        };
        files.push(CommitFileDto {
            status,
            path,
            orig_path,
        });
    }
    let has_more = files.len() > limit as usize;
    files.truncate(limit as usize);
    Ok(Json(CommitFilesDto { files, has_more }))
}

#[derive(Debug, Deserialize)]
pub struct CommitDiffQuery {
    pub path: String,
    pub hash: String,
    pub file: String,
}

/// 只读单文件提交 diff：`git diff-tree -p <hash> -- <file>`（flag 集同文件清单，
/// 保证两者对同一提交的口径一致：根提交/改名/合并第一父）。file 校验同 git_diff。
/// 走既有 DiffDto（untracked 恒 false——提交内文件必然 tracked）。
async fn git_commit_diff(
    State(state): State<SharedState>,
    Query(q): Query<CommitDiffQuery>,
) -> Result<Json<DiffDto>, AppError> {
    if !valid_commit_hash(&q.hash) {
        return Err(AppError::bad("hash 须为 7-40 位十六进制"));
    }
    if q.file.contains("..") || q.file.starts_with('/') {
        return Err(AppError::bad("file 必须是仓库内相对路径"));
    }
    let target = ensure_tree_path(&state, &q.path).await?;
    let out = git_output(
        target.as_path(),
        &[
            "diff-tree",
            "-p",
            "--no-commit-id",
            "--root",
            "-M",
            "-m",
            "--first-parent",
            &q.hash,
            "--",
            &q.file,
        ],
    )
    .await?;
    if !out.success {
        return Err(AppError::bad(format!(
            "git diff-tree 失败: {}",
            out.stderr.lines().last().unwrap_or("")
        )));
    }
    let diff = out.stdout;
    let binary = diff.starts_with("Binary files") || diff.contains("\nBinary files");
    Ok(Json(DiffDto {
        diff: if binary { String::new() } else { diff },
        untracked: false,
        binary,
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusDto {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<StatusFile>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusFile {
    /// 索引态（staged）；' ' = 无
    pub x: String,
    /// 工作区态（unstaged）；' ' = 无；untracked 文件 x='?' y='?'
    pub y: String,
    pub path: String,
    /// 重命名的原路径（R 状态 `old -> new`）
    pub orig_path: Option<String>,
}

fn parse_status(raw: &str) -> GitStatusDto {
    let mut dto = GitStatusDto {
        branch: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        files: Vec::new(),
    };
    for (i, line) in raw.lines().enumerate() {
        if i == 0 && line.starts_with("## ") {
            let head = &line[3..];
            let head = head.split(" [").next().unwrap_or(head); // 去掉 [ahead/behind] 尾巴
            let mut parts = head.split("...");
            dto.branch = parts
                .next()
                .filter(|b| !b.is_empty() && *b != "HEAD (no branch)")
                .map(str::to_string);
            dto.upstream = parts.next().map(str::to_string);
            if let Some(meta) = line.split("[").nth(1) {
                let meta = meta.trim_end_matches(']');
                for kv in meta.split(',') {
                    let kv = kv.trim();
                    if let Some(n) = kv.strip_prefix("ahead ") {
                        dto.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = kv.strip_prefix("behind ") {
                        dto.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let x = &line[0..1];
        let y = &line[1..2];
        let mut rest = &line[3..];
        let mut orig_path = None;
        if let Some(idx) = rest.find(" -> ") {
            orig_path = Some(rest[..idx].to_string());
            rest = &rest[idx + 4..];
        }
        let path = rest.trim_matches('"').to_string();
        dto.files.push(StatusFile {
            x: x.to_string(),
            y: y.to_string(),
            path,
            orig_path,
        });
    }
    dto
}

/// B6 只读 diff：tracked 文件走 `git diff [--cached] -- <file>`；未跟踪文件走
/// `git diff --no-index -- /dev/null <abs>`（全文即新增内容）。file 为仓库根相对
/// 路径（与 git-status porcelain 行同源，经 resolve_repo_root 统一解析拼接）；
/// `..`/首 `/` 拒绝防穿越。
async fn git_diff(
    State(state): State<SharedState>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<DiffDto>, AppError> {
    let node = ensure_tree_path(&state, &q.path).await?;
    let root = resolve_repo_root(&state, &node).await?;
    if q.file.contains("..") || q.file.starts_with('/') {
        return Err(AppError::bad("file 必须是节点内相对路径"));
    }
    let abs = root.join(&q.file);
    // 「git 跟踪」判定用 ls-files 而非文件存在——存在但未跟踪的文件走 no-index 全文；
    // staged 删除（git rm）后索引已无此文件，补 HEAD 探测（cat-file -e）识别「git
    // 仍知晓」，否则 cached 请求会把 staged 删除误判成 untracked 打进 no-index。
    let tracked = git_succeeds(&root, &["ls-files", "--error-unmatch", &q.file]).await;
    let known =
        tracked || git_succeeds(&root, &["cat-file", "-e", &format!("HEAD:{}", q.file)]).await;
    let cached = q.cached.unwrap_or(false);
    // 已删除文件（git 仍知晓但工作区不存在，非 cached 请求）：git diff 本身是空输出，
    // 须从 HEAD 取提交版内容出全删 diff（左全文/右空，与 A/U 镜像；前端 split 视图
    // 天然左 del / 右 empty 渲染）。cached 请求仍读索引原语义。
    let deleted = known && !cached && !abs.is_file();
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(&root);
    if deleted {
        cmd.arg("diff").arg("HEAD").arg("--").arg(&q.file);
    } else if known {
        if cached {
            cmd.arg("diff").arg("--cached");
        } else {
            cmd.arg("diff");
        }
        cmd.arg("--").arg(&q.file);
    } else {
        // 未跟踪文件：no-index 与 /dev/null 比较给出全文
        cmd.arg("diff")
            .arg("--no-index")
            .arg("--")
            .arg("/dev/null")
            .arg(&abs);
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output())
        .await
        .ok()
        .and_then(|r| r.ok())
        .ok_or_else(|| AppError::bad("git diff 执行失败"))?;
    // no-index 对有差异文件返回退出码 1，属正常；stdout 空 + 失败才是真失败
    if !out.status.success() && out.stdout.is_empty() {
        return Err(AppError::bad("git diff 失败"));
    }
    let diff = String::from_utf8_lossy(&out.stdout).to_string();
    // 二进制文件：git 只给「Binary files ... differ」占位文本（无可用内容），
    // 标记 binary 让前端出专用占位，不把垃圾文本行渲染进 diff 视图
    let binary = diff.starts_with("Binary files") || diff.contains("\nBinary files");
    Ok(Json(DiffDto {
        diff: if binary { String::new() } else { diff },
        untracked: !known,
        binary,
    }))
}

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub path: String,
    pub file: String,
    pub cached: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffDto {
    pub diff: String,
    /// 未跟踪文件（no-index 全文添加 diff）
    pub untracked: bool,
    /// 二进制文件（无文本 diff，前端出专用占位）
    pub binary: bool,
}

/// 图片扩展名 → Content-Type（file 端点仅服务图片：最小暴露面，文本走 diff）。
fn image_mime(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub path: String,
    pub file: String,
    /// head = `git show HEAD:<file>`；work = 工作区文件直读（缺省 work）
    pub rev: Option<String>,
}

/// 图片内容只读端点（diff 热修追加：binary+图片 → 左右对照）：rev=head 走
/// `git show`，rev=work 直读工作区；路径过同一白名单，仅服务图片扩展（最小面）。
/// file 为仓库根相对路径（与 git-status 行同源，经 resolve_repo_root 统一解析）。
async fn project_file(
    State(state): State<SharedState>,
    Query(q): Query<FileQuery>,
) -> Result<Response, AppError> {
    let node = ensure_tree_path(&state, &q.path).await?;
    let root = resolve_repo_root(&state, &node).await?;
    if q.file.contains("..") || q.file.starts_with('/') {
        return Err(AppError::bad("file 必须是节点内相对路径"));
    }
    let Some(ct) = image_mime(&q.file) else {
        return Err(AppError::bad("仅支持图片文件（png/jpg/gif/svg/webp/bmp）"));
    };
    let bytes = if q.rev.as_deref() == Some("head") {
        let out = tokio::process::Command::new("git")
            .args([
                "-C",
                root.to_str().unwrap_or_default(),
                "show",
                &format!("HEAD:{}", q.file),
            ])
            .output();
        let out = tokio::time::timeout(std::time::Duration::from_secs(5), out)
            .await
            .ok()
            .and_then(|r| r.ok())
            .ok_or_else(|| AppError::bad("git show 执行失败"))?;
        if !out.status.success() {
            return Err(AppError::bad("旧版本不存在（新文件或未提交）"));
        }
        out.stdout
    } else {
        tokio::fs::read(root.join(&q.file))
            .await
            .map_err(|e| AppError::bad(format!("工作区文件读取失败: {e}")))?
    };
    Ok(([(axum::http::header::CONTENT_TYPE, ct)], bytes).into_response())
}

/// porcelain 输出 → (变更文件数, 未跟踪数)：`??` 计未跟踪，其余行计变更。
fn count_porcelain(out: &str) -> (u32, u32) {
    let mut changes = 0u32;
    let mut untracked = 0u32;
    for line in out.lines() {
        if line.len() < 2 {
            continue;
        }
        match &line[..2] {
            "??" => untracked += 1,
            _ => changes += 1,
        }
    }
    (changes, untracked)
}

/// B7 submodule 清单：`git submodule status --recursive`——前缀标记（空格=同步，
/// - 未初始化，+ SHA 漂移，U 冲突）+ sha + 相对路径。
///
/// 并为每个子模块补轻量状态摘要（行内显改动徽标）：对子模块跑
/// `git status --porcelain` 计变更/未跟踪数——并发上限 4 防 30+ 子模块打爆；
/// 未初始化（'-'，目录未检出必失败）与含 ".." 的路径跳过探测；失败 → 0（fail-soft）。
///
/// 深层详情仍由前端按需拉取 git-status 递归展开（钻取语义不变）。
async fn list_submodules(
    State(state): State<SharedState>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<Vec<SubmoduleInfo>>, AppError> {
    let target = ensure_tree_path(&state, &q.path).await?;
    let output = tokio::process::Command::new("git")
        .args([
            "-C",
            target.to_str().unwrap_or_default(),
            "submodule",
            "status",
            "--recursive",
        ])
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), output)
        .await
        .ok()
        .and_then(|r| r.ok())
        .ok_or_else(|| AppError::bad("git submodule status 执行失败"))?;
    // 无 submodule 时命令输出空、退出码 0；仓非 git 同样空
    let mut rows: Vec<(String, String, String)> = Vec::new(); // (mark, sha, path)
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // 行首标记不可 trim：' '=同步 / '-'=未初始化 / '+'=SHA 漂移 / 'U'=冲突
        let mut chars = line.chars();
        let (Some(mark), Some(rest)) = (chars.next(), Some(chars.as_str())) else {
            continue;
        };
        let mut parts = rest.splitn(2, ' ');
        let (Some(sha), Some(raw_path)) = (parts.next(), parts.next()) else {
            continue;
        };
        // 行尾可能带 describe 后缀 `libs/sub (heads/main)`
        let path = raw_path.split(" (").next().unwrap_or(raw_path);
        rows.push((
            mark.to_string(),
            sha.chars().take(8).collect(),
            path.trim().to_string(),
        ));
    }

    // 并发 4 补子模块内 status 计数（index 对齐，保序回填）
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let mut futs = Vec::new();
    for (idx, (_, _, rel)) in rows.iter().enumerate() {
        if rows[idx].0 == "-" || rel.contains("..") {
            continue; // 未初始化必失败；".." 防御（git 输出不可信面）
        }
        let sem = sem.clone();
        let target = target.clone();
        let rel = rel.clone();
        futs.push((
            idx,
            tokio::spawn(async move {
                let _permit = sem.acquire().await.ok()?;
                let out = tokio::process::Command::new("git")
                    .args(["-C", target.join(&rel).to_str()?, "status", "--porcelain"])
                    .output();
                let out = tokio::time::timeout(std::time::Duration::from_secs(3), out)
                    .await
                    .ok()?
                    .ok()?;
                Some(count_porcelain(&String::from_utf8_lossy(&out.stdout)))
            }),
        ));
    }
    let counts = futures::future::join_all(
        futs.into_iter()
            .map(|(idx, f)| async move { (idx, f.await.ok().flatten()) }),
    )
    .await;

    let mut out_list = Vec::with_capacity(rows.len());
    for (idx, (mark, sha, path)) in rows.into_iter().enumerate() {
        let (changes, untracked) = counts
            .iter()
            .find(|(i, _)| *i == idx)
            .and_then(|(_, c)| *c)
            .unwrap_or((0, 0));
        out_list.push(SubmoduleInfo {
            status: mark,
            sha,
            path,
            changes,
            untracked,
        });
    }
    Ok(Json(out_list))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmoduleInfo {
    /// ' ' 同步 / '-' 未初始化 / '+' SHA 漂移（子模块 HEAD 与父仓记录不一致）/ 'U' 冲突
    pub status: String,
    pub sha: String,
    pub path: String,
    /// 工作区变更文件数（porcelain 非 `??` 行）；未初始化/探测失败 → 0
    pub changes: u32,
    /// 未跟踪文件数（`??` 行）
    pub untracked: u32,
}

/// B4 创建 worktree：基于基准节点 HEAD 新建分支 + worktree 目录
/// `<主仓>/.worktree/<name>`。分支/目录已存在 409；基准须在树白名单内。
/// git worktree add 原子：失败不留目录。成功后失效树缓存。
async fn create_worktree(
    State(state): State<SharedState>,
    Json(body): Json<WorktreeBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let name = body.name.trim();
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-'))
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.ends_with(|c: char| c.is_ascii_alphanumeric());
    if !valid {
        return Err(AppError::bad(
            "分支名限 1-64 位字母/数字/_/-，需以字母或数字开头结尾",
        ));
    }
    let base = ensure_tree_path(&state, &base_path(&body)).await?;
    // 主仓根（porcelain 首行）
    let main_root = git_worktree_list(&base)
        .await
        .and_then(|l| l.first().map(|e| PathBuf::from(&e.path)))
        .ok_or_else(|| AppError::bad("基准目录不是 git 仓库"))?;
    let target = main_root.join(".worktree").join(name);
    // 冲突预检：分支已存在 / 目标目录已存在 → 409，不破坏现有
    let head = git_output(&base, &["rev-parse", "HEAD"]).await?.stdout;
    if git_succeeds(
        &base,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .await
    {
        return Err(AppError::conflict(format!("分支 {name} 已存在")));
    }
    if tokio::fs::metadata(&target).await.is_ok() {
        return Err(AppError::conflict(format!(
            "目标目录 {} 已存在",
            target.display()
        )));
    }
    let out = git_output(
        &base,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            &target.to_string_lossy(),
            head.trim(),
        ],
    )
    .await?;
    if !out.success {
        return Err(AppError::bad(format!("worktree add 失败: {}", out.stderr)));
    }
    invalidate_tree_cache(&state).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "path": target.to_string_lossy() })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct WorktreeBody {
    /// 基准节点（主仓或任一 worktree）路径
    pub base: String,
    /// 新分支名（同时作 .worktree/ 下目录名）
    pub name: String,
}

fn base_path(body: &WorktreeBody) -> String {
    body.base.clone()
}

/// git 辅助：成功返回 stdout/stderr/success。async + 5s 超时（r3 安全审查 MINOR-1：
/// worktree add 含 post-checkout hook，同步无超时会挂死 runtime worker）。
struct GitOut {
    stdout: String,
    stderr: String,
    success: bool,
}

async fn git_output(cwd: &Path, args: &[&str]) -> Result<GitOut, AppError> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), output).await {
        Ok(Ok(o)) => o,
        _ => return Err(AppError::bad("git 执行失败或超时")),
    };
    Ok(GitOut {
        success: out.status.success(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
    })
}

async fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    git_output(cwd, args)
        .await
        .map(|o| o.success)
        .unwrap_or(false)
}

async fn projects_tree(State(state): State<SharedState>) -> Json<Vec<GitTreeGroup>> {
    Json(build_tree(&state).await)
}

#[derive(Debug, Deserialize)]
pub struct OpenDirBody {
    pub path: String,
}

/// B3 打开本地目录：canonicalize 消解 `..`/symlink 后必须落在白名单内（前缀=注册目录
/// 内部子目录亦放行），macOS `open` 在 Finder 打开。安全硬要求：任何校验失败统一 403
/// 同文案（不区分不存在/白名单外，消除路径存在性探测信号——r40 tester 观察）。
async fn open_dir(
    State(state): State<SharedState>,
    Json(body): Json<OpenDirBody>,
) -> Result<StatusCode, AppError> {
    if body.path.contains("..") {
        return Err(AppError::forbidden(DENY_MSG));
    }
    let target = match tokio::fs::canonicalize(&body.path).await {
        Ok(t) if t.is_dir() => t,
        _ => return Err(AppError::forbidden(DENY_MSG)),
    };
    let allowed = allowed_roots(&state)
        .await
        .iter()
        .any(|root| target == *root || target.starts_with(root));
    if !allowed {
        return Err(AppError::forbidden(DENY_MSG));
    }
    let output = tokio::process::Command::new(&state.cfg.dir_opener)
        .arg(&target)
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), output).await {
        Ok(Ok(o)) => o,
        _ => return Err(AppError::bad("打开目录失败或超时")),
    };
    if !out.status.success() {
        return Err(AppError::bad("打开目录失败"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_list_with_branches() {
        let raw = "worktree /repo/main\nHEAD abc123def4567890\nbranch refs/heads/main\n\nworktree /repo/.worktree/feat\nHEAD def4567890abcdef\nbranch refs/heads/feat-x\n";
        let list = parse_worktree_list(raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, "/repo/main");
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[1].path, "/repo/.worktree/feat");
        assert_eq!(list[1].branch.as_deref(), Some("feat-x"));
        assert!(list[1].head.as_deref().unwrap().starts_with("def4"));
    }

    #[test]
    fn parses_detached_head_without_branch() {
        let raw = "worktree /repo/.worktree/detached\nHEAD abc123def4567890\ndetached\n\n";
        let list = parse_worktree_list(raw);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].branch, None);
        assert_eq!(list[0].head.as_deref(), Some("abc123def4567890"));
    }

    #[test]
    fn node_extracts_short_name_and_head() {
        let n = node(
            "/repo/.worktree/feat",
            false,
            true,
            Some("abc123def4567890"),
            Some("feat-x"),
        );
        assert_eq!(n.name, "feat");
        assert_eq!(n.branch.as_deref(), Some("feat-x"));
        assert_eq!(n.head.as_deref(), Some("abc123de"));
        let flat = node("/opt/plain", true, false, None, None);
        assert_eq!(flat.name, "plain");
        assert!(!flat.is_git);
    }
}

#[cfg(test)]
mod submodule_tests {
    use super::*;

    #[test]
    fn count_porcelain_mixed_and_rename() {
        let out = " M a.txt\n?? new.txt\nR  old.txt -> new.txt\nMM b.txt\n";
        assert_eq!(count_porcelain(out), (3, 1));
    }

    #[test]
    fn count_porcelain_empty_and_untracked_only() {
        assert_eq!(count_porcelain(""), (0, 0));
        assert_eq!(count_porcelain("?? a\n?? b\n"), (0, 2));
        // 行长 < 2（空行）不计
        assert_eq!(count_porcelain("\n"), (0, 0));
    }
}
