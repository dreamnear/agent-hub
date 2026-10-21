pub mod attach;
pub mod cli;
pub mod jobs;
pub mod send;
pub mod session;
pub mod subagents;
pub mod tasks;
pub mod watcher;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::{anyhow, Result};
use futures::future::BoxFuture;
use tokio::sync::broadcast;

use crate::drivers::{
    events::{ChatEvent, MessagePage},
    unsupported, AgentCatalog, AgentDriver, SessionControl, SessionHistory, StartSpec,
};
use crate::models::{AgentSummary, ChatMessage};
use send::ClaudeSendRouter;
use subagents::SubagentEntry;
use tasks::TaskEntry;

/// claude bg 驱动门面（driver-unify）：实现统一 AgentDriver 三面。
/// jsonl 解析、状态判定（含 r69 中断覆盖状态机）、PTY 注入/中断、cli 封装、房间/tail
/// 全在本驱动内——app 层只见 AgentSummary/MessagePage/ChatEvent 三种结构。
#[derive(Clone)]
pub struct ClaudeDriver {
    pub bin: PathBuf,
    pub jobs_dir: PathBuf,
    /// 会话 jsonl 根（= config.claude_root；session/subagents/tasks 读取口径）
    pub projects_dir: PathBuf,
    /// 会话级 MCP config 目录（B2：`plugin_mcp`，每会话一 JSON，不写 `~/.claude`）。
    pub plugin_mcp_dir: PathBuf,
    /// hub 二进制路径（B2：注入 `hub-plugins` MCP server 的 command）。
    pub hub_bin: PathBuf,
    /// hub loopback 地址（B2 批3：注入代理 env `AGENT_HUB_LOOPBACK`，高危审批
    /// 落单/轮询用；loopback 免 token，不下发凭据）。
    pub hub_loopback: String,
    /// 注入/中断 PTY 路由（SessionControl 委托）。与 AppState.message_routes 的
    /// "claude" 条目同一实例、共享 attaches 表——不得另建（双实例 = 双重 attach，
    /// 破坏 TUI 注入时序）。
    pub sender: Arc<ClaudeSendRouter>,
    /// chat 房间表（SessionHistory::ensure_*_room 用；与 AppState.chat_rooms 同表）。
    pub rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    /// 中断标记（反馈轮 19，r69 状态机自 AppState 内化 2.3）：agentId → 中断收尾
    /// 落定后的 jsonl mtime 基线。CLI 中断后 state 滞留 working（r49 实测 3 分钟+），
    /// list 时覆盖为空闲，防发送按钮锁死；出现新活动（mtime 前进且 30s 活跃窗内）
    /// 或 state 真实离开工作态即解除，恢复权威 group。
    pub interrupted_at: Arc<std::sync::Mutex<HashMap<String, std::time::SystemTime>>>,
    /// interactive「从列表移除」忽略清单文件（R8）：按 sessionId 持久过滤前台
    /// 会话条目；进程消失后懒清理
    pub ignored_interactive_file: PathBuf,
    /// F-6：会话 id → 注入 config 实际路径（占位名 `hub-<uuid>.json`，原地重写
    /// 保留）——stop/remove 按此回收文件。hub 重启丢映射 → plugin_mcp 目录残留
    /// 死文件（无害：config 只在 mcp-proxy 拉起时读一次），治理归 B4。
    pub mcp_config_paths: Arc<std::sync::Mutex<HashMap<String, PathBuf>>>,
}

