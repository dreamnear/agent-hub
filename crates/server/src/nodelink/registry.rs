//! 节点注册表 + enrollment 状态机（agent-hub-gateway-node-p1 任务 4）。
//!
//! - **nodes.json**（`~/.claude-view/nodes.json`，env `AGENT_HUB_NODES_FILE` 覆盖；
//!   0600 原子落盘）：节点身份与状态（pending/approved/rejected）+ token 哈希
//!   （sha256，**零明文**——明文只在签发响应与握手瞬间存在）。网关重启凭它重建
//!   （N4 网关半边）。
//! - **enrollment 状态机**（仅内存，网关重启失效需重新生成——P1 接受，见任务
//!   风险 7）：`active（未用）→ pending（节点首连挂起审批，code 未消费）→
//!   exchanged（审批后节点持同一 code 重连换取 node token，换取即消费）`；
//!   expired / rejected 终态在校验入口显式分类。code 明文只返回一次、只存哈希
//!   （N5：二次使用 / 过期 / 未知 显式拒绝）。

use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use super::sha256_hex;
use crate::platform;

/// enrollment 有效期（proposal §4：10 分钟单次）
pub const ENROLL_TTL: Duration = Duration::from_secs(10 * 60);

/// 节点记录状态（snake_case 序列化）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Approved,
    Rejected,
}

/// 节点记录（nodes.json 条目；camelCase 对齐 instances.json 惯例）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRecord {
    pub node_id: String,
    pub node_name: String,
    pub status: NodeStatus,
    /// node token 的 sha256 hex——**唯一落盘形态，零明文**（None = 已批准未换取）
    #[serde(default)]
    pub token_hash: Option<String>,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub drivers: Vec<String>,
    pub created_at: String,
    #[serde(default)]
    pub last_seen: Option<String>,
    /// host stats presence（P3 任务 3）：旧节点不带 → None 如实缺失（N-1 窗口）
    #[serde(default)]
    pub stats: Option<crate::nodelink::HostStats>,
}

/// enrollment（仅内存，不落盘）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrollment {
    /// code 的 sha256——明文只在 enroll-code 响应出现一次
    pub code_hash: String,
    pub expires_at: SystemTime,
    pub state: EnrollState,
}

/// enrollment 使用状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollState {
    Active,
    Pending,
    Exchanged,
}

/// enrollment 校验失败分类（统一入口显式拒绝，N5）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollError {
    Unknown,
    Expired,
    AlreadyUsed,
}

/// token 校验结果（握手分类依据）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCheck {
    Verified,
    /// 记录状态 rejected → 回 Rejected 帧（终态语义，区别于凭据错误）
    Rejected,
    /// 记录缺失 / 未批准 / 哈希不匹配 → 回 NodeTokenRejected 帧
    Invalid,
}

/// approve 失败分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveError {
    Unknown,
    AlreadyRejected,
}

