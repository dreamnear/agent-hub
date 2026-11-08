//! watcher 编排与防噪引擎（ADR-0017 §3/§4）：会话状态表、冷却/去重/全局限流、
//! 触发→注入文案编排。
//!
//! 线程模型：调用方持 `std::sync::Mutex<Engine>` 短持锁、不跨 await——巡检快照
//! 与消息事件回调都是同步调用；真正的注入动作由 [`super`] spawn。
//! 全部状态在内存，进程重启清零（用户 2026-11-08 裁决「重启后冷却重置可接受」）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::drivers::pi::ASSISTANT_ID;
use crate::models::{AgentSummary, Group};

use super::rules::{
    self, MatchHit, SessionMeta, TemplateCtx, WatchConfig, WatchRule, MAX_TEXT_CHARS,
};

/// 冷却/去重表规模上限（超出按最旧淘汰，防长跑泄漏）。
const MAX_TRACKED_KEYS: usize = 4096;
/// 会话缺席多少个巡检 tick 后删除基线（≈5min @5s；只删基线，不触发）。
const ABSENT_TICKS_TO_REMOVE: u32 = 60;
/// 提醒前缀（便于助手识别自动化提醒）。
pub const PREFIX: &str = "[hub-watcher] ";

/// 单会话状态（巡检维护；消息事件只更新活跃时刻）。
#[derive(Debug, Clone)]
pub struct SessionState {
    pub driver: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub kind: Option<String>,
    pub group: Group,
    pub context_tokens: Option<u64>,
    /// 最近活跃时刻（`no_output` 判据）；None = 未武装
    pub last_activity: Option<Instant>,
    /// 上一 tick 时刻（`no_output` 的「上一 tick 已静默时长」基准）
    pub last_seen: Instant,
    pub absent_ticks: u32,
}

impl SessionState {
    fn baseline(a: &AgentSummary, now: Instant) -> Self {
        Self {
            driver: a.driver.clone(),
            agent_id: a.id.clone(),
            session_id: a.session_id.clone(),
            name: a.name.clone(),
            cwd: a.cwd.clone(),
            kind: a.kind.clone(),
            group: a.group,
            context_tokens: a.context_tokens,
            // 首次观测只建基线：Working 会话从此刻起算静默时长（不补发历史静默）
            last_activity: (a.group == Group::Working).then_some(now),
            last_seen: now,
            absent_ticks: 0,
        }
    }
}

/// 一次巡检快照的产出：需开房的会话 + 已过防噪闸门的注入文案。
#[derive(Debug, Default)]
pub struct SnapshotOutcome {
    pub rooms: Vec<RoomPlan>,
    pub texts: Vec<String>,
}

/// 需为消息类规则建立会话房间的会话（`ensure_chat_room` 由调用方执行）。
#[derive(Debug, Clone)]
pub struct RoomPlan {
    pub driver: String,
    /// 房间键（`session_id`，缺失时退回 agent id）
    pub room_key: String,
    pub session_key: String,
}

/// 一条待注入的触发意图（规则 + 会话 + 渲染上下文，已脱离借用）。
struct Intent {
    rule_idx: usize,
    session_key: String,
    state_key: String,
    hit: Option<MatchHit>,
    session_label: String,
    id: String,
    driver: String,
    cwd: Option<String>,
    elapsed_secs: Option<u64>,
    threshold: Option<u64>,
    tokens: Option<u64>,
}

impl Intent {
    fn new(
        rule_idx: usize,
        key: &str,
        state_key: impl Into<String>,
        hit: Option<MatchHit>,
        meta: &SessionMeta<'_>,
    ) -> Self {
        Self {
            rule_idx,
            session_key: key.to_string(),
            state_key: state_key.into(),
            hit,
            session_label: meta.name.unwrap_or(meta.id).to_string(),
            id: meta.id.to_string(),
            driver: meta.driver.to_string(),
            cwd: meta.cwd.map(str::to_string),
            elapsed_secs: None,
            threshold: None,
            tokens: None,
        }
    }
}