impl ClaudeDriver {
    /// 装配构造（中断标记表与注入路由 sender 随驱动自建——api 层不感知驱动内部
    /// 结构，2.7）。attaches 表必须唯一：轨一 message_routes 的 claude 条目与轨二
    /// 本驱动共享同一 sender（双实例 = 双重 attach 破坏 TUI 注入时序）。
    pub fn new(
        bin: PathBuf,
        jobs_dir: PathBuf,
        projects_dir: PathBuf,
        ignored_interactive_file: PathBuf,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    ) -> Self {
        Self::new_with_mcp(
            bin,
            jobs_dir,
            projects_dir,
            ignored_interactive_file,
            rooms,
            PathBuf::from(format!(
                "{}/.claude-view/plugin_mcp",
                crate::platform::home_dir_str()
            )),
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("server")),
            loopback_default(),
        )
    }

    pub fn new_with_mcp(
        bin: PathBuf,
        jobs_dir: PathBuf,
        projects_dir: PathBuf,
        ignored_interactive_file: PathBuf,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
        plugin_mcp_dir: PathBuf,
        hub_bin: PathBuf,
        hub_loopback: String,
    ) -> Self {
        let sender = Arc::new(ClaudeSendRouter::new(bin.clone()));
        Self {
            bin,
            jobs_dir,
            projects_dir,
            plugin_mcp_dir,
            hub_bin,
            hub_loopback,
            ignored_interactive_file,
            sender,
            rooms,
            interrupted_at: Arc::new(std::sync::Mutex::new(HashMap::new())),
            mcp_config_paths: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 轨一 sender 句柄（AppState.message_routes["claude"] 与本驱动同实例）。
    pub fn sender_handle(&self) -> Arc<dyn crate::drivers::MessageSender> {
        self.sender.clone()
    }

    /// CLI 列表为基座（driver="claude"）；id 命中 jobs 目录时增强 detail/tokens，
    /// name/sessionId/cwd 冲突以 CLI 为准；jobs 读取失败静默降级为仅 CLI 信息。
    /// group 归一（CLI state → 三态）在此完成，api 层只见成品（1.3）。
    pub async fn list(&self, all: bool) -> Result<Vec<AgentSummary>> {
        // 活跃清单（CLI 缺省口径）恒取：它是 daemon 活性的权威判据——`done` ≠
        // 退出（活 daemon 停在提示符时 state 也是 done，2026-10-02 反例实测），
        // 只有 --all 独有的条目才是 daemon 已死的真历史。
        // 清单查询 = 只读探询，任何 CLI 失败形态（未安装 / 注册表未初始化 / 命令
        // 失败）一律降级空清单 + WARN——gateway-node「新机器首跑」场景 CLI 侧
        // 注册表缺失会非零退出，此前传播为 500（agent-hub-gateway-first-run R1
        // 定版：200 空；决策变更依据 requirement.md，原「命令失败照常报错」废弃）。
        // 写路径（spawn/send）错误不受影响照常报错。
        let active_list = match cli::agents_json(&self.bin, false).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    bin = %self.bin.display(),
                    error = %e,
                    "claude agents --json 失败（首跑未初始化或 CLI 不可用？），后台任务列表按空处理"
                );
                Vec::new()
            }
        };
        let all_list = if all {
            match cli::agents_json(&self.bin, true).await {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(
                        bin = %self.bin.display(),
                        error = %e,
                        "claude agents --json --all 失败，历史清单按空处理"
                    );
                    Some(Vec::new())
                }
            }
        } else {
            None
        };
        let alive_ids: std::collections::HashSet<String> =
            active_list.iter().map(|c| c.id.clone()).collect();
        let base = all_list.unwrap_or(active_list);
        // R8 interactive「从列表移除」：忽略清单过滤 + 懒清理（进程消失 = 记录清除；
        // CLI 抖动误清的代价只是条目复活、用户再移除一次，宁可复活不丢条目）
        let mut ignored = read_ignored_interactive(&self.ignored_interactive_file);
        let before = ignored.len();
        let base = cli::filter_ignored(base, &mut ignored);
        if ignored.len() != before {
            if let Err(e) = write_ignored_interactive(&self.ignored_interactive_file, &ignored) {
                tracing::warn!(error = %e, "忽略清单懒清理回写失败，跳过（下轮重试）");
            }
        }

        let mut out = Vec::with_capacity(base.len());
        for c in base {
            let alive = alive_ids.contains(&c.id);
            let (detail, tokens) = match jobs::read_job_state(&self.jobs_dir, &c.id).await {
                Some(job) => (job.detail, job.tokens),
                None => (None, None),
            };
            // 当前上下文占用：转录尾部最后一条 assistant usage（与累计 tokens 语义不同，
            // 见 last_context_tokens 注释）；无 session_id/转录 → None（fail-soft）
            let context_tokens = match &c.session_id {
                Some(sid) => session::last_context_tokens(&self.projects_dir, sid).await,
                None => None,
            };
            // done ≠ 退出：活 daemon 的 done（TUI 停在提示符等输入）→ Idle 组照常
            // 显示；daemon 已死（--all 独有）的 done/exited → Completed（历史）。
            let group = match c.state.as_deref() {
                Some("done") if alive => crate::models::Group::Other,
                _ => crate::models::map_group(c.state.as_deref().unwrap_or("")),
            };
            // R7 会话粘连修复：interactive 前台会话（用户终端手跑）无 daemon id，
            // 空 id 透传会在前端选中键上塌陷（多条同键恒同选中）。合成稳定 id
            // `itv-<sessionId前12>`；无 session_id 的 interactive 条目连转录都
            // 定位不了，丢弃（展示无意义）。
            let id = if c.id.is_empty() {
                match c.session_id.as_deref().filter(|s| !s.is_empty()) {
                    Some(sid) => cli::synthesize_interactive_id(sid),
                    None => continue,
                }
            } else {
                c.id
            };
            out.push(AgentSummary {
                driver: "claude".into(),
                id,
                name: c.name,
                cwd: c.cwd,
                kind: c.kind,
                group,
                raw_state: c.state,
                detail,
                tokens,
                context_tokens,
                started_at: c.started_at,
                session_id: c.session_id,
            });
        }
        self.apply_interrupt_overrides(&mut out).await;
        // Done 不进默认侧栏（2026-10-02 全驱动展示纪律）：仅「daemon 已死」的
        // Completed（--all 独有条目）被过滤——活 done 已映射 Other 不受影响。
        // all=true（历史面板 opt-in）保留全量。
        if !all {
            out.retain(|a| a.group != crate::models::Group::Completed);
        }
        Ok(out)
    }

    /// list 后应用中断标记（r69 状态机，自 AppState::apply_interrupt_overrides 逐行
    /// 平移，2.3 内化；三段式：锁内快照 → 锁外 async 判据 → 锁内应用，std Mutex
    /// 不跨 await）。判据与时序常量冻结，不得顺手优化（K1）。
    async fn apply_interrupt_overrides(&self, agents: &mut [AgentSummary]) {
        use crate::models::Group;

        let marked: Vec<(String, std::time::SystemTime)> = {
            let map = self.interrupted_at.lock().expect("interrupted_at lock");
            agents
                .iter()
                .filter_map(|a| map.get(&a.id).map(|t| (a.id.clone(), *t)))
                .collect()
        };
        if marked.is_empty() {
            return;
        }

        // 解除判据：state 真实离开工作态；或 jsonl 持续新写入（mtime 越过基线
        // 且在 30s 活跃窗内——一次性中断收尾写入不满足，流式输出才满足）
        let mut relieved: Vec<String> = Vec::new();
        for (id, baseline) in &marked {
            let Some(a) = agents.iter().find(|a| &a.id == id) else {
                continue;
            };
            if !matches!(a.group, Group::Working | Group::NeedsInput) {
                relieved.push(id.clone());
                continue;
            }
            if let Some(sid) = a.session_id.as_deref() {
                let mtime = session::session_mtime(&self.projects_dir, sid).await;
                let active = session::session_is_active(&self.projects_dir, sid, 30).await;
                if mtime.is_some_and(|m| m > *baseline) && active {
                    relieved.push(id.clone());
                }
            }
        }

        let mut map = self.interrupted_at.lock().expect("interrupted_at lock");
        for id in &relieved {
            map.remove(id);
        }
        for a in agents.iter_mut() {
            if map.contains_key(&a.id) && matches!(a.group, Group::Working | Group::NeedsInput) {
                a.group = Group::Other;
            }
        }
    }

    /// 注入后落盘验证（反馈轮 28-A，2.4 自 api/messages.rs 内化）：3s 后 jsonl
    /// mtime 未推进 = 消息可能未被 CLI 处理（TUI 未渲染/模态吞输入）。后台任务不
    /// 阻塞响应；working 排队正常也 >3s，故日志措辞区分「待核对」而非定论丢失。
    /// 日志文案为 r25/r28 取证锚点，逐字保留。
    async fn spawn_send_post_check(&self, id: &str) {
        let Some(session_id) = ClaudeDriver::get(self, id)
            .await
            .ok()
            .flatten()
            .and_then(|a| a.session_id)
        else {
            return;
        };
        let root = self.projects_dir.clone();
        let id2 = id.to_string();
        let before = session::session_mtime(&root, &session_id).await;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let after = session::session_mtime(&root, &session_id).await;
            match (before, after) {
                (Some(b), Some(a)) if b == a => {
                    tracing::warn!(
                        agent = %id2,
                        session = %session_id,
                        "send post-check: jsonl 3s 未推进（working 排队中或注入丢失，需核对）"
                    );
                }
                (None, None) => {
                    tracing::warn!(agent = %id2, session = %session_id, "send post-check: 找不到会话 jsonl，无法验证落盘");
                }
                _ => {}
            }
        });
    }

    /// 精确查询（AgentCatalog::get 实体）。设计判据为「不依赖全表扫」，但 claude CLI
    /// 无 by-id 原语（agents --json 全量；jobs state.json 缺 kind/started_at，无法
    /// 独立成 summary）——行为等价红线优先：扫描内化在驱动内，api 层不再自带
    /// list+find。CLI 出 by-id 原语后此实现一行换掉。
    /// ponytail: 全表扫每端点一次的成本与批前 api agent_of 相同，非新增开销。
    pub async fn get(&self, id: &str) -> Result<Option<AgentSummary>> {
        let agents = self.list(true).await?;
        Ok(agents.into_iter().find(|a| a.id == id))
    }

    /// subagent 清单共用实现：面板默认只列活跃条目（2026-10-01 产品纪律——历史
    /// 条目默认不进面板），include_finished=true 为历史入口的显式 opt-in 全量
    /// 口径（数据不删）。
    async fn subagents_impl(&self, id: &str, include_finished: bool) -> Result<Vec<SubagentEntry>> {
        let agent = ClaudeDriver::get(self, id)
            .await?
            .ok_or_else(|| anyhow!("agent {id} 不存在"))?;
        let (Some(cwd), Some(session_id)) = (&agent.cwd, &agent.session_id) else {
            return Ok(Vec::new());
        };
        Ok(subagents::list_subagents(
            &self.projects_dir,
            Path::new(cwd),
            session_id,
            include_finished,
        )
        .await)
    }
}

