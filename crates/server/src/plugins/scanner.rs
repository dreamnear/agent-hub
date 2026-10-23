//! 目录扫描（ADR-0002 第 2 条 / ADR-0001 第 5 条）：启动 / 周期 / 手动三种触发。
//!
//! 只发现、不安装：扫描结果进「新发现 · 待确认」，安装须用户显式确认（BR-02）。
//! 范围 = `plugins_dir/*/plugin.json`（一层深、不递归、不跟随符号链接）+ transient 目录根。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::audit::{self, AuditReport};
use super::credentials::CredentialsStore;
use super::manifest::ManifestIssue;
use super::registry::Registry;

/// 一个新发现 / 更新候选条目。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredItem {
    /// manifest 声明的 id（manifest 不可解析时回退目录名）
    pub id: String,
    pub dir_name: String,
    pub path: String,
    /// `drop-in` | `transient`
    pub source_kind: String,
    /// `new` | `audit_failed` | `update`
    pub status: String,
    /// 已安装条目的当前 hash（更新候选时用于展示版本差异）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_content_hash: Option<String>,
    /// 更新差异面（B4 BR-13：审计卡展示差异面；内容变化 + 权限面变化分列）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_diff: Option<UpdateDiff>,
    pub report: AuditReport,
}

/// 更新差异面（B4）：新版相对已装版的贡献物与权限面变化（BR-13 扩权确认的
/// 展示依据——确认动作同时覆盖内容与权限面，差异逐条可读）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDiff {
    pub version_from: String,
    pub version_to: String,
    /// 权限面 hash 变化（true = 扩权/缩权/换权，卡片红标）
    pub permissions_changed: bool,
    /// 数据域变化（added/removed 各一侧）
    pub data_added: Vec<String>,
    pub data_removed: Vec<String>,
    pub credentials_added: Vec<String>,
    pub credentials_removed: Vec<String>,
    pub high_risk_added: Vec<String>,
    pub high_risk_removed: Vec<String>,
    /// 贡献物增删（kind:id 形态；变更按 removed+added 各计一次）
    pub contributions_added: Vec<String>,
    pub contributions_removed: Vec<String>,
}

/// 扫描结果。
#[derive(Debug, Clone, Default)]
pub struct ScanOutcome {
    pub items: Vec<DiscoveredItem>,
    pub last_scan_at: String,
}

/// 更新差异面计算（B4，纯函数）：已装 manifest 快照 vs 新版审计投影。
/// 贡献物按 `kind:id` 对齐；权限面按域集合对齐（数组序无关）。
pub fn compute_update_diff(
    installed: &super::registry::InstalledPlugin,
    new_report: &AuditReport,
) -> UpdateDiff {
    let old_m = &installed.manifest;
    let new_reach = &new_report.reach;
    let set_diff = |old: &[String], new: &[String]| -> (Vec<String>, Vec<String>) {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        for n in new {
            if !old.contains(n) {
                added.push(n.clone());
            }
        }
        for o in old {
            if !new.contains(o) {
                removed.push(o.clone());
            }
        }
        (added, removed)
    };
    let (data_added, data_removed) = set_diff(&old_m.permissions.data, &new_reach.data);
    let cred_old: Vec<String> = old_m.permissions.credentials.clone();
    let cred_new: Vec<String> = new_reach
        .credentials
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let (credentials_added, credentials_removed) = set_diff(&cred_old, &cred_new);
    let high_risk_old: Vec<String> = old_m.permissions.high_risk.clone();
    let high_risk_new: Vec<String> =
        new_reach.high_risk.iter().map(|h| h.id.clone()).collect();
    let (high_risk_added, high_risk_removed) = set_diff(&high_risk_old, &high_risk_new);
    let key = |c: &super::audit::Contribution| format!("{}:{}", c.kind, c.id);
    let old_contribs: Vec<String> = super::audit::contributions_of(old_m).iter().map(&key).collect();
    let new_contribs: Vec<String> = new_report.contributions.iter().map(&key).collect();
    let (contributions_added, contributions_removed) = set_diff(&old_contribs, &new_contribs);
    let permissions_changed = !(data_added.is_empty()
        && data_removed.is_empty()
        && credentials_added.is_empty()
        && credentials_removed.is_empty()
        && high_risk_added.is_empty()
        && high_risk_removed.is_empty());
    UpdateDiff {
        version_from: installed.entry.version.clone(),
        version_to: new_report.plugin_version.clone(),
        permissions_changed,
        data_added,
        data_removed,
        credentials_added,
        credentials_removed,
        high_risk_added,
        high_risk_removed,
        contributions_added,
        contributions_removed,
    }
}

