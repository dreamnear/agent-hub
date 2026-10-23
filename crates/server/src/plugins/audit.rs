//! 静态审计（ADR-0007 第 5 条 / BR-02）：贡献物清单 + 触达面 + 校验结论 + 资产体积。
//!
//! 审计先于任何生效动作；产物落 `plugin_audit/<id>-<content-hash>.json` 供安装确认 UI
//! 渲染与事后追溯。任一 `errors` 非空 = 整体拒绝安装（BR-02）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{self, ManifestIssue};
use super::{api_surface, credentials::CredentialsStore};

/// 单插件视图首屏体积上限（ADR-0010 第 4 条，gzip 前原始字节的保守代理：
/// 超过 300KB **压缩后**才可能超限，此处以原始 900KB 触发提示，宁松勿误报）
const ASSET_WARN_BYTES: u64 = 900 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contribution {
    /// `tool` | `skill` | `mcp` | `view` | `command`
    pub kind: String,
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `read` | `high`（skill/mcp/view 为 `-`）
    pub risk: String,
    /// 来源文件（相对插件根）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialReach {
    pub name: String,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HighRiskReach {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Reachability {
    /// `api` 类声明（v1 预留，无消费者）
    pub api: Vec<String>,
    pub data: Vec<String>,
    pub credentials: Vec<CredentialReach>,
    pub high_risk: Vec<HighRiskReach>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<Vec<String>>,
    pub mcp: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandReach>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReach {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_tool: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditReport {
    pub plugin_id: String,
    pub plugin_dir: String,
    pub content_hash: String,
    pub contributions: Vec<Contribution>,
    pub reach: Reachability,
    pub errors: Vec<ManifestIssue>,
    pub warnings: Vec<ManifestIssue>,
    pub asset_bytes: u64,
    pub asset_files: u64,
    /// 是否需要用户确认（含贡献物或权限声明）
    pub installable: bool,
    /// 渲染器 match 声明（B3，ADR-0012 拍板 3）：跨插件冲突判定的结构化暴露
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub render_claims: Vec<RenderClaim>,
}

/// 一条渲染器 match 占用（kind = "ext" | "fence"，value 小写规范化）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderClaim {
    pub kind: String,
    pub value: String,
}

/// 目录内容 hash（ADR-0001：排序后路径 + 内容；不跟随符号链接）。
pub fn content_hash(root: &Path) -> String {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files(root, root, &mut files, 0);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel, bytes) in files {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(&bytes);
        h.update([0u8]);
    }
    format!("sha256:{}", super::registry::hex(&h.finalize()))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>, depth: usize) {
    // 深度护栏：插件目录不应有极深嵌套（防扫描期被恶意结构拖死）
    if depth > 24 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        // 不跟随符号链接（ADR-0001 第 5 条的审计前置加固）
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_files(root, &path, out, depth + 1);
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if let Ok(bytes) = std::fs::read(&path) {
                out.push((rel, bytes));
            }
        }
    }
}