pub struct Engine {
    cfg: WatchConfig,
    /// (ruleId, sessionKey, stateKey) → 上次触发时刻（冷却 + 同态去重）
    last_fired: HashMap<(String, String, String), Instant>,
    /// 全局 rate cap 滚动窗口（触发时刻队列）
    rate: VecDeque<Instant>,
    /// sessionKey = `<driver>:<agent_id>` → 会话态
    sessions: HashMap<String, SessionState>,
    /// 消息事件 label（agent_id 或 session_id）→ sessionKey
    index: HashMap<String, String>,
    /// 已计划开房间的 sessionKey（幂等标记，不必真开成功才记）
    rooms_opened: HashSet<String>,
    /// 超出开房上限只 warn 一次
    limit_warned: bool,
    fired: u64,
    suppressed: u64,
    dropped: u64,
}

impl Engine {
    pub fn new(cfg: WatchConfig) -> Self {
        Self {
            cfg,
            last_fired: HashMap::new(),
            rate: VecDeque::new(),
            sessions: HashMap::new(),
            index: HashMap::new(),
            rooms_opened: HashSet::new(),
            limit_warned: false,
            fired: 0,
            suppressed: 0,
            dropped: 0,
        }
    }

    pub fn state_poll_secs(&self) -> u64 {
        self.cfg.state_poll_secs
    }

    /// (已注入, 冷却抑制, 限流丢弃) 累计计数（可观测性）。
    pub fn counters(&self) -> (u64, u64, u64) {
        (self.fired, self.suppressed, self.dropped)
    }