/// 枚举插件根下的一层目录（不跟随符号链接，ADR-0001 第 5 条）。
pub fn plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        out.push(path);
    }
    out.sort();
    out
}

/// 执行一次扫描。`transient` 目录逐一作为单个插件根处理（不写注册表，ADR-0001 第 4 条）。
pub fn scan(
    plugins_dir: &Path,
    transient: &[PathBuf],
    registry: &Registry,
    creds: &CredentialsStore,
) -> ScanOutcome {
    let mut candidates: Vec<(PathBuf, String, AuditReport)> = Vec::new();

    for dir in plugin_dirs(plugins_dir) {
        if !dir.join("plugin.json").is_file() {
            // 非插件目录（例如用户自己的杂项目录）静默跳过
            continue;
        }
        let report = audit::audit_dir(&dir, creds);
        candidates.push((dir, "drop-in".into(), report));
    }
    for dir in transient {
        if !dir.join("plugin.json").is_file() {
            tracing::warn!(dir = %dir.display(), "transient 插件目录缺 plugin.json，忽略");
            continue;
        }
        let report = audit::audit_dir(dir, creds);
        candidates.push((dir.clone(), "transient".into(), report));
    }

    // id 冲突：两个目录声明同 id → 两个都不安装（BR-02 整体拒绝）
    let mut by_id: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (_, _, r)) in candidates.iter().enumerate() {
        if !r.plugin_id.is_empty() {
            by_id.entry(r.plugin_id.clone()).or_default().push(i);
        }
    }
    for (id, idxs) in &by_id {
        if idxs.len() > 1 {
            for &i in idxs {
                let r = &mut candidates[i].2;
                r.errors.push(ManifestIssue::new(
                    "id",
                    "id-conflict",
                    format!("多个目录声明同一插件 id {id}，全部拒绝安装"),
                ));
                r.installable = false;
            }
        }
    }

    // 渲染器 match 全 hub 唯一（B3，ADR-0012 拍板 3）：候选之间 + 与已装插件之间，
    // 冲突即拒（错因可定位：指出占用方）。已装插件经 manifest 快照先占。
    let cand_id = |c: &(PathBuf, String, AuditReport)| -> String {
        if c.2.plugin_id.is_empty() {
            c.0.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            c.2.plugin_id.clone()
        }
    };
    let mut claimed: BTreeMap<(String, String), String> = BTreeMap::new();
    for p in registry.list() {
        for r in &p.manifest.contributes.renderers {
            let owner = p.entry.id.clone();
            for f in &r.match_rule.fence {
                claimed
                    .entry(("fence".into(), f.to_ascii_lowercase()))
                    .or_insert_with(|| owner.clone());
            }
            for e in &r.match_rule.ext {
                claimed
                    .entry((
                        "ext".into(),
                        e.to_ascii_lowercase().trim_start_matches('.').to_string(),
                    ))
                    .or_insert_with(|| owner.clone());
            }
        }
    }
    // 先收集各候选的冲突判定（borrow 拆分：判定阶段全只读，落错阶段可变）
    let conflicts: Vec<(usize, String, String, String)> = candidates
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            c.2.render_claims.iter().map(move |claim| (i, claim))
        })
        .filter_map(|(i, claim)| {
            let key = (claim.kind.clone(), claim.value.clone());
            // B3 观察项承接：update 候选的自撞豁免——占用方 == 候选自身 id
            //（同一插件的新版替换旧版，match 面延续不算冲突）
            let own_id = cand_id(&candidates[i]);
            if claimed.get(&key).map(|o| o.as_str()) == Some(own_id.as_str()) {
                return None;
            }
            let owner = match claimed.get(&key) {
                Some(o) => Some(o.clone()),
                None => candidates.iter().enumerate().find_map(|(j, cj)| {
                    (j != i
                        && cj.2.render_claims.iter().any(|x| x.kind == claim.kind && x.value == claim.value))
                        .then(|| cand_id(cj))
                }),
            };
            owner.map(|o| (i, key.0, key.1, o))
        })
        .collect();
    for (i, kind, value, owner) in conflicts {
        claimed.remove(&(kind.clone(), value.clone()));
        let self_id = cand_id(&candidates[i]);
        let r = &mut candidates[i].2;
        r.errors.push(ManifestIssue::new(
            format!("contributes.renderers.match.{kind}"),
            "renderer-match-conflict",
            format!("{kind} {value:?} 已被插件 {owner:?} 占用（match 全 hub 唯一，ADR-0012 拍板 3）——本插件（{self_id:?}）拒绝安装"),
        ));
        r.installable = false;
    }

    // 主题插件互斥（B3，ADR-0013 拍板 1 确定性规则——第二个主题插件拒装，错因指明占用方）。
    // 候选内互撞：后到的拒；与已装冲突：候选拒。
    {
        let mut theme_owner: Option<String> = None;
        for p in registry.list() {
            if p.manifest.contributes.theme_tokens.is_some() {
                theme_owner = Some(p.entry.id.clone());
                break;
            }
        }
        let mut pending_owner: Option<String> = None;
        for c in candidates.iter_mut() {
            if c.2.plugin_id.is_empty() {
                continue;
            }
            let has_theme = c.2.contributions.iter().any(|cc| cc.kind == "themeTokens");
            if !has_theme {
                continue;
            }
            // 审计失败的候选不占槽（对比度拒装等先落地，槽留给合法主题插件）
            if !c.2.installable {
                continue;
            }
            if let Some(ref owner) = theme_owner.clone().or_else(|| pending_owner.clone()) {
                if cand_id(c) != *owner {
                    c.2.errors.push(ManifestIssue::new(
                        "contributes.themeTokens",
                        "theme-mutex",
                        format!("主题插件已由 {:?} 占用（互斥 1/1，ADR-0013 拍板 1）——本插件（{:?}）拒绝安装", owner, cand_id(c)),
                    ));
                    c.2.installable = false;
                }
            } else {
                pending_owner = Some(cand_id(c));
            }
        }
    }

    // 文件装饰 ext 互斥（B3，ADR-0013 §12.1：ext 全 hub 唯一——同 renderers 同口径）
    {
        let mut file_claimed: BTreeMap<String, String> = BTreeMap::new();
        for p in registry.list() {
            for d in &p.manifest.contributes.file_decorations {
                for e in &d.match_rule.ext {
                    file_claimed
                        .entry(e.to_ascii_lowercase().trim_start_matches('.').to_string())
                        .or_insert_with(|| p.entry.id.clone());
                }
            }
        }
        let mut file_conflicts: Vec<(usize, String, String)> = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            for d in &c.2.contributions {
                if d.kind != "fileDecoration" {
                    continue;
                }
                let ext = d.name.trim_start_matches("ext:").to_string();
                for e in ext.split('|') {
                    let e_l = e.to_ascii_lowercase().trim_start_matches('.').to_string();
                    let owner = match file_claimed.get(&e_l) {
                        Some(o) => Some(o.clone()),
                        None => candidates.iter().enumerate().find_map(|(j, cj)| {
                            (j != i).then(|| {
                                cj.2.contributions.iter().any(|x| {
                                    if x.kind != "fileDecoration" {
                                        return false;
                                    }
                                    let xs: Vec<String> = x
                                        .name
                                        .trim_start_matches("ext:")
                                        .split('|')
                                        .map(|s| s.to_ascii_lowercase().trim_start_matches('.').to_string())
                                        .collect();
                                    xs.contains(&e_l)
                                })
                                .then(|| cand_id(cj))
                            })
                            .flatten()
                        }),
                    };
                    if let Some(owner) = owner {
                        file_conflicts.push((i, e_l, owner));
                    } else {
                        file_claimed.entry(e_l).or_insert_with(|| cand_id(&candidates[i]));
                    }
                }
            }
        }
        for (i, ext, owner) in file_conflicts {
            if candidates[i].2.errors.iter().any(|e| e.rule == "renderer-match-conflict") {
                continue;
            }
            let self_id = cand_id(&candidates[i]);
            let r = &mut candidates[i].2;
            r.errors.push(ManifestIssue::new(
                "contributes.fileDecorations.match.ext",
                "decoration-match-conflict",
                format!("ext {ext:?} 的文件装饰已被 {:?} 占用（ext 全 hub 唯一）——本插件（{self_id:?}）拒绝安装", owner),
            ));
            r.installable = false;
        }
    }

    let mut items = Vec::new();
    for (dir, source_kind, report) in candidates {
        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = if report.plugin_id.is_empty() {
            dir_name.clone()
        } else {
            report.plugin_id.clone()
        };
        let installed = registry.get(&id);
        let installed_hash = installed.as_ref().map(|p| p.entry.content_hash.clone());
        let status = if !report.errors.is_empty() {
            "audit_failed"
        } else if installed.is_none() {
            "new"
        } else if installed_hash.as_deref() != Some(report.content_hash.as_str()) {
            "update"
        } else {
            // 已安装且 hash 一致：不需要用户处理，不进新发现区
            continue;
        };
        // B4 FAIL-2 回修：卸载 + purge 清理登记内的 (id, hash) 不再重发现/重审计
        //（目录按 ADR-0002 保留，但审计产物不随周期扫描无限重生；hash 变化即失效）
        if status == "new" && registry.is_discovery_dismissed(&id, &report.content_hash) {
            continue;
        }
        // B4 BR-13：更新候选的差异面（新版审计投影 vs 已装 manifest 快照）
        let update_diff = if status == "update" {
            installed
                .as_ref()
                .map(|p| compute_update_diff(p, &report))
        } else {
            None
        };
        items.push(DiscoveredItem {
            id,
            dir_name,
            path: dir.to_string_lossy().into_owned(),
            source_kind,
            status: status.into(),
            installed_content_hash: installed_hash,
            update_diff,
            report,
        });
    }

    // 需处理项置前（审计失败优先），其余按 id 稳定排序
    items.sort_by(|a, b| {
        let rank = |s: &str| match s {
            "audit_failed" => 0,
            "update" => 1,
            _ => 2,
        };
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.id.cmp(&b.id))
    });

    ScanOutcome {
        items,
        last_scan_at: super::registry::now_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn creds() -> CredentialsStore {
        CredentialsStore::load(std::env::temp_dir().join("hub-test-no-such-credentials.json"))
    }

    fn write_plugin(root: &Path, dir: &str, id: &str) {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("plugin.json"),
            format!(
                r#"{{"schemaVersion":1,"id":"{id}","name":"{id}","version":"0.1.0","hub":{{"apiVersion":"1"}}}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn discovers_new_plugins_one_level_only() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "a", "plugin-a");
        // 嵌套目录不是插件（一层深）
        write_plugin(&dir.path().join("a"), "nested", "plugin-nested");
        // 非插件目录忽略
        fs::create_dir_all(dir.path().join("random")).unwrap();
        let reg = Registry::open(dir.path().join("plugins.json"));
        let out = scan(dir.path(), &[], &reg, &creds());
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].id, "plugin-a");
        assert_eq!(out.items[0].status, "new");
    }

    #[test]
    fn audit_failed_items_are_marked_and_ordered_first() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "ok", "plugin-ok");
        fs::create_dir_all(dir.path().join("bad")).unwrap();
        fs::write(
            dir.path().join("bad/plugin.json"),
            r#"{"schemaVersion":1,"id":"Bad Id","name":"x","version":"0.1.0","hub":{"apiVersion":"1"}}"#,
        )
        .unwrap();
        let reg = Registry::open(dir.path().join("plugins.json"));
        let out = scan(dir.path(), &[], &reg, &creds());
        assert_eq!(out.items[0].status, "audit_failed");
        assert!(out.items[0]
            .report
            .errors
            .iter()
            .any(|e| e.field == "id" && e.rule == "id-format"));
    }

    #[test]
    fn same_id_in_two_dirs_rejected_entirely() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "one", "same-id");
        write_plugin(dir.path(), "two", "same-id");
        let reg = Registry::open(dir.path().join("plugins.json"));
        let out = scan(dir.path(), &[], &reg, &creds());
        assert_eq!(out.items.len(), 2);
        assert!(out.items.iter().all(|i| i.status == "audit_failed"
            && i.report.errors.iter().any(|e| e.rule == "id-conflict")));
    }

    #[test]
    fn symlinked_plugin_dir_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        write_plugin(outside.path(), "real", "linked-plugin");
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path().join("real"), dir.path().join("link")).unwrap();
        let reg = Registry::open(dir.path().join("plugins.json"));
        let out = scan(dir.path(), &[], &reg, &creds());
        assert!(out.items.is_empty(), "符号链接目录不跟随");
    }

    #[test]
    fn installed_same_hash_is_not_rediscovered() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "a", "plugin-a");
        let reg = Registry::open(dir.path().join("plugins.json"));
        let first = scan(dir.path(), &[], &reg, &creds());
        let report = &first.items[0].report;
        reg.insert_installed(
            super::super::registry::PluginEntry {
                id: "plugin-a".into(),
                version: "0.1.0".into(),
                content_hash: report.content_hash.clone(),
                permissions_hash: "sha256:x".into(),
                source: super::super::registry::PluginSource::drop_in(),
                installed_at: super::super::registry::now_rfc3339(),
                enabled: true,
                state: super::super::registry::PluginState::Ready,
                last_fault: None,
                pending_update: None,
            },
            super::super::manifest::parse(
                &fs::read_to_string(dir.path().join("a/plugin.json")).unwrap(),
            )
            .unwrap()
            .manifest,
            dir.path().join("a"),
        )
        .unwrap();
        let second = scan(dir.path(), &[], &reg, &creds());
        assert!(second.items.is_empty(), "同 hash 已安装不应再进新发现区");
    }
}