impl SessionControl for ClaudeDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        // 委托轨一 ClaudeSendRouter（同一实例，见字段注释）+ 注入后置校验（2.4 内化）
        Box::pin(async move {
            crate::drivers::MessageSender::send(&*self.sender, id, text).await?;
            self.spawn_send_post_check(id).await;
            Ok(())
        })
    }

    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        // C2 中断全链路内化（2.3）：validate → PTY 中断 → r69 基线采样 → 打标。
        // 原实现在 api/messages.rs::interrupt_agent，逻辑逐行平移，时序常量冻结。
        Box::pin(async move {
            attach::validate_id(id).map_err(|e| {
                anyhow::Error::new(crate::drivers::NotFound(format!("非法 agent id: {e}")))
            })?;
            crate::drivers::MessageSender::interrupt(&*self.sender, id).await?;
            // r69 竞态修复：基线必须落在中断收尾写入（abort 记录）之后——旧实现固定睡
            // 2s 单次采样，收尾写入晚于采样点时自己就满足「mtime 越过基线且 30s 活跃」
            // 解除判据，标记在下次 list 即被解除，回落 CLI 滞留的 working（r69 实测
            // 0/N 转闲）。改为轮询至 jsonl 静默（观察满 4s 且 2s 无前进）再取基线；
            // 10s 上限兜底中断失败场景——输出持续流动时取到的基线随即被越过，解除判据
            // 放行恢复权威 working，不残留假空闲。机制见 session::sample_quiet_mtime。
            if let Some(session_id) = ClaudeDriver::get(self, id)
                .await
                .ok()
                .flatten()
                .and_then(|a| a.session_id)
            {
                if let Some(t) = session::sample_quiet_mtime(
                    &self.projects_dir,
                    &session_id,
                    2_000,
                    4_000,
                    10_000,
                )
                .await
                {
                    self.interrupted_at
                        .lock()
                        .expect("interrupted_at lock")
                        .insert(id.to_string(), t);
                }
            }
            Ok(())
        })
    }

    fn is_finished(&self, id: &str) -> bool {
        crate::drivers::MessageSender::is_finished(&*self.sender, id)
    }

    fn release(&self, id: &str) -> Result<()> {
        crate::drivers::MessageSender::release(&*self.sender, id)
    }

    fn answer_permission(
        &self,
        _id: &str,
        _perm_id: &str,
        _option_id: Option<&str>,
    ) -> Result<bool> {
        // claude PTY 链路无权限 RPC 语义（权限弹卡仅 ACP 侧，r77）
        Err(unsupported("claude", "answer_permission"))
    }
}