    /// 巡检快照：开房计划 → 会话态 diff → 四类条件穿越 → 防噪 → 文案。
    ///
    /// 空观测（并发 list 降级 / 无会话）一律「无事发生」：不触发、不清态。
    pub fn on_snapshot(&mut self, agents: &[AgentSummary], now: Instant) -> SnapshotOutcome {
        let mut out = SnapshotOutcome::default();
        if agents.is_empty() {
            return out;
        }

        out.rooms = self.plan_rooms(agents);

        let mut intents: Vec<Intent> = Vec::new();
        let mut seen: HashSet<String> = HashSet::with_capacity(agents.len());

        for a in agents {
            // 自环硬守卫（ADR-0017 §3）：助手自身恒不参与任何规则评估
            if is_self(a) {
                continue;
            }
            let key = session_key(&a.driver, &a.id);
            seen.insert(key.clone());
            let interactive = a.kind.as_deref() == Some("interactive");

            if !self.sessions.contains_key(&key) {
                // 首次观测只建基线，不触发（重启不补发）
                self.index.insert(a.id.clone(), key.clone());
                if let Some(sid) = &a.session_id {
                    self.index.insert(sid.clone(), key.clone());
                }
                self.sessions.insert(key, SessionState::baseline(a, now));
                continue;
            }

            let prev = self.sessions.get_mut(&key).expect("session exists");
            prev.absent_ticks = 0;
            let prev_tick = prev.last_seen;
            let prev_group = prev.group;
            let prev_ctx = prev.context_tokens;
            let prev_activity = prev.last_activity;
            prev.last_seen = now;

            // no_output 武装/复位：进入 Working 以 now 武装；离开 Working 清空
            if a.group == Group::Working {
                if prev_group != Group::Working || prev_activity.is_none() {
                    prev.last_activity = Some(now);
                }
            } else {
                prev.last_activity = None;
            }

            prev.group = a.group;
            prev.context_tokens = a.context_tokens;
            prev.session_id = a.session_id.clone();
            prev.name = a.name.clone();
            prev.cwd = a.cwd.clone();
            prev.kind = a.kind.clone();

            self.index.insert(a.id.clone(), key.clone());
            if let Some(sid) = &a.session_id {
                self.index.insert(sid.clone(), key.clone());
            }

            // interactive 前台会话只读观察、不注入（R7 同向）
            if interactive {
                continue;
            }

            let cur_activity = prev.last_activity;
            let meta = SessionMeta {
                driver: &prev.driver,
                id: &prev.agent_id,
                session_id: prev.session_id.as_deref(),
                name: prev.name.as_deref(),
                cwd: prev.cwd.as_deref(),
            };

            // ① needs_input：进入 NeedsInput 的穿越点
            if prev_group != Group::NeedsInput && a.group == Group::NeedsInput {
                for (idx, rule) in self.cfg.rules.iter().enumerate() {
                    let WatchRule::NeedsInput(common) = rule else {
                        continue;
                    };
                    if !common.enabled || !rules::scope_match(&common.scope, &meta) {
                        continue;
                    }
                    intents.push(Intent::new(idx, &key, "needs_input", None, &meta));
                }
            }

            // ② context_tokens：上穿阈值的穿越点（None 不触发）
            if let (Some(prev_tokens), Some(cur_tokens)) = (prev_ctx, a.context_tokens) {
                for (idx, rule) in self.cfg.rules.iter().enumerate() {
                    let WatchRule::ContextTokens { common, threshold } = rule else {
                        continue;
                    };
                    if !common.enabled || !rules::scope_match(&common.scope, &meta) {
                        continue;
                    }
                    if prev_tokens < *threshold && cur_tokens >= *threshold {
                        let mut intent = Intent::new(idx, &key, "context_tokens", None, &meta);
                        intent.threshold = Some(*threshold);
                        intent.tokens = Some(cur_tokens);
                        intents.push(intent);
                    }
                }
            }

            // ③ no_output：Working 且已建消息通道时，静默时长跨阈值的穿越点
            if a.group == Group::Working && self.rooms_opened.contains(&key) {
                let prev_elapsed = if prev_group == Group::Working {
                    prev_activity.map(|t| prev_tick.saturating_duration_since(t))
                } else {
                    None
                };
                let cur_elapsed = cur_activity.map(|t| now.saturating_duration_since(t));
                if let (Some(pe), Some(ce)) = (prev_elapsed, cur_elapsed) {
                    for (idx, rule) in self.cfg.rules.iter().enumerate() {
                        let WatchRule::NoOutput {
                            common,
                            threshold_secs,
                        } = rule
                        else {
                            continue;
                        };
                        if !common.enabled || !rules::scope_match(&common.scope, &meta) {
                            continue;
                        }
                        let threshold = Duration::from_secs(*threshold_secs);
                        if pe < threshold && ce >= threshold {
                            let mut intent = Intent::new(idx, &key, "no_output", None, &meta);
                            intent.elapsed_secs = Some(ce.as_secs());
                            intent.threshold = Some(*threshold_secs);
                            intents.push(intent);
                        }
                    }
                }
            }
        }

        for intent in intents {
            if let Some(text) = self.fire(intent, now) {
                out.texts.push(text);
            }
        }

        // 缺席只累计计数、不触发也不清态；连续缺席足够久才删基线
        let mut dead: Vec<String> = Vec::new();
        for (key, st) in self.sessions.iter_mut() {
            if seen.contains(key) {
                continue;
            }
            st.absent_ticks = st.absent_ticks.saturating_add(1);
            if st.absent_ticks >= ABSENT_TICKS_TO_REMOVE {
                dead.push(key.clone());
            }
        }
        for key in dead {
            self.forget_session(&key);
        }

        out
    }