#[cfg(test)]
mod b4_tests {
    use super::*;
    use std::fs;

    fn creds() -> CredentialsStore {
        CredentialsStore::load(std::env::temp_dir().join("hub-test-no-such-credentials.json"))
    }

    fn write_plugin(root: &Path, dir: &str, id: &str, version: &str, fence: Option<&str>) {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        let renderer = fence
            .map(|f| format!(
                r#","renderers": [{{"id":"plug_{id}_g","match":{{"fence":["{f}"]}},"engine":"mermaid"}}]"#
            ))
            .unwrap_or_default();
        fs::write(
            d.join("plugin.json"),
            format!(
                r#"{{"schemaVersion":1,"id":"{id}","name":"{id}","version":"{version}","hub":{{"apiVersion":"1"}},"contributes":{{"views":[{{"id":"home","title":"H","entry":"view/index.html"}}]{renderer}}}}}"#
            ),
        )
        .unwrap();
        fs::create_dir_all(d.join("view")).unwrap();
        fs::write(d.join("view/index.html"), "<html></html>").unwrap();
    }

    #[test]
    fn update_candidate_keeping_own_renderer_claim_is_not_self_conflict() {
        // B3 观察项承接：update 候选沿用自身 match 面 = 自撞豁免（B4 修复）
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "a", "rend-p", "0.1.0", Some("gantt"));
        let reg = Registry::open(dir.path().join("plugins.json"));
        // 安装 0.1.0（先占 fence gantt）
        let first = scan(dir.path(), &[], &reg, &creds());
        let item = &first.items[0];
        let m = super::super::manifest::parse(
            &fs::read_to_string(dir.path().join("a/plugin.json")).unwrap(),
        )
        .unwrap()
        .manifest;
        reg.insert_installed(
            super::super::registry::PluginEntry {
                id: "rend-p".into(),
                version: "0.1.0".into(),
                content_hash: item.report.content_hash.clone(),
                permissions_hash: "sha256:x".into(),
                source: super::super::registry::PluginSource::drop_in(),
                installed_at: super::super::registry::now_rfc3339(),
                enabled: true,
                state: super::super::registry::PluginState::Ready,
                last_fault: None,
                pending_update: None,
            },
            m,
            dir.path().join("a"),
        )
        .unwrap();
        // 升级 0.2.0，沿用同一 fence
        write_plugin(dir.path(), "a", "rend-p", "0.2.0", Some("gantt"));
        let second = scan(dir.path(), &[], &reg, &creds());
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].status, "update", "自撞豁免：仍是普通更新候选");
        assert!(
            !second.items[0]
                .report
                .errors
                .iter()
                .any(|e| e.rule == "renderer-match-conflict"),
            "{:?}",
            second.items[0].report.errors
        );
        // 差异面：version 0.1.0 → 0.2.0
        let diff = second.items[0].update_diff.as_ref().expect("update diff");
        assert_eq!(diff.version_from, "0.1.0");
        assert_eq!(diff.version_to, "0.2.0");
        assert!(!diff.permissions_changed, "纯内容更新无权限面变化");
    }
}