/// 审计一个插件目录（不安装）。`creds` 只用于展示「已配置 / 未配置」，值零回显。
pub fn audit_dir(root: &Path, creds: &CredentialsStore) -> AuditReport {
    let hash = content_hash(root);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut contributions = Vec::new();
    let mut reach = Reachability::default();

    let manifest_path = root.join("plugin.json");
    let raw = match std::fs::read_to_string(&manifest_path) {
        Ok(r) => r,
        Err(e) => {
            errors.push(
                ManifestIssue::new("plugin.json", "missing-file", format!("读取失败：{e}"))
                    .with_path("plugin.json"),
            );
            return AuditReport {
                plugin_id: String::new(),
                plugin_dir: root.to_string_lossy().into_owned(),
                content_hash: hash,
                contributions,
                reach,
                errors,
                warnings,
                asset_bytes: 0,
                asset_files: 0,
                installable: false,
                render_claims: Vec::new(),
            };
        }
    };

    let parsed = match manifest::parse(&raw) {
        Ok(p) => p,
        Err(errs) => {
            errors.extend(errs);
            return AuditReport {
                plugin_id: String::new(),
                plugin_dir: root.to_string_lossy().into_owned(),
                content_hash: hash,
                contributions,
                reach,
                errors,
                warnings,
                asset_bytes: 0,
                asset_files: 0,
                installable: false,
                render_claims: Vec::new(),
            };
        }
    };
    warnings.extend(parsed.warnings);
    let m = parsed.manifest;

    // —— 路径存在性与符号链接越界（ADR-0001 第 2 条）——
    for (i, v) in m.contributes.views.iter().enumerate() {
        let field = format!("contributes.views[{i}].entry");
        match resolve_inside(root, &v.entry) {
            Ok(p) => {
                if !p.is_file() {
                    errors.push(
                        ManifestIssue::new(field, "path-not-file", "视图入口不是文件")
                            .with_path(v.entry.clone()),
                    );
                }
            }
            Err(why) => errors.push(
                ManifestIssue::new(field, why.rule(), why.message()).with_path(v.entry.clone()),
            ),
        }
    }
    for (i, s) in m.contributes.skills.iter().enumerate() {
        let rel = s
            .path
            .clone()
            .unwrap_or_else(|| format!("skills/{}/SKILL.md", s.id));
        let field = format!("contributes.skills[{i}].path");
        match resolve_inside(root, &rel) {
            Ok(p) if p.is_file() => {}
            Ok(_) => errors.push(
                ManifestIssue::new(field, "path-not-file", "skill 声明路径不是文件")
                    .with_path(rel.clone()),
            ),
            Err(why) => errors
                .push(ManifestIssue::new(field, why.rule(), why.message()).with_path(rel.clone())),
        }
    }
    for (i, t) in m.contributes.tools.iter().enumerate() {
        if let Some(p) = &t.params_schema {
            match resolve_inside(root, p) {
                Ok(path) if path.is_file() => {}
                Ok(_) => errors.push(
                    ManifestIssue::new(
                        format!("contributes.tools[{i}].paramsSchema"),
                        "path-not-file",
                        "参数 schema 不是文件",
                    )
                    .with_path(p.clone()),
                ),
                Err(why) => errors.push(
                    ManifestIssue::new(
                        format!("contributes.tools[{i}].paramsSchema"),
                        why.rule(),
                        why.message(),
                    )
                    .with_path(p.clone()),
                ),
            }
        }
    }
    // 文件装饰图标（B3，ADR-0013）：图标文件必须存在（禁逃逸已在 manifest 段阻断）
    for (i, d) in m.contributes.file_decorations.iter().enumerate() {
        let field = format!("contributes.fileDecorations[{i}].icon");
        match resolve_inside(root, &d.icon) {
            Ok(p) => {
                if !p.is_file() {
                    errors.push(
                        ManifestIssue::new(field, "path-not-file", "装饰图标不是文件")
                            .with_path(d.icon.clone()),
                    );
                }
            }
            Err(why) => errors.push(
                ManifestIssue::new(field, why.rule(), why.message()).with_path(d.icon.clone()),
            ),
        }
    }
    // 渲染器（B3，ADR-0012）：plugin-iframe 档的 entry 同样走路径校验
    for (i, r) in m.contributes.renderers.iter().enumerate() {
        if let Some(entry) = &r.entry {
            let field = format!("contributes.renderers[{i}].entry");
            match resolve_inside(root, entry) {
                Ok(p) => {
                    if !p.is_file() {
                        errors.push(
                            ManifestIssue::new(field, "path-not-file", "渲染入口不是文件")
                                .with_path(entry.clone()),
                        );
                    }
                }
                Err(why) => errors.push(
                    ManifestIssue::new(field, why.rule(), why.message()).with_path(entry.clone()),
                ),
            }
        }
    }
    contributions = contributions_of(&m);

    // —— 触达面 ——
    reach.api = m.permissions.api.clone();
    reach.data = m.permissions.data.clone();
    reach.credentials = creds
        .statuses(&m.permissions.credentials)
        .into_iter()
        .map(|s| CredentialReach {
            name: s.name,
            configured: s.configured,
        })
        .collect();
    reach.high_risk = m
        .permissions
        .high_risk
        .iter()
        .map(|id| HighRiskReach {
            id: id.clone(),
            label: api_surface::high_risk_label(id).to_string(),
        })
        .collect();
    if let Some(b) = &m.backend {
        // 命令 + 参数原样展示（凭据占位 `$cred:` 只出现名字）
        let mut cmd = vec![b.command.clone()];
        cmd.extend(b.args.iter().cloned());
        reach.backend = Some(cmd);
        probe_command(root, &b.command, &mut errors, &mut warnings);
    }
    for mcp in &m.contributes.mcp {
        let mut cmd = vec![mcp.command.clone()];
        cmd.extend(mcp.args.iter().cloned());
        reach.mcp.push(cmd);
    }
    for c in &m.contributes.commands {
        reach.commands.push(CommandReach {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: c.kind.clone(),
            target_view: c.target.view.clone(),
            target_tool: c.target.tool.clone(),
        });
    }

    // —— 视图资产体积 + 硬编码色值 warning（ADR-0010 / ADR-0004 第 6 条）——
    let mut asset_bytes = 0u64;
    let mut asset_files = 0u64;
    let mut color_hits: BTreeMap<String, usize> = BTreeMap::new();
    let view_dirs: Vec<PathBuf> = m
        .contributes
        .views
        .iter()
        .filter_map(|v| {
            std::path::Path::new(&v.entry)
                .parent()
                .map(|p| root.join(p))
        })
        .collect();
    for dir in &view_dirs {
        scan_view_assets(dir, &mut asset_bytes, &mut asset_files, &mut color_hits, 0);
    }
    if asset_bytes > ASSET_WARN_BYTES {
        warnings.push(ManifestIssue::new(
            "contributes.views",
            "asset-size",
            format!(
                "视图资产原始体积 {asset_bytes} 字节（{asset_files} 个文件），可能超过单插件 300KB(gzip) 提示线",
            ),
        ));
    }
    for (file, hits) in color_hits {
        warnings.push(
            ManifestIssue::new(
                "contributes.views",
                "hardcoded-color",
                format!("视图文件含 {hits} 处硬编码色值（应走宿主 --c-* token，BR-16）"),
            )
            .with_path(file),
        );
    }

    let installable = errors.is_empty();
    let render_claims = m
        .contributes
        .renderers
        .iter()
        .flat_map(|r| {
            r.match_rule
                .fence
                .iter()
                .map(|f| RenderClaim {
                    kind: "fence".into(),
                    value: f.to_ascii_lowercase(),
                })
                .chain(r.match_rule.ext.iter().map(|e| RenderClaim {
                    kind: "ext".into(),
                    value: e.to_ascii_lowercase().trim_start_matches('.').to_string(),
                }))
        })
        .collect();
    AuditReport {
        plugin_id: m.id.clone(),
        plugin_dir: root.to_string_lossy().into_owned(),
        content_hash: hash,
        contributions,
        reach,
        errors,
        warnings,
        asset_bytes,
        asset_files,
        installable,
        render_claims,
    }
}