    /// 消息事件（内部观察者回调路径）：解析会话 → 更新活跃 → `message` 规则。
    pub fn on_message(&mut self, label: &str, kind: &str, text: &str, now: Instant) -> Vec<String> {
        let Some(key) = self.index.get(label).cloned() else {
            // 解析不到 = 该事件只可能来自未巡检到的会话：不建态、不触发
            return Vec::new();
        };
        let Some(st) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        // 任意 kind 都算活跃（no_output 复位）
        st.last_activity = Some(now);
        st.last_seen = now;
        if st.driver == "pi" && st.agent_id == ASSISTANT_ID {
            return Vec::new();
        }
        if st.kind.as_deref() == Some("interactive") {
            return Vec::new();
        }
        let (driver, id, session_id, name, cwd) = (
            st.driver.clone(),
            st.agent_id.clone(),
            st.session_id.clone(),
            st.name.clone(),
            st.cwd.clone(),
        );

        let meta = SessionMeta {
            driver: &driver,
            id: &id,
            session_id: session_id.as_deref(),
            name: name.as_deref(),
            cwd: cwd.as_deref(),
        };
        let mut intents = Vec::new();
        for (idx, rule) in self.cfg.rules.iter().enumerate() {
            let WatchRule::Message {
                common,
                kinds,
                keywords,
                regex,
            } = rule
            else {
                continue;
            };
            if !common.enabled || !rules::scope_match(&common.scope, &meta) {
                continue;
            }
            let Some(hit) = rules::match_message(kinds, keywords, regex, kind, text) else {
                continue;
            };
            let state_key = hit.state_key();
            intents.push(Intent::new(idx, &key, state_key, Some(hit), &meta));
        }
        intents
            .into_iter()
            .filter_map(|intent| self.fire(intent, now))
            .collect()
    }

    /// 会话关闭：清理会话态、索引、开房标记与该会话的全部冷却条目。
    pub fn on_session_close(&mut self, label: &str) {
        let Some(key) = self.index.get(label).cloned() else {
            return;
        };
        self.forget_session(&key);
    }

    /// 触发闸门：冷却/同态去重 → 全局限流 → 记账 → 渲染文案。
    fn fire(&mut self, intent: Intent, now: Instant) -> Option<String> {
        let rule = self.cfg.rules.get(intent.rule_idx)?;
        let common = rule.common();
        let dedup_key = (
            common.id.clone(),
            intent.session_key.clone(),
            intent.state_key.clone(),
        );
        if let Some(prev) = self.last_fired.get(&dedup_key) {
            if now.saturating_duration_since(*prev) < Duration::from_secs(common.cooldown_secs) {
                self.suppressed += 1;
                tracing::debug!(
                    rule = %common.id,
                    session = %intent.session_key,
                    state = %intent.state_key,
                    "hub-watcher: 冷却期内抑制"
                );
                return None;
            }
        }

        let window = Duration::from_secs(self.cfg.rate_limit.window_secs);
        while let Some(front) = self.rate.front() {
            if now.saturating_duration_since(*front) >= window {
                self.rate.pop_front();
            } else {
                break;
            }
        }
        if self.rate.len() >= self.cfg.rate_limit.max {
            self.dropped += 1;
            tracing::warn!(
                rule = %common.id,
                limit = self.cfg.rate_limit.max,
                window_secs = self.cfg.rate_limit.window_secs,
                "hub-watcher: 全局节流丢弃"
            );
            return None;
        }

        self.last_fired.insert(dedup_key, now);
        self.rate.push_back(now);
        self.fired += 1;
        if self.last_fired.len() > MAX_TRACKED_KEYS {
            if let Some(oldest) = self
                .last_fired
                .iter()
                .min_by_key(|(_, fired_at)| **fired_at)
                .map(|(k, _)| k.clone())
            {
                self.last_fired.remove(&oldest);
            }
        }

        let ctx = TemplateCtx {
            session: &intent.session_label,
            id: &intent.id,
            driver: &intent.driver,
            cwd: intent.cwd.as_deref(),
            elapsed_secs: intent.elapsed_secs,
            threshold: intent.threshold,
            tokens: intent.tokens,
        };
        let body = rules::render(
            common.template.as_deref(),
            rule.default_template(),
            &common.id,
            intent.hit.as_ref(),
            &ctx,
        );
        tracing::info!(
            rule = %common.id,
            session = %intent.session_key,
            state = %intent.state_key,
            length = body.chars().count(),
            "hub-watcher: 规则命中，注入提醒"
        );
        Some(
            format!("{PREFIX}{body}")
                .chars()
                .take(MAX_TEXT_CHARS)
                .collect(),
        )
    }