impl AgentCatalog for ClaudeDriver {
    fn list<'a>(&'a self, all: bool) -> BoxFuture<'a, Result<Vec<AgentSummary>>> {
        Box::pin(ClaudeDriver::list(self, all))
    }

    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<AgentSummary>>> {
        Box::pin(ClaudeDriver::get(self, id))
    }

    fn start<'a>(&'a self, spec: &'a StartSpec) -> BoxFuture<'a, Result<String>> {
        let mcp_dir = self.plugin_mcp_dir.clone();
        let hub_bin = self.hub_bin.clone();
        let hub_loopback = self.hub_loopback.clone();
        Box::pin(async move {
            // 转调 api/agents.rs::start_agent 的 CLI 段；validate_cwd 属信任边界
            // 校验，留在 api 层
            // B2：会话级 --mcp-config 注入（有启用插件且非 resume 时生效；不写 ~/.claude）
            // 约束：必须以**真实 bg id**作为 session，需先启动再写 config，因此注入
            // 分两段——无插件时零行为（纯加法），有插件时先起会话再写 config 并
            // 追加 --mcp-config 走二次注入（见下方注释）。
            // 简化 v1：先以真实能力探询决定是否走“注入分支”——有启用插件则在
            // 起会话前先写一个占位 config（文件名=占位 sid），启动参数带上；返回
            // 真实 id 后再以真实 id 重写并回收占位文件（fail-soft，不阻断启动）。
            let needs = if spec.resume_session_id.is_some() {
                false
            } else {
                let dir = mcp_dir.clone();
                tokio::task::spawn_blocking(move || {
                    let cfg = crate::config::Config::load();
                    let _ = &dir;
                    let registry =
                        crate::plugins::registry::Registry::open(cfg.plugin_state_file.clone());
                    registry.list().into_iter().any(|p| {
                        p.entry.enabled
                            && p.entry.state != crate::plugins::registry::PluginState::Fault
                    })
                })
                .await
                .unwrap_or(false)
            };
            let placeholder_cfg: Option<String> = if !needs {
                None
            } else {
                let sid_placeholder = format!("hub-{}", uuid::Uuid::new_v4());
                match crate::plugins::mcp_config::write_config_sync(
                    &mcp_dir,
                    &hub_bin,
                    &hub_loopback,
                    &sid_placeholder,
                ) {
                    Ok(path) => Some(path.to_string_lossy().to_string()),
                    Err(_) => None,
                }
            };
            let req = cli::StartReq {
                prompt: spec.prompt.clone(),
                name: spec.name.clone(),
                model: spec.model.clone(),
                effort: spec.effort.clone(),
                resume: spec.resume_session_id.clone(),
                mcp_config: placeholder_cfg.clone(),
            };
            let id = match cli::start_bg(&self.bin, Path::new(&spec.cwd), &req).await {
                Ok(id) => id,
                Err(e) => {
                    // F-3（tester）：启动失败同样回收占位 config（残留修复——
                    // 原实现 `?` 早退跳过下方清理，plugin_mcp 留 hub-*.json）
                    if let Some(cfg) = placeholder_cfg.as_deref() {
                        let _ = std::fs::remove_file(std::path::PathBuf::from(cfg));
                    }
                    return Err(e);
                }
            };
            if let Some(cfg) = placeholder_cfg.clone() {
                // F-6（tester，preview 反馈轮）：占位 config **原地重写**而非删除另写——
                // `--mcp-config` 参数指向的就是这个文件，claude --bg 由 CLI daemon 托管，
                // 会话进程读 config 的时点不受 hub 控制（多会话/长生命周期下明显滞后），
                // 删除后晚读即落空（工具清单缺失的直接根因）。tmp+rename 原子替换，
                // daemon 读到旧内容（session 占位）或新内容（真实 id）都完整可用。
                // 文件保留至 stop/remove 回收（映射登记）。
                let path = std::path::PathBuf::from(&cfg);
                let _ = crate::plugins::mcp_config::write_config_at_sync(
                    &path,
                    &hub_bin,
                    &hub_loopback,
                    &id,
                );
                self.mcp_config_paths
                    .lock()
                    .unwrap()
                    .insert(id.clone(), path);
            }
            Ok(id)
        })
    }

    fn stop<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        let mcp_dir = self.plugin_mcp_dir.clone();
        let id = id.to_string();
        let bin = self.bin.clone();
        let injected = self.mcp_config_paths.lock().unwrap().remove(&id);
        Box::pin(async move {
            let r = cli::stop(&bin, &id).await;
            // F-6：优先按映射回收实际注入文件（占位名 hub-<uuid>.json）；
            // remove_config_sync 兜底（`<id>.json` 命名约定，映射缺失时唯一线索）
            if let Some(p) = injected {
                let _ = std::fs::remove_file(p);
            }
            crate::plugins::mcp_config::remove_config_sync(&mcp_dir, &id);
            r
        })
    }

    fn remove<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        let mcp_dir = self.plugin_mcp_dir.clone();
        let id = id.to_string();
        let bin = self.bin.clone();
        let injected = self.mcp_config_paths.lock().unwrap().remove(&id);
        Box::pin(async move {
            let r = cli::remove(&bin, &id).await;
            if let Some(p) = injected {
                let _ = std::fs::remove_file(p);
            }
            crate::plugins::mcp_config::remove_config_sync(&mcp_dir, &id);
            r
        })
    }

    fn respawn<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // 转调 api/agents.rs::respawn_agent 现状；会话不存在由驱动域错误表达
            // （NotFound → api 层 404，替代 "No job matching" 字符串匹配，K7）
            attach::validate_id(id)?;
            cli::respawn(&self.bin, id).await.map_err(|e| {
                if format!("{e:#}").contains("No job matching") {
                    crate::drivers::not_found(format!("agent {id} 不存在"))
                } else {
                    e
                }
            })
        })
    }

    fn logs<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(cli::logs(&self.bin, id))
    }
}

