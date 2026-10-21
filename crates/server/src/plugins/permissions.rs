//! 权限面判定（ADR-0007 第 2 条 / 技术设计 §7）。
//!
//! 单一入口 [`check`]：按**调用类别**分派判定（不做一刀切交集）——
//! 数据方法判 `data` 域、涉凭据方法再判 `credentials` 名、高危方法另判 `highRisk`。
//! 未声明即拒绝（BR-12），拒绝由调用方统一写 `Violation` 留痕。

use serde_json::Value;

use super::api_surface::RPC_METHODS;
use super::manifest::Permissions;

/// 一次插件发起的 hub 调用（backend → hub RPC 或视图经宿主转发的 hub.api.* 方法）。
#[derive(Debug, Clone)]
pub struct PluginCall {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyKind {
    /// 方法未在 hub.api 白名单表内（含未注册方法）
    NotDeclared,
    /// 方法要求用户逐次确认的高危项
    HighRiskNeedsApproval,
    /// 凭据名未在声明清单内
    CredentialUnknown,
    /// 数据域未声明
    DataDenied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deny {
    pub kind: DenyKind,
    pub message: String,
}

pub type Verdict = Result<(), Deny>;

/// 方法是否在当前 hub 注册（B1 方法表里 available 的项）。
pub fn method_available(method: &str) -> bool {
    RPC_METHODS
        .iter()
        .any(|m| m.method == method && m.available)
}

/// 判定矩阵（技术设计 §7 表）。
pub fn check(call: &PluginCall, decl: &Permissions) -> Verdict {
    let Some(def) = RPC_METHODS.iter().find(|m| m.method == call.method) else {
        return Err(Deny {
            kind: DenyKind::NotDeclared,
            message: format!("未知 RPC 方法 {}（扩展 API 白名单之外）", call.method),
        });
    };
    if !def.available {
        return Err(Deny {
            kind: DenyKind::NotDeclared,
            message: format!("RPC 方法 {} 尚未在本 hub 版本注册", call.method),
        });
    }

    // 1) 数据域：所有绑定域都须声明
    for domain in def.data_domains {
        if !decl.data.iter().any(|d| d == domain) {
            return Err(Deny {
                kind: DenyKind::DataDenied,
                message: format!("方法 {} 需要数据域 {domain}，插件未声明", call.method),
            });
        }
    }

    // 2) 凭据名：`<name>` 占位按调用参数里的凭据名判定
    for c in def.credentials {
        if *c == "<name>" {
            let Some(name) = call.params.get("name").and_then(Value::as_str) else {
                return Err(Deny {
                    kind: DenyKind::CredentialUnknown,
                    message: "hub.credentials.use 缺少 name 参数".into(),
                });
            };
            if !decl.credentials.iter().any(|d| d == name) {
                return Err(Deny {
                    kind: DenyKind::CredentialUnknown,
                    message: format!("凭据 {name} 未在 manifest 中声明"),
                });
            }
        } else if !decl.credentials.iter().any(|d| d == c) {
            return Err(Deny {
                kind: DenyKind::CredentialUnknown,
                message: format!("凭据 {c} 未在 manifest 中声明"),
            });
        }
    }

    // 3) 高危项：表标高危的方法一律逐次确认（高危四项归 hub 维护，判定与插件
    //    声明无关——声明一致性由审计期 M9 保障，运行期判定只看方法表）。
    //    B3 批 3 修复轮（F-12）：M7 清死逻辑时把本段一并删除，致 hub.tasks.toggle
    //    直通翻转无审批门——恢复判定，handle_call 的 approvals 分支重新可达。
    if !def.high_risk.is_empty() {
        let label = def
            .high_risk
            .iter()
            .map(|i| super::api_surface::high_risk_label(i))
            .collect::<Vec<_>>()
            .join("、");
        return Err(Deny {
            kind: DenyKind::HighRiskNeedsApproval,
            message: format!("方法 {} 属高危项（{label}），需逐次确认", call.method),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl() -> Permissions {
        Permissions {
            api: vec![],
            data: vec!["projects:read".into()],
            credentials: vec!["gitlab_pat".into()],
            high_risk: vec![],
        }
    }

    #[test]
    fn credential_declared_is_allowed() {
        let call = PluginCall {
            method: "hub.credentials.use".into(),
            params: json!({ "name": "gitlab_pat" }),
        };
        assert_eq!(check(&call, &decl()), Ok(()));
    }

    #[test]
    fn credential_not_declared_is_denied() {
        let call = PluginCall {
            method: "hub.credentials.use".into(),
            params: json!({ "name": "other_pat" }),
        };
        let deny = check(&call, &decl()).unwrap_err();
        assert_eq!(deny.kind, DenyKind::CredentialUnknown);
    }

    #[test]
    fn data_domain_declared_is_allowed() {
        let call = PluginCall {
            method: "hub.projects.list".into(),
            params: json!({}),
        };
        assert_eq!(check(&call, &decl()), Ok(()));
    }

    #[test]
    fn projects_effective_maps_projects_read() {
        let call = PluginCall {
            method: "hub.projects.effective".into(),
            params: json!({}),
        };
        assert_eq!(check(&call, &decl()), Ok(()));
        let mut d = decl();
        d.data.clear();
        assert_eq!(check(&call, &d).unwrap_err().kind, DenyKind::DataDenied);
    }

    #[test]
    fn data_domain_missing_is_denied() {
        let mut d = decl();
        d.data.clear();
        let call = PluginCall {
            method: "hub.projects.list".into(),
            params: json!({}),
        };
        assert_eq!(check(&call, &d).unwrap_err().kind, DenyKind::DataDenied);
    }

    #[test]
    fn unknown_method_is_denied() {
        let call = PluginCall {
            method: "hub.evil.call".into(),
            params: json!({}),
        };
        assert_eq!(
            check(&call, &decl()).unwrap_err().kind,
            DenyKind::NotDeclared
        );
    }

    #[test]
    fn unregistered_v1_method_is_denied() {
        let call = PluginCall {
            method: "hub.sessions.list".into(),
            params: json!({}),
        };
        let deny = check(&call, &decl()).unwrap_err();
        assert_eq!(deny.kind, DenyKind::NotDeclared);
        assert!(deny.message.contains("尚未在本 hub 版本注册"));
    }

    #[test]
    fn high_risk_method_needs_approval_regardless_of_declaration() {
        // F-12 回归锁：表标高危即需确认（判定与 decl.high_risk 无关——一致性由 M9 审计保障）
        let mut d = decl();
        d.data.push("tasks:write".into());
        d.high_risk.clear(); // 插件声明缺失也不影响判定
        let call = PluginCall {
            method: "hub.tasks.toggle".into(),
            params: json!({ "id": "t1" }),
        };
        let deny = check(&call, &d).unwrap_err();
        assert_eq!(deny.kind, DenyKind::HighRiskNeedsApproval);
        assert!(deny.message.contains("写既有会话内容"));
    }

    #[test]
    fn storage_and_log_need_no_declaration() {
        let empty = Permissions::default();
        for m in ["hub.storage.get", "hub.storage.set", "hub.log.emit"] {
            let call = PluginCall {
                method: m.into(),
                params: json!({}),
            };
            assert_eq!(check(&call, &empty), Ok(()), "{m} 应免声明");
        }
    }
}