fn now_rfc3339() -> String {
    // 简化时间戳：登记/可见性用途，非密码学事实
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

pub struct NodeRegistry {
    file: PathBuf,
    nodes: Mutex<Vec<NodeRecord>>,
    enrollments: Mutex<Vec<Enrollment>>,
}

impl NodeRegistry {
    pub fn new(file: PathBuf) -> Self {
        let nodes = match fs::read_to_string(&file) {
            Ok(raw) => serde_json::from_str::<Vec<NodeRecord>>(&raw).unwrap_or_else(|e| {
                tracing::warn!(path = %file.display(), error = %e, "nodes.json 解析失败，按空表处理");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        tracing::info!(path = %file.display(), count = nodes.len(), "节点注册表已加载");
        Self {
            file,
            nodes: Mutex::new(nodes),
            enrollments: Mutex::new(Vec::new()),
        }
    }

    /// 原子落盘 0600（同 instances.json 惯例；只含 token 哈希，零明文）。
    fn save(&self, nodes: &[NodeRecord]) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)?;
        }
        let raw = serde_json::to_string_pretty(nodes)?;
        let tmp = self.file.with_extension("json.tmp");
        fs::write(&tmp, &raw)?;
        fs::rename(&tmp, &self.file)?;
        platform::harden_0600(&self.file);
        Ok(())
    }

    // ===== 节点记录 =====

    pub fn list(&self) -> Vec<NodeRecord> {
        self.nodes.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn get(&self, node_id: &str) -> Option<NodeRecord> {
        self.nodes
            .lock()
            .ok()?
            .iter()
            .find(|n| n.node_id == node_id)
            .cloned()
    }

    /// 首连挂起：记录不存在或 pending 时创建/刷新 pending（非敏感元数据随 hello
    /// 刷新）；已 approved / rejected 的记录不被本方法改写（调用方分流）。
    pub fn upsert_pending(
        &self,
        node_id: &str,
        node_name: &str,
        platform: &str,
        drivers: &[String],
        stats: Option<crate::nodelink::HostStats>,
    ) -> NodeStatus {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = g.iter_mut().find(|n| n.node_id == node_id) {
            slot.node_name = node_name.to_string();
            slot.platform = platform.to_string();
            slot.drivers = drivers.to_vec();
            slot.stats = stats;
            if slot.status == NodeStatus::Pending {
                slot.last_seen = Some(now_rfc3339());
            }
            let status = slot.status;
            let _ = self.save(&g);
            return status;
        }
        g.push(NodeRecord {
            node_id: node_id.to_string(),
            node_name: node_name.to_string(),
            status: NodeStatus::Pending,
            token_hash: None,
            platform: platform.to_string(),
            drivers: drivers.to_vec(),
            created_at: now_rfc3339(),
            last_seen: Some(now_rfc3339()),
            stats,
        });
        let _ = self.save(&g);
        NodeStatus::Pending
    }

    /// 审批：pending→approved；重复 approve 幂等；rejected 不可逆（N2 终态）。
    pub fn approve(&self, node_id: &str) -> Result<(), ApproveError> {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let slot = g
            .iter_mut()
            .find(|n| n.node_id == node_id)
            .ok_or(ApproveError::Unknown)?;
        match slot.status {
            NodeStatus::Rejected => return Err(ApproveError::AlreadyRejected),
            NodeStatus::Approved => return Ok(()),
            NodeStatus::Pending => {}
        }
        slot.status = NodeStatus::Approved;
        let _ = self.save(&g);
        Ok(())
    }

    /// 拒绝：任意状态→rejected（终态）；清除 token 哈希（卫生）。
    pub fn reject(&self, node_id: &str) -> bool {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        match g.iter_mut().find(|n| n.node_id == node_id) {
            Some(slot) => {
                slot.status = NodeStatus::Rejected;
                slot.token_hash = None;
                let _ = self.save(&g);
                true
            }
            None => false,
        }
    }

    /// 签发 node token：存哈希（零明文），明文由调用方经握手响应回给节点。
    pub fn issue_token(&self, node_id: &str, token_plain: &str) {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = g.iter_mut().find(|n| n.node_id == node_id) {
            slot.token_hash = Some(sha256_hex(token_plain));
            let _ = self.save(&g);
        }
    }

    /// 读取当前 token 哈希（sweep S1 轮换回滚：签发前留底快照）。
    pub fn token_hash(&self, node_id: &str) -> Option<String> {
        let g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        g.iter().find(|n| n.node_id == node_id)?.token_hash.clone()
    }

    /// 回滚 token 哈希（sweep S1 NAK/超时路径）：恢复轮换前快照
    /// （None = 清除）——节点磁盘凭据未变时网关侧同步还原，双向一致。
    pub fn restore_token_hash(&self, node_id: &str, hash: Option<String>) {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = g.iter_mut().find(|n| n.node_id == node_id) {
            slot.token_hash = hash;
            let _ = self.save(&g);
        }
    }

    /// token 校验（握手 token 路径）：哈希常量时间比对。
    pub fn verify_token(&self, node_id: &str, token_plain: &str) -> TokenCheck {
        let g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = g.iter().find(|n| n.node_id == node_id) else {
            return TokenCheck::Invalid;
        };
        match record.status {
            NodeStatus::Rejected => return TokenCheck::Rejected,
            NodeStatus::Pending => return TokenCheck::Invalid,
            NodeStatus::Approved => {}
        }
        let Some(hash) = &record.token_hash else {
            return TokenCheck::Invalid; // 已批准未换取：无凭据可比
        };
        if super::constant_time_eq_str(hash, &sha256_hex(token_plain)) {
            TokenCheck::Verified
        } else {
            TokenCheck::Invalid
        }
    }

    /// 已认证连接的 presence 刷新（platform/drivers/last_seen 非敏感元数据）。
    pub fn refresh_presence(&self, node_id: &str, platform: &str, drivers: &[String]) {
        self.refresh_presence_with_stats(node_id, platform, drivers, None);
    }

    /// presence 刷新 + host stats（P3 任务 3）：announce 随握手/announce 帧更新。
    pub fn refresh_presence_with_stats(
        &self,
        node_id: &str,
        platform: &str,
        drivers: &[String],
        stats: Option<crate::nodelink::HostStats>,
    ) {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = g.iter_mut().find(|n| n.node_id == node_id) {
            slot.platform = platform.to_string();
            slot.drivers = drivers.to_vec();
            slot.stats = stats;
            slot.last_seen = Some(now_rfc3339());
            let _ = self.save(&g);
        }
    }

    /// 改名（P3 M5）：registry 记录名 + 实例表名同步由调用方（api 层）承担；
    /// 未知节点 Err。
    pub fn rename(&self, node_id: &str, node_name: &str) -> Result<(), ApproveError> {
        let mut g = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let slot = g
            .iter_mut()
            .find(|n| n.node_id == node_id)
            .ok_or(ApproveError::Unknown)?;
        slot.node_name = node_name.to_string();
        let _ = self.save(&g);
        Ok(())
    }

    // ===== enrollment 状态机 =====

    /// 生成 enrollment code（明文只经本返回值出现一次；只存哈希不落盘）。
    pub fn create_enrollment(&self, now: SystemTime) -> (String, SystemTime) {
        let code = format!("enr-{}", uuid::Uuid::new_v4());
        let expires_at = now + ENROLL_TTL;
        self.enrollments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Enrollment {
                code_hash: sha256_hex(&code),
                expires_at,
                state: EnrollState::Active,
            });
        (code, expires_at)
    }

    /// 统一校验入口（N5）：未知 / 过期 / 已消费 显式分类拒绝。
    pub fn enroll_validate(&self, code: &str, now: SystemTime) -> Result<(), EnrollError> {
        let hash = sha256_hex(code);
        let g = self.enrollments.lock().unwrap_or_else(|e| e.into_inner());
        let Some(e) = g.iter().find(|e| e.code_hash == hash) else {
            return Err(EnrollError::Unknown);
        };
        if e.state == EnrollState::Exchanged {
            return Err(EnrollError::AlreadyUsed);
        }
        // 有效窗 [创建, expires_at)；到期时刻即过期（TTL 边界单测锚）
        if e.expires_at <= now {
            return Err(EnrollError::Expired);
        }
        Ok(())
    }

    /// active→pending（节点首连挂起审批；code 未消费，审批后仍可换取）。
    pub fn enroll_mark_pending(&self, code: &str) {
        let hash = sha256_hex(code);
        let mut g = self.enrollments.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.iter_mut().find(|e| e.code_hash == hash) {
            if e.state == EnrollState::Active {
                e.state = EnrollState::Pending;
            }
        }
    }

    /// 换取即消费（→ exchanged）：此后同一 code 再校验 = AlreadyUsed（N5 锚）。
    pub fn enroll_consume(&self, code: &str) {
        let hash = sha256_hex(code);
        let mut g = self.enrollments.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.iter_mut().find(|e| e.code_hash == hash) {
            e.state = EnrollState::Exchanged;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (NodeRegistry, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reg = NodeRegistry::new(dir.path().join("nodes.json"));
        (reg, dir)
    }

    fn epoch(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// enrollment 六态流转：active → pending → exchanged；过期 / 未知 / 二次使用
    /// 显式分类拒绝（N5）
    #[test]
    fn enrollment_state_machine() {
        let (reg, _dir) = registry();
        let t0 = epoch(1_000_000);
        let (code, expires) = reg.create_enrollment(t0);
        assert_eq!(expires, t0 + ENROLL_TTL);
        // active：校验通过
        reg.enroll_validate(&code, t0).unwrap();
        reg.enroll_validate(&code, t0 + ENROLL_TTL - Duration::from_secs(1))
            .unwrap();
        // 未知 code
        assert_eq!(
            reg.enroll_validate("enr-unknown", t0),
            Err(EnrollError::Unknown)
        );
        // active → pending（首连挂起；code 未消费仍可校验）
        reg.enroll_mark_pending(&code);
        reg.enroll_validate(&code, t0).unwrap();
        // 过期：TTL 边界外显式 Expired
        assert_eq!(
            reg.enroll_validate(&code, t0 + ENROLL_TTL),
            Err(EnrollError::Expired)
        );
        // 回到有效窗口，pending → exchanged（换取即消费）
        reg.enroll_consume(&code);
        assert_eq!(
            reg.enroll_validate(&code, t0),
            Err(EnrollError::AlreadyUsed),
            "N5：exchanged 后同一 code 再校验拒绝"
        );
    }

    /// pending→approved→签发 token→token 校验；rejected 终态不可逆
    #[test]
    fn node_lifecycle_and_token_check() {
        let (reg, _dir) = registry();
        let t0 = epoch(1_000_000);
        let (code, _) = reg.create_enrollment(t0);
        // 首连挂起
        reg.enroll_validate(&code, t0).unwrap();
        assert_eq!(
            reg.upsert_pending("n-1", "lab", "macos", &["claude".into()], None),
            NodeStatus::Pending
        );
        assert_eq!(reg.get("n-1").unwrap().status, NodeStatus::Pending);
        // 未批准节点 token 校验 = Invalid（无凭据可比）
        assert_eq!(reg.verify_token("n-1", "tok"), TokenCheck::Invalid);
        // 审批 + 换取
        reg.approve("n-1").unwrap();
        assert_eq!(reg.approve("n-1"), Ok(()), "重复 approve 幂等");
        reg.enroll_consume(&code);
        reg.issue_token("n-1", "sek-tok-1");
        assert_eq!(reg.verify_token("n-1", "sek-tok-1"), TokenCheck::Verified);
        assert_eq!(reg.verify_token("n-1", "wrong"), TokenCheck::Invalid);
        assert_eq!(reg.verify_token("n-x", "sek-tok-1"), TokenCheck::Invalid);
        // reject 终态：token 校验走 Rejected（区别于凭据错误），approve 不可逆
        assert!(reg.reject("n-1"));
        assert_eq!(reg.verify_token("n-1", "sek-tok-1"), TokenCheck::Rejected);
        assert_eq!(
            reg.approve("n-1"),
            Err(ApproveError::AlreadyRejected),
            "rejected 不可再批准（N2 终态）"
        );
        assert_eq!(reg.approve("n-x"), Err(ApproveError::Unknown));
    }

    /// N5/N2 落盘红线：nodes.json 0600 且**序列化文本零 token 明文**（只含哈希）
    #[test]
    fn nodes_json_is_0600_and_zero_plaintext() {
        let (reg, dir) = registry();
        let file = dir.path().join("nodes.json");
        let t0 = epoch(1_000_000);
        let (code, _) = reg.create_enrollment(t0);
        reg.upsert_pending(
            "n-1",
            "lab",
            "macos-aarch64",
            &["claude".into(), "dsh".into()],
            None,
        );
        reg.approve("n-1").unwrap();
        reg.enroll_consume(&code);
        reg.issue_token("n-1", "sek-tok-plaintext-999");
        let raw = fs::read_to_string(&file).unwrap();
        assert!(
            !raw.contains("sek-tok-plaintext-999"),
            "nodes.json 不得含 token 明文: {raw}"
        );
        assert!(
            raw.contains(&sha256_hex("sek-tok-plaintext-999")),
            "哈希为唯一落盘形态: {raw}"
        );
        assert_eq!(platform::file_mode(&file), Some(0o600));
        // 重启重建（N4 网关半边）：同文件 new 回读 approved + 哈希
        let reg2 = NodeRegistry::new(file.clone());
        let rec = reg2.get("n-1").unwrap();
        assert_eq!(rec.status, NodeStatus::Approved);
        assert_eq!(
            reg2.verify_token("n-1", "sek-tok-plaintext-999"),
            TokenCheck::Verified
        );
    }

    /// 注册表 CRUD 往返：list/get/presence 刷新；enrollment 仅内存（重建即失）
    #[test]
    fn registry_roundtrip_and_volatile_enrollments() {
        let (reg, dir) = registry();
        let file = dir.path().join("nodes.json");
        reg.upsert_pending("n-1", "lab", "macos", &["claude".into()], None);
        reg.refresh_presence("n-1", "linux-x86_64", &["claude".into(), "omp".into()]);
        let rec = reg.get("n-1").unwrap();
        assert_eq!(rec.platform, "linux-x86_64");
        assert_eq!(rec.drivers, vec!["claude".to_string(), "omp".to_string()]);
        assert_eq!(reg.list().len(), 1);
        // enrollment 仅内存：重建后 code 未知（网关重启需重新生成，P1 接受）
        let t0 = epoch(1_000_000);
        let (code, _) = reg.create_enrollment(t0);
        let reg2 = NodeRegistry::new(file);
        assert_eq!(
            reg2.enroll_validate(&code, t0),
            Err(EnrollError::Unknown),
            "enrollment 不落盘（任务 4 风险 7 定版）"
        );
    }
}