impl SessionHistory for ClaudeDriver {
    fn page<'a>(
        &'a self,
        id: &'a str,
        limit: usize,
        before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move {
            // 转调 api/messages.rs::list_messages_page 内核（agent 解析 → 定位 → 分页读）。
            // 中断覆盖只影响 group，历史面不消费（已随 2.3 内化进 list）。
            let agent = ClaudeDriver::get(self, id)
                .await?
                .ok_or_else(|| anyhow!("agent {id} 不存在"))?;
            let Some(session_id) = agent.session_id else {
                return Ok(None);
            };
            let path = match &agent.cwd {
                Some(cwd) => session::session_dir(&self.projects_dir, Path::new(cwd))
                    .map(|dir| dir.join(format!("{session_id}.jsonl"))),
                None => session::find_session_path(&self.projects_dir, &session_id).await,
            };
            let Some(path) = path else {
                return Ok(None);
            };
            Ok(session::read_session_page(&path, limit, before).await)
        })
    }

    /// 旧全量读取（/messages 端点）：cwd 已知走 session_dir 精确定位，否则扫描兜底。
    fn full_history<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<ChatMessage>>> {
        Box::pin(async move {
            let agent = ClaudeDriver::get(self, id)
                .await?
                .ok_or_else(|| anyhow!("agent {id} 不存在"))?;
            let Some(session_id) = agent.session_id else {
                return Ok(Vec::new());
            };
            let msgs = match &agent.cwd {
                Some(cwd) => session::session_dir(&self.projects_dir, Path::new(cwd))
                    .and_then(|dir| Some(dir).map(|d| d.join(format!("{session_id}.jsonl")))),
                None => None,
            };
            let msgs = match msgs {
                Some(path) => session::parse_session_file(&path).await,
                None => session::read_session(&self.projects_dir, &session_id).await,
            };
            Ok(msgs.unwrap_or_default())
        })
    }

    fn is_active<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let agent = ClaudeDriver::get(self, id)
                .await?
                .ok_or_else(|| anyhow!("agent {id} 不存在"))?;
            let Some(session_id) = agent.session_id else {
                return Ok(false);
            };
            // 30s 静默阈值与 api/messages.rs::session_active 同源
            Ok(session::session_is_active(&self.projects_dir, &session_id, 30).await)
        })
    }

    fn subagents<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        Box::pin(self.subagents_impl(id, false))
    }

    fn subagents_finished<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        Box::pin(self.subagents_impl(id, true))
    }

    fn subagent_page<'a>(
        &'a self,
        id: &'a str,
        sub: &'a str,
        limit: usize,
        before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move {
            let agent = ClaudeDriver::get(self, id)
                .await?
                .ok_or_else(|| anyhow!("agent {id} 不存在"))?;
            let Some(session_id) = agent.session_id else {
                return Ok(None);
            };
            let Some(path) =
                session::find_subagent_path(&self.projects_dir, &session_id, sub).await
            else {
                return Ok(None);
            };
            Ok(session::read_session_page(&path, limit, before).await)
        })
    }

    fn tasks<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Vec<TaskEntry>>>> {
        Box::pin(async move {
            let agent = ClaudeDriver::get(self, id)
                .await?
                .ok_or_else(|| anyhow!("agent {id} 不存在"))?;
            let (Some(cwd), Some(session_id)) = (&agent.cwd, &agent.session_id) else {
                // cwd/session 未知 = 无权威视角（对齐 api 层 null 语义，K5）
                return Ok(None);
            };
            Ok(
                tasks::aggregate_project_tasks(&self.projects_dir, Path::new(cwd), session_id)
                    .await,
            )
        })
    }

    fn ensure_chat_room(&self, session_id: &str) -> broadcast::Sender<ChatEvent> {
        // 转调 api/ws.rs::chat 的房间取建段：Entry 区分新建/已有，保证每房间恰一个 tail
        let (sender, need_tail) = {
            let mut rooms = self.rooms.write().expect("chat_rooms lock poisoned");
            match rooms.entry(session_id.to_string()) {
                std::collections::hash_map::Entry::Occupied(e) => (e.get().clone(), false),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let (tx, _) = broadcast::channel(64);
                    e.insert(tx.clone());
                    (tx, true)
                }
            }
        };
        if need_tail {
            session::spawn_session_tail(
                self.projects_dir.clone(),
                session_id.to_string(),
                sender.clone(),
            );
        }
        sender
    }

    fn ensure_subagent_room(
        &self,
        session_id: &str,
        subagent_id: &str,
    ) -> broadcast::Sender<ChatEvent> {
        // 房间键带 subagent 路径段防与主会话冲突（api/ws.rs::chat_subagent 同款）
        let room = format!("{session_id}/subagents/{subagent_id}");
        let (sender, need_tail) = {
            let mut rooms = self.rooms.write().expect("chat_rooms lock poisoned");
            match rooms.entry(room) {
                std::collections::hash_map::Entry::Occupied(e) => (e.get().clone(), false),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let (tx, _) = broadcast::channel(64);
                    e.insert(tx.clone());
                    (tx, true)
                }
            }
        };
        if need_tail {
            session::spawn_subagent_tail(
                self.projects_dir.clone(),
                session_id.to_string(),
                subagent_id.to_string(),
                sender.clone(),
            );
        }
        sender
    }
}