/// 从 manifest 生成贡献物清单（审计输出与 `/api/plugins` 列表共用同一投影，
/// 避免两处形状漂移）。
pub fn contributions_of(m: &super::manifest::Manifest) -> Vec<Contribution> {
    let mut out = Vec::new();
    for t in &m.contributes.tools {
        out.push(Contribution {
            kind: "tool".into(),
            id: t.id.clone(),
            name: t.name.clone(),
            description: t.description.clone(),
            risk: t.risk.clone(),
            path: t.params_schema.clone(),
        });
    }
    for s in &m.contributes.skills {
        out.push(Contribution {
            kind: "skill".into(),
            id: s.id.clone(),
            name: s.id.clone(),
            description: None,
            risk: "-".into(),
            path: Some(
                s.path
                    .clone()
                    .unwrap_or_else(|| format!("skills/{}/SKILL.md", s.id)),
            ),
        });
    }
    for mcp in &m.contributes.mcp {
        out.push(Contribution {
            kind: "mcp".into(),
            id: mcp.id.clone(),
            name: mcp.id.clone(),
            description: None,
            risk: "-".into(),
            path: Some(mcp.command.clone()),
        });
    }
    for v in &m.contributes.views {
        out.push(Contribution {
            kind: "view".into(),
            id: v.id.clone(),
            name: v.title.clone(),
            description: None,
            risk: "-".into(),
            path: Some(v.entry.clone()),
        });
    }
    for c in &m.contributes.commands {
        let target_desc = if let Some(vid) = &c.target.view {
            format!("view:{vid}")
        } else if let Some(tid) = &c.target.tool {
            format!("tool:{tid}")
        } else {
            String::new()
        };
        out.push(Contribution {
            kind: "command".into(),
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone().or(Some(target_desc)),
            risk: "-".into(),
            path: None,
        });
    }
    for r in &m.contributes.renderers {
        let mut match_desc = String::new();
        if !r.match_rule.fence.is_empty() {
            match_desc.push_str(&format!("fence:{}", r.match_rule.fence.join("|")));
        }
        if !r.match_rule.ext.is_empty() {
            if !match_desc.is_empty() {
                match_desc.push(' ');
            }
            match_desc.push_str(&format!("ext:{}", r.match_rule.ext.join("|")));
        }
        out.push(Contribution {
            kind: "renderer".into(),
            id: r.id.clone(),
            name: r.engine.clone(),
            description: Some(match_desc),
            risk: "-".into(),
            path: r.entry.clone(),
        });
    }
    if let Some(tt) = &m.contributes.theme_tokens {
        for token in tt.tokens.keys() {
            out.push(Contribution {
                kind: "themeTokens".into(),
                id: token.clone(),
                name: token.clone(),
                description: None,
                risk: "-".into(),
                path: None,
            });
        }
    }
    for d in &m.contributes.file_decorations {
        out.push(Contribution {
            kind: "fileDecoration".into(),
            id: d.id.clone(),
            name: format!("ext:{}", d.match_rule.ext.join("|")),
            description: None,
            risk: "-".into(),
            path: Some(d.icon.clone()),
        });
    }
    for svc in &m.contributes.services {
        out.push(Contribution {
            kind: "service".into(),
            id: svc.id.clone(),
            name: svc.label.clone(),
            description: svc.placeholder.clone(),
            risk: "-".into(),
            path: None,
        });
    }
    out
}