    /// 开房计划：消息类规则作用域内的会话，按上限与确定性顺序挑出。
    fn plan_rooms(&mut self, agents: &[AgentSummary]) -> Vec<RoomPlan> {
        let cap = self.cfg.max_watched_sessions;
        if cap == 0 {
            return Vec::new();
        }
        let stream_rules: Vec<&WatchRule> = self
            .cfg
            .rules
            .iter()
            .filter(|r| r.common().enabled && r.needs_message_stream())
            .collect();
        if stream_rules.is_empty() {
            return Vec::new();
        }

        let mut candidates: Vec<&AgentSummary> = agents
            .iter()
            .filter(|a| {
                if is_self(a)
                    || a.kind.as_deref() == Some("interactive")
                    || a.group == Group::Completed
                {
                    return false;
                }
                if self.rooms_opened.contains(&session_key(&a.driver, &a.id)) {
                    return false;
                }
                let meta = meta_of(a);
                stream_rules
                    .iter()
                    .any(|r| rules::scope_match(&r.common().scope, &meta))
            })
            .collect();
        // 确定性：started_at 降序（None 靠后），再按 id 字典序
        candidates.sort_by(|x, y| match (x.started_at, y.started_at) {
            (Some(a), Some(b)) => b.cmp(&a).then_with(|| x.id.cmp(&y.id)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => x.id.cmp(&y.id),
        });

        let remaining = cap.saturating_sub(self.rooms_opened.len());
        if candidates.len() > remaining && !self.limit_warned {
            tracing::warn!(
                cap,
                "hub-watcher: 超出 maxWatchedSessions，超限会话的消息类规则不生效"
            );
            self.limit_warned = true;
        }
        let plans: Vec<RoomPlan> = candidates
            .into_iter()
            .take(remaining)
            .map(|a| RoomPlan {
                driver: a.driver.clone(),
                room_key: a.session_id.clone().unwrap_or_else(|| a.id.clone()),
                session_key: session_key(&a.driver, &a.id),
            })
            .collect();
        for plan in &plans {
            self.rooms_opened.insert(plan.session_key.clone());
        }
        plans
    }

    fn forget_session(&mut self, key: &str) {
        self.sessions.remove(key);
        self.rooms_opened.remove(key);
        self.last_fired.retain(|(_, sk, _), _| sk != key);
        self.index.retain(|_, v| v.as_str() != key);
    }
}

fn session_key(driver: &str, id: &str) -> String {
    format!("{driver}:{id}")
}

fn is_self(a: &AgentSummary) -> bool {
    a.driver == "pi" && a.id == ASSISTANT_ID
}

fn meta_of(a: &AgentSummary) -> SessionMeta<'_> {
    SessionMeta {
        driver: &a.driver,
        id: &a.id,
        session_id: a.session_id.as_deref(),
        name: a.name.as_deref(),
        cwd: a.cwd.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(raw: &str) -> Engine {
        Engine::new(WatchConfig::parse(raw).expect("配置应可解析"))
    }

    fn agent(driver: &str, id: &str, group: Group, context_tokens: Option<u64>) -> AgentSummary {
        AgentSummary {
            driver: driver.into(),
            id: id.into(),
            name: Some(id.into()),
            cwd: Some("/w".into()),
            kind: Some("background".into()),
            raw_state: None,
            group,
            detail: None,
            tokens: None,
            context_tokens,
            started_at: Some(1),
            session_id: Some(format!("sess-{id}")),
        }
    }

    fn interactive(driver: &str, id: &str, group: Group) -> AgentSummary {
        let mut a = agent(driver, id, group, None);
        a.kind = Some("interactive".into());
        a
    }

    // —— 第 5 组：穿越 / 冷却 / 限流 / stateKey ——

    #[test]
    fn needs_input_fires_once_on_crossing() {
        let mut e = engine(r#"{"version":1,"rules":[{"id":"ni","on":"needs_input"}]}"#);
        let t0 = Instant::now();
        let out = e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert!(out.texts.is_empty(), "首次观测只建基线");
        let out = e.on_snapshot(
            &[agent("claude", "a", Group::NeedsInput, None)],
            t0 + Duration::from_secs(5),
        );
        assert_eq!(out.texts.len(), 1);
        assert!(out.texts[0].starts_with(PREFIX), "{}", out.texts[0]);
        assert!(out.texts[0].contains("等待你的输入"));
        let out = e.on_snapshot(
            &[agent("claude", "a", Group::NeedsInput, None)],
            t0 + Duration::from_secs(10),
        );
        assert!(out.texts.is_empty(), "驻留不复发");
    }

    #[test]
    fn empty_snapshot_changes_nothing() {
        let mut e = engine(r#"{"version":1,"rules":[{"id":"ni","on":"needs_input"}]}"#);
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        e.on_snapshot(&[], t0 + Duration::from_secs(5));
        let out = e.on_snapshot(
            &[agent("claude", "a", Group::NeedsInput, None)],
            t0 + Duration::from_secs(10),
        );
        assert_eq!(out.texts.len(), 1, "空观测（并发降级）不得清态");
    }

    #[test]
    fn message_fires_then_cooldown_suppresses_and_expires() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"m","on":"message","keywords":["ERROR"],"cooldownSecs":60}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        let hit = e.on_message(
            "sess-a",
            "assistant",
            "boom ERROR",
            t0 + Duration::from_secs(1),
        );
        assert_eq!(hit.len(), 1);
        let sup = e.on_message(
            "sess-a",
            "assistant",
            "ERROR again",
            t0 + Duration::from_secs(2),
        );
        assert!(sup.is_empty(), "冷却期内不重复");
        assert_eq!(e.counters(), (1, 1, 0));
        let later = e.on_message(
            "sess-a",
            "assistant",
            "ERROR later",
            t0 + Duration::from_secs(62),
        );
        assert_eq!(later.len(), 1, "冷却到期放行");
    }

    #[test]
    fn message_state_key_distinguishes_keywords() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"m","on":"message","keywords":["alpha","beta"],"cooldownSecs":300}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert_eq!(
            e.on_message("sess-a", "assistant", "alpha", t0 + Duration::from_secs(1))
                .len(),
            1
        );
        // 同一规则不同命中词 = 不同 stateKey，不被上一条的冷却连坐
        assert_eq!(
            e.on_message("sess-a", "assistant", "beta", t0 + Duration::from_secs(2))
                .len(),
            1
        );
        assert!(e
            .on_message(
                "sess-a",
                "assistant",
                "alpha again",
                t0 + Duration::from_secs(3)
            )
            .is_empty());
    }

    #[test]
    fn global_rate_cap_drops_beyond_max() {
        let mut e = engine(
            r#"{"version":1,
                "rateLimit":{"max":2,"windowSecs":300},
                "rules":[{"id":"m","on":"message","keywords":["x"],"cooldownSecs":0}]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert_eq!(
            e.on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(1))
                .len(),
            1
        );
        assert_eq!(
            e.on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(2))
                .len(),
            1
        );
        assert!(e
            .on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(3))
            .is_empty());
        let (fired, _, dropped) = e.counters();
        assert_eq!((fired, dropped), (2, 1), "第 3 条被全局限流丢弃");
        // 窗口滑动后可再触发
        assert_eq!(
            e.on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(400))
                .len(),
            1
        );
    }

    #[test]
    fn tracked_table_evicts_oldest_beyond_cap() {
        let mut e = engine(
            r#"{"version":1,"rules":[{"id":"m","on":"message","keywords":["x"],"cooldownSecs":3600}]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        for i in 0..MAX_TRACKED_KEYS {
            e.last_fired.insert(
                ("m".into(), format!("sess-{i}"), "kw:x".into()),
                t0 + Duration::from_millis(i as u64),
            );
        }
        let _ = e.on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(10));
        assert_eq!(e.last_fired.len(), MAX_TRACKED_KEYS, "超限只淘汰到上限");
        assert!(
            !e.last_fired
                .contains_key(&("m".into(), "sess-0".into(), "kw:x".into())),
            "淘汰最旧条目"
        );
    }

    // —— 第 6 组：no_output / context ——

    #[test]
    fn no_output_fires_on_crossing_and_resets_on_message() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"stuck","on":"no_output","thresholdSecs":60,"cooldownSecs":0}
            ]}"#,
        );
        let t0 = Instant::now();
        let secs = |n: u64| t0 + Duration::from_secs(n);
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert!(e
            .on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(30))
            .texts
            .is_empty());
        let out = e.on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(61));
        assert_eq!(out.texts.len(), 1, "静默跨过 60s 阈值");
        assert!(out.texts[0].contains("无新输出"));
        assert!(
            e.on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(70))
                .texts
                .is_empty(),
            "已过阈不再重复穿越"
        );

        // 消息复位后再次静默，可再次穿越
        e.on_message("sess-a", "assistant", "still alive", secs(80));
        assert!(e
            .on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(90))
            .texts
            .is_empty());
        let out = e.on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(145));
        assert_eq!(out.texts.len(), 1);
    }

    #[test]
    fn no_output_not_evaluated_without_room() {
        let mut e = engine(
            r#"{"version":1,"maxWatchedSessions":0,"rules":[
                {"id":"stuck","on":"no_output","thresholdSecs":10,"cooldownSecs":0}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        let out = e.on_snapshot(
            &[agent("claude", "a", Group::Working, None)],
            t0 + Duration::from_secs(100),
        );
        assert!(out.texts.is_empty(), "未建消息通道的会话不评估 no_output");
        assert!(out.rooms.is_empty());
    }

    #[test]
    fn no_output_resets_when_leaving_working() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"stuck","on":"no_output","thresholdSecs":60,"cooldownSecs":0}
            ]}"#,
        );
        let t0 = Instant::now();
        let secs = |n: u64| t0 + Duration::from_secs(n);
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert!(e
            .on_snapshot(&[agent("claude", "a", Group::NeedsInput, None)], secs(50))
            .texts
            .is_empty());
        // 离开 Working 清空武装；再次进入 Working 从此刻重新起算
        assert!(e
            .on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(100))
            .texts
            .is_empty());
        let out = e.on_snapshot(&[agent("claude", "a", Group::Working, None)], secs(161));
        assert_eq!(out.texts.len(), 1);
    }

    #[test]
    fn context_tokens_crossing_only() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"ctx","on":"context_tokens","threshold":100,"cooldownSecs":0}
            ]}"#,
        );
        let t0 = Instant::now();
        let secs = |n: u64| t0 + Duration::from_secs(n);
        e.on_snapshot(&[agent("claude", "a", Group::Working, Some(50))], t0);
        let out = e.on_snapshot(&[agent("claude", "a", Group::Working, Some(120))], secs(5));
        assert_eq!(out.texts.len(), 1);
        assert!(out.texts[0].contains("120"), "{}", out.texts[0]);
        assert!(
            e.on_snapshot(&[agent("claude", "a", Group::Working, Some(130))], secs(10))
                .texts
                .is_empty(),
            "高位不复发"
        );
        assert!(e
            .on_snapshot(&[agent("claude", "a", Group::Working, Some(40))], secs(15))
            .texts
            .is_empty());
        let out = e.on_snapshot(&[agent("claude", "a", Group::Working, Some(120))], secs(20));
        assert_eq!(out.texts.len(), 1, "回落再上穿可再触发");
    }

    #[test]
    fn context_tokens_none_never_fires() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"ctx","on":"context_tokens","threshold":100,"cooldownSecs":0}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert!(e
            .on_snapshot(
                &[agent("claude", "a", Group::Working, None)],
                t0 + Duration::from_secs(5)
            )
            .texts
            .is_empty());
        // 其余驱动 context_tokens 恒 None → 规则永不触发
        e.on_snapshot(&[agent("dsh", "b", Group::Working, None)], t0);
        assert!(e
            .on_snapshot(
                &[agent("dsh", "b", Group::Working, None)],
                t0 + Duration::from_secs(10)
            )
            .texts
            .is_empty());
    }

    // —— 第 7 组：观察者接线 / 自环 / 作用域 ——

    #[test]
    fn self_and_interactive_are_never_triggered() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"ni","on":"needs_input"},
                {"id":"m","on":"message","keywords":["x"],"cooldownSecs":0}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(
            &[
                agent("claude", "a", Group::Working, None),
                agent("pi", "hub-assistant", Group::Working, None),
                interactive("claude", "fg", Group::Working),
            ],
            t0,
        );
        let out = e.on_snapshot(
            &[
                agent("claude", "a", Group::NeedsInput, None),
                agent("pi", "hub-assistant", Group::NeedsInput, None),
                interactive("claude", "fg", Group::NeedsInput),
            ],
            t0 + Duration::from_secs(5),
        );
        assert_eq!(out.texts.len(), 1, "仅普通后台会话触发");
        assert!(
            e.on_message(
                "hub-assistant",
                "assistant",
                "x",
                t0 + Duration::from_secs(6)
            )
            .is_empty(),
            "助手自身自环排除"
        );
        assert!(
            e.on_message("sess-fg", "assistant", "x", t0 + Duration::from_secs(6))
                .is_empty(),
            "interactive 只读观察"
        );
        assert_eq!(
            e.on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(6))
                .len(),
            1
        );
    }

    #[test]
    fn unknown_message_label_is_ignored() {
        let mut e = engine(
            r#"{"version":1,"rules":[{"id":"m","on":"message","keywords":["x"],"cooldownSecs":0}]}"#,
        );
        let t0 = Instant::now();
        assert!(e.on_message("ghost", "assistant", "x", t0).is_empty());
        assert!(e.sessions.is_empty(), "解析不到 label 不建态");
    }

    #[test]
    fn scope_limits_message_rules() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"m","on":"message","keywords":["x"],"cooldownSecs":0,
                 "scope":{"sessions":[],"cwdPrefix":["/other"]}}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert!(e
            .on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(1))
            .is_empty());
    }

    #[test]
    fn session_close_forgets_state_and_cooldown() {
        let mut e = engine(
            r#"{"version":1,"rules":[
                {"id":"ni","on":"needs_input"},
                {"id":"m","on":"message","keywords":["x"],"cooldownSecs":3600}
            ]}"#,
        );
        let t0 = Instant::now();
        e.on_snapshot(&[agent("claude", "a", Group::Working, None)], t0);
        assert_eq!(
            e.on_message("sess-a", "assistant", "x", t0 + Duration::from_secs(1))
                .len(),
            1
        );
        e.on_session_close("sess-a");
        assert!(e.sessions.is_empty());
        assert!(e.index.is_empty());
        assert!(e.last_fired.is_empty(), "该会话的冷却条目一并清理");
        // 重开后按首次观测处理：不补发
        assert!(e
            .on_snapshot(
                &[agent("claude", "a", Group::NeedsInput, None)],
                t0 + Duration::from_secs(5)
            )
            .texts
            .is_empty());
    }

    #[test]
    fn room_planning_respects_cap_and_dedups() {
        let mut e = engine(
            r#"{"version":1,"maxWatchedSessions":1,"rules":[
                {"id":"stuck","on":"no_output","thresholdSecs":60}
            ]}"#,
        );
        let t0 = Instant::now();
        let mut older = agent("claude", "old", Group::Working, None);
        older.started_at = Some(10);
        let mut newer = agent("claude", "new", Group::Working, None);
        newer.started_at = Some(20);
        let out = e.on_snapshot(&[older.clone(), newer.clone()], t0);
        assert_eq!(out.rooms.len(), 1, "上限 1");
        assert_eq!(
            out.rooms[0].session_key, "claude:new",
            "started_at 降序优先"
        );
        assert_eq!(out.rooms[0].room_key, "sess-new");
        let out = e.on_snapshot(&[older, newer], t0 + Duration::from_secs(5));
        assert!(out.rooms.is_empty(), "同一会话只开一次");
    }
}