/// claude 思考档位枚举（对齐 CLI `--effort <level>` 合法值：low/medium/high/
/// xhigh/max，2026-10-02 用户确认；与 --effort 启动参数同源）。
const CLAUDE_EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// 会话外观三透传（agent-hub-model-thinking，claude 侧）：
/// - 清单 = 内置常用模型（来源：claude CLI `/model` 支持 opus/sonnet/haiku 三档；
///   CLI 无模型清单端点，静态内置是低成本口径——上游开放查询后替换）
/// - 当前读 = jobs state.json 的 respawnFlags `--model`（尽力而为；无 → None，
///   前端如实显示「由会话进程管理」，不虚构）
/// - 设置 = 注入 `/model <名>`（现有 attach 注入链路；working 时 TUI 排队，
///   收场文案如实提示「已排队」）
pub struct ClaudeModelCaps;

impl crate::drivers::ModelSettings for ClaudeDriver {
    fn model_caps(&self) -> crate::drivers::ModelCaps {
        crate::drivers::ModelCaps {
            can_set: true,
            can_read: true, // 尽力而为：respawnFlags 缺 --model/--effort 时读不到 → None
            supports_thinking: true, // /effort <level> 斜杠命令，2026-10-02 用户确认
        }
    }

    fn list_models<'a>(&'a self) -> BoxFuture<'a, Result<Vec<crate::drivers::ModelInfo>>> {
        Box::pin(async move {
            // 来源注释（需求允许静态内置）：claude TUI /model 实测可选三档
            // effort 是会话级语义（不随模型变），每个模型条目挂同一枚举
            let variants: Vec<String> =
                CLAUDE_EFFORT_LEVELS.iter().map(|s| s.to_string()).collect();
            Ok(["opus", "sonnet", "haiku"]
                .iter()
                .map(|id| crate::drivers::ModelInfo {
                    id: (*id).into(),
                    provider: "anthropic".into(),
                    name: match *id {
                        "opus" => "Opus".to_string(),
                        "sonnet" => "Sonnet".to_string(),
                        _ => "Haiku".to_string(),
                    },
                    free: false,
                    variants: variants.clone(),
                })
                .collect())
        })
    }

    fn session_model<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::drivers::SessionModel>>> {
        Box::pin(async move {
            let job = jobs::read_job_state(&self.jobs_dir, id).await;
            let model = job.as_ref().and_then(|j| j.respawn_model());
            let effort = job.as_ref().and_then(|j| j.respawn_effort());
            Ok(model.map(|m| crate::drivers::SessionModel {
                id: m,
                provider: "anthropic".into(),
                variant: effort,
            }))
        })
    }

    fn set_session_model<'a>(
        &'a self,
        id: &'a str,
        model: &'a str,
        variant: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            // 思考档位走 /effort（2026-10-02 用户确认的斜杠命令），与 /model
            // 完全同一条注入链路；档位校验对齐 CLI 合法值（非法显式报错，
            // 不透传 TUI 静默失败）
            if let Some(v) = variant {
                if !CLAUDE_EFFORT_LEVELS.contains(&v) {
                    anyhow::bail!(
                        "claude 思考档位应为 {} 之一: {v}",
                        CLAUDE_EFFORT_LEVELS.join("/")
                    );
                }
                self.sender
                    .inject_command(id, &format!("/effort {v}"))
                    .await?;
            }
            attach::validate_id(id)?;
            self.sender
                .inject_command(id, &format!("/model {model}"))
                .await?;
            // 收场文案如实：working = TUI 排队（注入已送达，待当前轮消化）
            let busy = jobs::read_job_state(&self.jobs_dir, id)
                .await
                .and_then(|j| j.state)
                .is_some_and(|s| s == "working" || s == "running");
            Ok(if busy {
                "已排队（会话正在处理，当前轮结束后生效）".into()
            } else {
                "已切换".into()
            })
        })
    }
}