/// 审计产物落盘（`plugin_audit/<id>-<hash>.json`，0644；BR-02 追溯用）。
pub fn persist(report: &AuditReport, audit_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(audit_dir)?;
    let short = report
        .content_hash
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect::<String>();
    let id = if report.plugin_id.is_empty() {
        "unknown"
    } else {
        report.plugin_id.as_str()
    };
    let path = audit_dir.join(format!("{id}-{short}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(report)?)?;
    Ok(path)
}

/// 审计产物保留策略（B4 治理，需求功能点 6）：按插件 id 分组，每插件仅保留
/// 最近 N 份（mtime 新者优先）。返回清理的文件数。文件名约定 `<id>-<hash16>.json`
/// ——id 分组 = 文件名剥掉末尾 `-<16 hex>.json`。
pub fn prune_artifacts(audit_dir: &Path, keep_per_plugin: usize) -> std::io::Result<usize> {
    if keep_per_plugin == 0 {
        return Ok(0); // 0 = 关闭清理（保守缺省防误删）
    }
    let groups = group_artifacts(audit_dir)?;
    let mut removed = 0;
    for (_, mut files) in groups {
        if files.len() <= keep_per_plugin {
            continue;
        }
        // mtime 新者优先（新审计对当前版本更有追溯价值）
        files.sort_by(|a, b| b.modified.cmp(&a.modified));
        for f in files.into_iter().skip(keep_per_plugin) {
            if std::fs::remove_file(&f.path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// 卸载随清理（B4）：删除某插件的全部审计产物。
pub fn purge_plugin_artifacts(audit_dir: &Path, plugin_id: &str) -> std::io::Result<usize> {
    let groups = group_artifacts(audit_dir)?;
    let mut removed = 0;
    if let Some(files) = groups.get(plugin_id) {
        for f in files {
            if std::fs::remove_file(&f.path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[derive(Debug)]
struct ArtifactFile {
    path: PathBuf,
    modified: std::time::SystemTime,
}

/// 审计目录清点：按插件 id 分组（文件名 `<id>-<hash16>.json` → id = 去尾 `-{16hex}`）。
fn group_artifacts(
    audit_dir: &Path,
) -> std::io::Result<std::collections::BTreeMap<String, Vec<ArtifactFile>>> {
    let mut groups: std::collections::BTreeMap<String, Vec<ArtifactFile>> = Default::default();
    let read = match std::fs::read_dir(audit_dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(groups),
        Err(e) => return Err(e),
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") || name.len() < 22 {
            continue;
        }
        let stem = name.trim_end_matches(".json");
        // 末段 16 hex hash 归 id；不合规形态归 "unknown" 组（保守保留原策略可清理）
        let id = match stem.rsplit_once('-') {
            Some((id, hash))
                if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                id.to_string()
            }
            _ => "unknown".to_string(),
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        groups.entry(id).or_default().push(ArtifactFile { path, modified });
    }
    Ok(groups)
}

/// 相对路径解析 + 逃逸检测（canonicalize 后必须仍在插件根内）。
pub fn resolve_inside(root: &Path, rel: &str) -> Result<PathBuf, PathProblem> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(PathProblem::Escape("路径必须是相对插件根".into()));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(PathProblem::Escape("路径不得含 ..".into()));
    }
    let joined = root.join(p);
    if !joined.exists() {
        return Err(PathProblem::Missing(format!("文件不存在：{rel}")));
    }
    let root_canon = root
        .canonicalize()
        .map_err(|e| PathProblem::Missing(format!("插件目录不可读：{e}")))?;
    let canon = joined
        .canonicalize()
        .map_err(|e| PathProblem::Missing(format!("路径不可解析：{e}")))?;
    if !canon.starts_with(&root_canon) {
        // 符号链接指向插件根之外（ADR-0001 第 5 条）
        return Err(PathProblem::Escape("符号链接指向插件根之外".into()));
    }
    Ok(canon)
}

#[derive(Debug, Clone)]
pub enum PathProblem {
    Escape(String),
    Missing(String),
}

impl PathProblem {
    fn rule(&self) -> &'static str {
        match self {
            PathProblem::Escape(_) => "path-escape",
            PathProblem::Missing(_) => "path-missing",
        }
    }

    fn message(&self) -> String {
        match self {
            PathProblem::Escape(m) | PathProblem::Missing(m) => m.clone(),
        }
    }
}

/// 运行时探测（ADR-0006 拍板结论 2）：解释器/命令不存在只 warning 不拒绝。
fn probe_command(
    root: &Path,
    command: &str,
    errors: &mut Vec<ManifestIssue>,
    warnings: &mut Vec<ManifestIssue>,
) {
    if command.contains('/') {
        if let Err(why) = resolve_inside(root, command) {
            // 插件自带运行时代码：存在性是硬要求
            errors.push(
                ManifestIssue::new("backend.command", why.rule(), why.message())
                    .with_path(command.to_string()),
            );
        }
        return;
    }
    if find_in_path(command).is_none() {
        warnings.push(ManifestIssue::new(
            "backend.command",
            "runtime-missing",
            format!("PATH 中未找到 {command}——插件装载时可能失败"),
        ));
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    for dir in crate::platform::search_path_dirs() {
        let p = dir.join(command);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 视图资产扫描：体积统计 + 硬编码色值探测（`#hex` / `rgb(` / `hsl(`）。
fn scan_view_assets(
    dir: &Path,
    bytes: &mut u64,
    files: &mut u64,
    color_hits: &mut BTreeMap<String, usize>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            scan_view_assets(&path, bytes, files, color_hits, depth + 1);
            continue;
        }
        *bytes += meta.len();
        *files += 1;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "css" | "html" | "js" | "mjs" | "svg") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let hits = count_color_literals(&text);
        if hits > 0 {
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            color_hits.insert(rel, hits);
        }
    }
}

/// 常见硬编码色值形态计数（轻量扫描器；只做 warning，不阻断，ADR-0004 拍板结论 4）。
/// B3 正式化：`--*: <兜底值>` 变量定义行豁免——视图 CSS 的 :root 兜底默认值是
/// 声明式首帧形态（hub:init token 到达即被宿主值覆盖，见示例插件注释），与
/// 「属性直写散值」不同类；扫描只计后者。
fn count_color_literals(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut hits = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            // 变量定义行豁免：`#` 所在行的行首（去空白）以 `--` 开头且 `#` 前有 `:`——
            // `--bg: #ffffff` 形态（首帧兜底变量），不算散值
            let line_start = text[..i].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let before_hash = &text[line_start..i];
            if before_hash.trim_start().starts_with("--") && before_hash.contains(':') {
                i += 1;
                continue;
            }
            // #rgb / #rrggbb / #rrggbbaa
            let mut n = 0;
            while i + 1 + n < bytes.len() && n < 8 && bytes[i + 1 + n].is_ascii_hexdigit() {
                n += 1;
            }
            let boundary = i + 1 + n >= bytes.len() || !bytes[i + 1 + n].is_ascii_alphanumeric();
            if n >= 3 && boundary {
                hits += 1;
                i += 1 + n;
                continue;
            }
        }
        i += 1;
    }
    let lower = text.to_ascii_lowercase();
    hits += lower.matches("rgb(").count() + lower.matches("rgba(").count();
    hits += lower.matches("hsl(").count() + lower.matches("hsla(").count();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn creds() -> CredentialsStore {
        // 只读空表：路径不存在即空（审计只需要「已配置 / 未配置」态）
        CredentialsStore::load(std::env::temp_dir().join("hub-test-no-such-credentials.json"))
    }

    fn write_demo(dir: &Path) {
        fs::create_dir_all(dir.join("view")).unwrap();
        fs::create_dir_all(dir.join("backend")).unwrap();
        fs::write(dir.join("backend/main.js"), "// demo\n").unwrap();
        fs::write(
            dir.join("view/index.html"),
            "<html><body><script src=\"app.js\"></script></body></html>",
        )
        .unwrap();
        fs::write(
            dir.join("plugin.json"),
            r#"{
              "schemaVersion": 1,
              "id": "demo-plugin",
              "name": "Demo",
              "version": "0.1.0",
              "hub": {"apiVersion": "1"},
              "contributes": {
                "tools": [{"id":"ping","name":"Ping","risk":"read","impl":{"kind":"backend","ref":"ping"}}],
                "views": [{"id":"home","title":"Home","entry":"view/index.html"}]
              },
              "permissions": {"data":["projects:read"],"credentials":["gitlab_pat"]},
              "backend": {"command":"node","args":["backend/main.js"]}
            }"#,
        )
        .unwrap();
    }

    #[test]
    fn audit_reports_contributions_and_reach() {
        let dir = tempfile::tempdir().unwrap();
        write_demo(dir.path());
        let r = audit_dir(dir.path(), &creds());
        assert!(r.installable, "{:?}", r.errors);
        assert_eq!(r.contributions.len(), 2);
        assert_eq!(r.reach.data, vec!["projects:read"]);
        assert_eq!(r.reach.credentials.len(), 1);
        assert!(!r.reach.credentials[0].configured);
        assert_eq!(r.reach.backend.as_ref().unwrap()[0], "node");
        assert!(r.asset_bytes > 0);
    }

    #[test]
    fn missing_view_entry_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_demo(dir.path());
        fs::remove_file(dir.path().join("view/index.html")).unwrap();
        let r = audit_dir(dir.path(), &creds());
        assert!(!r.installable);
        assert!(r.errors.iter().any(|e| e.rule == "path-missing"));
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("evil.html"), "<html></html>").unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_demo(dir.path());
        fs::remove_file(dir.path().join("view/index.html")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("evil.html"),
            dir.path().join("view/index.html"),
        )
        .unwrap();
        let r = audit_dir(dir.path(), &creds());
        assert!(!r.installable);
        assert!(r.errors.iter().any(|e| e.rule == "path-escape"));
    }

    #[test]
    fn hardcoded_colors_are_warnings_only() {
        let dir = tempfile::tempdir().unwrap();
        write_demo(dir.path());
        fs::write(
            dir.path().join("view/index.html"),
            "<style>.x{color:#ffffff;background:rgb(1,2,3)}</style>",
        )
        .unwrap();
        let r = audit_dir(dir.path(), &creds());
        assert!(r.installable, "硬编码色值不阻断安装");
        assert!(r.warnings.iter().any(|w| w.rule == "hardcoded-color"));
    }

    #[test]
    fn content_hash_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        write_demo(dir.path());
        let h1 = content_hash(dir.path());
        fs::write(dir.path().join("backend/main.js"), "// changed\n").unwrap();
        let h2 = content_hash(dir.path());
        assert_ne!(h1, h2);
        assert!(h1.starts_with("sha256:"));
    }

    #[test]
    fn audit_is_persisted_for_traceability() {
        let dir = tempfile::tempdir().unwrap();
        write_demo(dir.path());
        let r = audit_dir(dir.path(), &creds());
        let audit_dir_path = dir.path().join("audit");
        let p = persist(&r, &audit_dir_path).unwrap();
        assert!(p.exists());
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("demo-plugin-"));
    }

    #[test]
    fn prune_keeps_newest_per_plugin() {
        // B4 治理：保留策略——每插件留 N 份（mtime 新者优先），跨插件互不影响
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path();
        let write_at = |name: &str, age: std::time::Duration| {
            let p = a.join(name);
            std::fs::write(&p, "{}").unwrap();
            let old = std::time::SystemTime::now() - age;
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        write_at("demo-aaaaaaaaaaaaaaaa.json", std::time::Duration::from_secs(300));
        write_at("demo-bbbbbbbbbbbbbbbb.json", std::time::Duration::from_secs(100));
        write_at("other-cccccccccccccccc.json", std::time::Duration::from_secs(50));
        let removed = prune_artifacts(a, 1).unwrap();
        assert_eq!(removed, 1);
        assert!(!a.join("demo-aaaaaaaaaaaaaaaa.json").exists(), "旧的被清");
        assert!(a.join("demo-bbbbbbbbbbbbbbbb.json").exists(), "新的保留");
        assert!(a.join("other-cccccccccccccccc.json").exists(), "另一插件不受影响");
        // keep=0 = 关闭清理
        let removed = prune_artifacts(a, 0).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn purge_removes_only_target_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path();
        for name in ["demo-aaaaaaaaaaaaaaaa.json", "other-bbbbbbbbbbbbbbbb.json"] {
            std::fs::write(a.join(name), "{}").unwrap();
        }
        let removed = purge_plugin_artifacts(a, "demo").unwrap();
        assert_eq!(removed, 1);
        assert!(!a.join("demo-aaaaaaaaaaaaaaaa.json").exists());
        assert!(a.join("other-bbbbbbbbbbbbbbbb.json").exists());
    }
}