impl AgentDriver for ClaudeDriver {
    fn name(&self) -> &'static str {
        "claude"
    }
}

/// 忽略清单读取（R8）：坏 JSON/缺失 → 空集（fail-soft）。
pub(crate) fn read_ignored_interactive(path: &PathBuf) -> std::collections::HashSet<String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<Vec<String>>(&raw)
            .unwrap_or_default()
            .into_iter()
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// 忽略清单写入（R8）：父目录不存在则创建；失败由调用方 log 降级。
pub(crate) fn write_ignored_interactive(
    path: &PathBuf,
    set: &std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    let mut v: Vec<&String> = set.iter().collect();
    v.sort(); // 稳定序列化，diff 友好
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string(&v)?)?;
    Ok(())
}

/// interactive「从列表移除」（R8 端点语义）：按 sessionId 追加忽略记录（去重），
/// 幂等——条目已在清单/已消失均为成功终态。下次 list 起隐藏，进程消失后记录
/// 由懒清理回收。
/// 并发窗口（r-review MINOR 锚定，有意不加锁）：本函数与 list 懒清理回写是
/// read-modify-write，无文件锁下存在 lost-update——最坏后果为刚点下的移除被
/// 覆盖、条目下次 list 复活（再点一次即可），与「宁可复活不丢条目」取舍同构；
/// 单用户本地工具现状可接受，未来多写者化时再加锁。
pub fn unlist_interactive(path: &PathBuf, session_id: &str) -> anyhow::Result<()> {
    let mut set = read_ignored_interactive(path);
    set.insert(session_id.to_string());
    write_ignored_interactive(path, &set)
}

/// hub loopback 缺省值（new() 测试/独立构造路径用）：AGENT_HUB_BIND 解析端口，
/// 未设/解析失败回退 7800。生产路径 AppState 用 cfg.bind_addr.port() 传入。
fn loopback_default() -> String {
    let port = std::env::var("AGENT_HUB_BIND")
        .ok()
        .and_then(|raw| raw.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()))
        .unwrap_or(7800);
    format!("http://127.0.0.1:{port}")
}
