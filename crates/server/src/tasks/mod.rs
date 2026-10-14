//! 定时任务（agent-hub-daemon-capabilities）：croner 调度 + 声明式任务存储 + Run 日志。
//!
//! - 任务表：`<tasks_dir>/tasks.json`（原子写 tmp+rename；启动加载；API 变更即落盘）
//! - Run 日志：`<tasks_dir>/runs/<taskId>.jsonl` 追加写，滚动保留 100 条
//! - 错过触发（hub 停机期间）= 跳过并在 runs 记一条 missed，不补跑
//! - 同任务串行：上一次 fire 未完成则本次记 skipped（调度与手动 run 共用守卫）
//! - 时区：{{date}}/{{time}} 与 cron 匹配均用本地时区；chrono 为 croner 传递依赖，
//!   显式声明 = 零新增依赖面（对齐「不引时间库」纪律的例外成本已评估）

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Local};
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::str::FromStr as _;

use crate::api::SharedState;
use crate::drivers::StartSpec;
use crate::platform;

// —— 数据模型 ——

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    #[serde(default)] // create 请求不带 id（服务端生成）；存储/API 响应恒有
    pub id: String,
    pub name: String,
    pub cron: String,
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// 来源标记（assistant-cron C4）：None = hub UI 手动建档；Some("assistant") =
    /// 助手经「allow assistant task control」权限门创建（hub_cron_create）。
    /// 仅展示/审计语义，调度行为与手动任务无差别；旧 tasks.json 无此字段兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(flatten)]
    pub action: TaskAction,
}

fn bool_true() -> bool {
    true
}

/// 任务动作两类（requirement 决策 6）：spawn 到点新建会话；inject 向既有会话注入消息。
/// 复杂场景（自动提 issue 等）由 prompt 指示 agent 自组合，hub 不逐场景硬编码（决策 8）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum TaskAction {
    Spawn {
        driver: String,
        cwd: String,
        #[serde(default)]
        name_template: Option<String>,
        prompt_template: String,
    },
    Inject {
        driver: String,
        session_id: String,
        text: String,
    },
}

/// 每次触发一条 Run 记录（requirement 决策 7）：会话全文即事实源，Run 只留索引与状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    /// 本地时区 RFC3339
    pub fired_at: String,
    /// running | ok | failed | skipped | missed
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const RUN_CAP: usize = 100;

// —— 纯函数（单测面） ——

/// cron 表达式合法性（API 校验与调度器共用同一判定）。
pub fn cron_valid(pattern: &str) -> bool {
    Cron::from_str(pattern).is_ok()
}

/// 模板变量替换：{{date}}/{{time}} 本地时区（YYYY-MM-DD / HH:mm）。
pub fn render_template(tpl: &str, now: DateTime<Local>) -> String {
    tpl.replace("{{date}}", &now.format("%Y-%m-%d").to_string())
        .replace("{{time}}", &now.format("%H:%M").to_string())
}

/// 下次触发时间（本地时区 RFC3339）；cron 不合法 → None。
pub fn next_run_at(pattern: &str, after: DateTime<Local>) -> Option<String> {
    let cron = Cron::from_str(pattern).ok()?;
    cron.find_next_occurrence(&after, false).ok().map(|t| {
        t.with_timezone(&Local)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
}

/// 错过检测（hub 启动时）：上次触发之后本应有一次触发但已过去 → 视为停机期间错过。
/// 只记一条 missed（不枚举停机时长内的每个槽位，防长期停机刷爆日志）。
pub fn missed_since(pattern: &str, last_fired_rfc3339: &str, now: DateTime<Local>) -> bool {
    let Ok(cron) = Cron::from_str(pattern) else {
        return false;
    };
    let Ok(last) = DateTime::parse_from_rfc3339(last_fired_rfc3339) else {
        return false;
    };
    let last = last.with_timezone(&Local);
    match cron.find_next_occurrence(&last, false) {
        Ok(next) => next.with_timezone(&Local) < now,
        Err(_) => false,
    }
}

// —— 存储与运行日志 ——

fn atomic_write(path: &Path, body: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

fn load_tasks_file(path: &Path) -> Vec<TaskSpec> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new(); // 首启无文件 = 空表
    };
    match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "tasks.json 解析失败，按空表处理");
            Vec::new()
        }
    }
}

fn runs_path(dir: &Path, task_id: &str) -> PathBuf {
    dir.join("runs").join(format!("{task_id}.jsonl"))
}

fn append_run_file(dir: &Path, task_id: &str, run: &TaskRun) {
    let path = runs_path(dir, task_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|raw| raw.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    lines.push(serde_json::to_string(run).unwrap_or_default());
    // 滚动上限：只留最近 RUN_CAP 条
    let tail = if lines.len() > RUN_CAP {
        &lines[lines.len() - RUN_CAP..]
    } else {
        &lines[..]
    };
    if let Ok(mut f) = std::fs::File::create(&path) {
        for l in tail {
            let _ = writeln!(f, "{l}");
        }
    }
}

fn read_run_file(dir: &Path, task_id: &str) -> Vec<TaskRun> {
    let Ok(raw) = std::fs::read_to_string(runs_path(dir, task_id)) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// 任务表 + Run 日志 + 同任务串行守卫的进程内管理器（挂 AppState）。
pub struct TaskManager {
    dir: PathBuf,
    inner: Mutex<Vec<TaskSpec>>,
    /// 同任务串行守卫：在途任务 id 集合（调度器与手动 run 共用）
    running: Mutex<HashSet<String>>,
    /// 本实例是否持有调度权（S2 可观测：scheduler-status 端点与 UI 徽标数据源；
    /// spawn_scheduler 拿/接排他文件锁时置位）
    scheduler_primary: AtomicBool,
}

impl TaskManager {
    pub fn new(dir: PathBuf) -> Self {
        let tasks = load_tasks_file(&dir.join("tasks.json"));
        tracing::info!(dir = %dir.display(), count = tasks.len(), "定时任务表已加载");
        Self {
            dir,
            inner: Mutex::new(tasks),
            running: Mutex::new(HashSet::new()),
            scheduler_primary: AtomicBool::new(false),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 调度权状态读写（S2）：spawn_scheduler 拿/接排他文件锁时置位，端点读。
    pub fn set_scheduler_primary(&self, primary: bool) {
        self.scheduler_primary.store(primary, Ordering::Relaxed);
    }

    pub fn scheduler_primary(&self) -> bool {
        self.scheduler_primary.load(Ordering::Relaxed)
    }

    /// 从共享 tasks.json 重载任务表（schedule-lock S4 边界修复）：多实例共享
    /// TASKS_DIR 时，他实例的动态 CRUD 只落盘、不改本实例内存——standby 接管
    /// 后内存表可能是启动时的旧快照（tester 实测：接管后漏调度 4 周期）。调度
    /// 循环每 tick 前 reload，以共享文件为事实源（原子写保证一致性；本实例自身
    /// 写路径落盘内容与内存一致，reload 无损）。
    pub fn reload_from_disk(&self) {
        if let Ok(mut g) = self.inner.lock() {
            *g = load_tasks_file(&self.dir.join("tasks.json"));
        }
    }

    pub fn list(&self) -> Vec<TaskSpec> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<TaskSpec> {
        self.inner.lock().ok()?.iter().find(|t| t.id == id).cloned()
    }

    /// 新增/替换并落盘（API 写路径唯一入口）。
    /// PoisonError 不入 io::Error（其持 MutexGuard 非 Send，会毒化 async 调用链的
    /// Send 界）——毒化即短路失败，写路径与 in-memory 不一致可由重启自愈。
    pub fn upsert(&self, spec: TaskSpec) -> std::io::Result<()> {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return Err(std::io::Error::other("tasks lock poisoned")),
        };
        if let Some(slot) = g.iter_mut().find(|t| t.id == spec.id) {
            *slot = spec;
        } else {
            g.push(spec);
        }
        atomic_write(
            &self.dir.join("tasks.json"),
            &serde_json::to_string_pretty(&*g)?,
        )
    }

    pub fn remove(&self, id: &str) -> std::io::Result<bool> {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return Err(std::io::Error::other("tasks lock poisoned")),
        };
        let before = g.len();
        g.retain(|t| t.id != id);
        let removed = g.len() != before;
        if removed {
            atomic_write(
                &self.dir.join("tasks.json"),
                &serde_json::to_string_pretty(&*g)?,
            )?;
        }
        Ok(removed)
    }

    /// 串行守卫：成功占位返回 true；该任务已有在途触发返回 false（本次记 skipped）。
    pub fn try_begin(&self, id: &str) -> bool {
        self.running
            .lock()
            .map(|mut g| g.insert(id.to_owned()))
            .unwrap_or(false)
    }

    pub fn end(&self, id: &str) {
        if let Ok(mut g) = self.running.lock() {
            g.remove(id);
        }
    }

    pub fn append_run(&self, task_id: &str, run: &TaskRun) {
        append_run_file(&self.dir, task_id, run);
    }

    /// 最近优先（UI 直接倒序展示）。
    pub fn runs(&self, task_id: &str) -> Vec<TaskRun> {
        let mut v = read_run_file(&self.dir, task_id);
        v.reverse();
        v
    }

    /// 启动时错过检测：enabled 任务若停机期间有应触发而未触发的槽位，记一条 missed。
    pub fn record_missed_on_startup(&self, now: DateTime<Local>) {
        for task in self.list() {
            if !task.enabled {
                continue;
            }
            let Some(last) = self
                .runs(&task.id)
                .into_iter()
                .find(|r| r.status == "ok" || r.status == "failed" || r.status == "running")
            else {
                continue; // 从未触发过：无「错过」基准
            };
            if missed_since(&task.cron, &last.fired_at, now) {
                self.append_run(
                    &task.id,
                    &TaskRun {
                        fired_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        status: "missed".into(),
                        session_id: None,
                        duration_ms: None,
                        error: Some("hub 停机期间错过触发（不补跑）".into()),
                    },
                );
            }
        }
    }
}

// —— 动作执行 ——

/// 渲染并执行任务动作；返回 spawn 产物的 agent id（inject 无）。
/// 驱动不在线/未知 driver → Err（调用方记 failed）；cwd 校验复用 API 层同款。
pub async fn execute_action(
    state: &SharedState,
    action: &TaskAction,
) -> anyhow::Result<Option<String>> {
    let now = Local::now();
    match action {
        TaskAction::Spawn {
            driver,
            cwd,
            name_template,
            prompt_template,
        } => {
            crate::api::agents::validate_cwd(cwd).await?;
            let d = state.drivers.get(driver)?;
            let prompt = render_template(prompt_template, now);
            let name = name_template.as_ref().map(|t| render_template(t, now));
            let id = d
                .start(&StartSpec {
                    prompt,
                    name,
                    model: None,
                    effort: None,
                    cwd: cwd.clone(),
                })
                .await?;
            Ok(Some(id))
        }
        TaskAction::Inject {
            driver,
            session_id,
            text,
        } => {
            let d = state.drivers.get(driver)?;
            d.send(session_id, &render_template(text, now)).await?;
            Ok(None)
        }
    }
}

// —— 调度循环 ——

/// 调度 tick：15s（分钟粒度 cron 的最大触发延迟；同一分钟去重防双发）。
const SCHED_TICK_SECS: u64 = 15;

/// 尝试独占获取本 tasks_dir 的调度权（agent-hub-schedule-lock S1/S4）：平台驱动层
/// 排他文件锁（platform::try_lock_file）非阻塞获取——拿到返回 Some(FileLock)
/// （保活即持锁，进程死亡 OS 自动释放，kill -9 亦然）；被他实例持有返回 None
/// （本实例进 standby）。锁文件落在 tasks_dir 内：不同 TASKS_DIR 的实例天然互不
/// 协商（全量写路径隔离的既有语义）。
pub fn try_acquire_scheduler_lock(dir: &Path) -> Option<platform::FileLock> {
    platform::try_lock_file(&dir.join("scheduler.lock"))
}

pub fn spawn_scheduler(state: SharedState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // 启动错过检测（一次性；停机期间的槽位记 missed 不补跑）
        state.tasks.record_missed_on_startup(Local::now());
        // 调度权协商（S1）：首尝试即拿锁 = 主实例；拿不到 = standby（不调度，
        // 每 tick 重试，前持锁实例死亡（含 kill -9）后锁自动释放，本实例接管）
        let mut lock = try_acquire_scheduler_lock(state.tasks.dir());
        state.tasks.set_scheduler_primary(lock.is_some());
        if lock.is_some() {
            tracing::info!("调度权取得（主实例）");
        } else {
            tracing::info!("调度权被他实例持有，本实例 standby（接管窗口 ≤ 一个 tick）");
        }
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(SCHED_TICK_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 槽位判定用「上次扫描以来是否经过匹配时刻」而非 is_time_matching——
        // croner 秒域缺省 0，is_time_matching 只在第 0 秒命中，15s tick 几乎必错过
        let mut last_scan = Local::now();
        loop {
            ticker.tick().await;
            // 共享文件为事实源（S4 边界修复）：他实例动态 CRUD 只落盘，本实例
            // 每 tick 前 reload，防接管/调度用旧内存快照漏调度
            state.tasks.reload_from_disk();
            // standby：重试拿锁；接管成功后 last_scan 对齐当前时刻——接管窗口内
            // 的槽位不补跑（前持锁实例可能已触发过，宁可漏跑不错跑，与单实例
            // 停机「错过不补跑」语义一致）
            if lock.is_none() {
                lock = try_acquire_scheduler_lock(state.tasks.dir());
                if lock.is_some() {
                    state.tasks.set_scheduler_primary(true);
                    last_scan = Local::now();
                    tracing::info!("调度权接管（前持锁实例已退出）");
                }
                continue;
            }
            let now = Local::now();
            for task in state.tasks.list() {
                if !task.enabled {
                    continue;
                }
                let due = Cron::from_str(&task.cron)
                    .ok()
                    .and_then(|cron| cron.find_next_occurrence(&last_scan, false).ok())
                    .is_some_and(|next| next.with_timezone(&Local) <= now);
                if !due {
                    continue;
                }
                fire(state.clone(), &task, now);
            }
            last_scan = now;
        }
    })
}

/// 触发一次（调度器与手动 run 共用）：串行守卫 → running 记录 → 异步执行不阻塞调度。
pub fn fire(state: SharedState, spec: &TaskSpec, now: DateTime<Local>) {
    let fired_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if !state.tasks.try_begin(&spec.id) {
        state.tasks.append_run(
            &spec.id,
            &TaskRun {
                fired_at,
                status: "skipped".into(),
                session_id: None,
                duration_ms: None,
                error: Some("上一次触发仍在执行".into()),
            },
        );
        return;
    }
    state.tasks.append_run(
        &spec.id,
        &TaskRun {
            fired_at: fired_at.clone(),
            status: "running".into(),
            session_id: None,
            duration_ms: None,
            error: None,
        },
    );
    let started = std::time::Instant::now();
    let spec = spec.clone(); // 脱离调用方生命周期（tokio::spawn 要求 'static）
    tokio::spawn(async move {
        let result = execute_action(&state, &spec.action).await;
        let run = TaskRun {
            fired_at,
            status: if result.is_ok() { "ok" } else { "failed" }.into(),
            session_id: result.as_ref().ok().and_then(|id| id.clone()),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            error: result.as_ref().err().map(|e| format!("{e:#}")),
        };
        state.tasks.append_run(&spec.id, &run);
        state.tasks.end(&spec.id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_valid_accepts_5_and_6_field_and_rejects_garbage() {
        assert!(cron_valid("0 30 9 * * *"));
        assert!(cron_valid("30 9 * * *"));
        assert!(!cron_valid("not a cron"));
        assert!(!cron_valid("* * * *"));
    }

    #[test]
    fn render_template_replaces_date_and_time() {
        let now = Local::now();
        let out = render_template("日报 {{date}} {{time}}", now);
        assert_eq!(
            out,
            format!("日报 {} {}", now.format("%Y-%m-%d"), now.format("%H:%M"))
        );
        assert!(!out.contains("{{"));
    }

    #[test]
    fn next_run_at_returns_future_local_rfc3339() {
        let now = Local::now();
        let next = next_run_at("0 0 1 * * *", now).expect("daily 01:00 has a next");
        let parsed = DateTime::parse_from_rfc3339(&next).expect("rfc3339");
        assert!(parsed.with_timezone(&Local) > now);
    }

    #[test]
    fn missed_since_true_when_slot_passed_and_false_when_up_to_date() {
        let now = Local::now();
        // 上次触发 = 昨天 → 每天 01:00 的任务在昨天之后必然已漏一次
        let yesterday =
            (now - chrono::Duration::hours(26)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(missed_since("0 0 1 * * *", &yesterday, now));
        // 刚刚触发过 → 未错过
        let just_now =
            (now - chrono::Duration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(!missed_since("0 0 1 * * *", &just_now, now));
        assert!(!missed_since("bad cron", &just_now, now));
    }

    #[test]
    fn store_upsert_remove_and_atomic_reload() {
        let dir = std::env::temp_dir().join(format!("hub-tasks-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = TaskManager::new(dir.clone());
        let spec = TaskSpec {
            id: "t1".into(),
            name: "日报".into(),
            cron: "0 30 9 * * *".into(),
            enabled: true,
            source: None,
            action: TaskAction::Inject {
                driver: "claude".into(),
                session_id: "s1".into(),
                text: "hi".into(),
            },
        };
        mgr.upsert(spec.clone()).unwrap();
        // 另一个实例从盘上重载读到同一份
        let mgr2 = TaskManager::new(dir.clone());
        assert_eq!(mgr2.list().len(), 1);
        assert_eq!(mgr2.get("t1").unwrap().name, "日报");
        // 替换 + 删除
        let mut updated = spec.clone();
        updated.enabled = false;
        mgr.upsert(updated).unwrap();
        assert!(!mgr.get("t1").unwrap().enabled);
        assert!(mgr.remove("t1").unwrap());
        assert!(!mgr.remove("t1").unwrap());
        assert!(mgr.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_log_appends_and_rolls_at_cap() {
        let dir = std::env::temp_dir().join(format!("hub-runs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = TaskManager::new(dir.clone());
        let mk = |i: usize| TaskRun {
            fired_at: format!("2026-01-01T00:{:02}:00+08:00", i % 60),
            status: "ok".into(),
            session_id: None,
            duration_ms: Some(5),
            error: None,
        };
        for i in 0..(RUN_CAP + 10) {
            mgr.append_run("t", &mk(i));
        }
        let runs = mgr.runs("t");
        assert_eq!(runs.len(), RUN_CAP);
        // 最近优先：最后写入的 i=RUN_CAP+9 在首位
        assert_eq!(runs[0].fired_at, mk(RUN_CAP + 9).fired_at);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_guard_serializes_same_task_only() {
        let dir = std::env::temp_dir().join(format!("hub-guard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = TaskManager::new(dir.clone());
        assert!(mgr.try_begin("t1"));
        assert!(!mgr.try_begin("t1")); // 在途 → 拒绝（记 skipped 的前提）
        assert!(mgr.try_begin("t2")); // 不同任务互不影响
        mgr.end("t1");
        assert!(mgr.try_begin("t1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R8（schedule-lock）S1：排他文件锁跨 fd 互斥——同进程两次独立 open 也互斥
    ///（unix 后端 flock 绑 open file description，与跨进程语义同源）；S4：持锁方
    /// drop（= 进程死亡路径）后锁释放，等待方可接管。
    #[test]
    fn scheduler_lock_is_exclusive_and_released_on_drop() {
        // 目录名带纳秒唯一化：并行测试线程 + 连续重跑防同名互撞
        let dir = std::env::temp_dir().join(format!(
            "hub-schedlock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = try_acquire_scheduler_lock(&dir);
        assert!(first.is_some(), "首实例应取得调度权");
        let second = try_acquire_scheduler_lock(&dir);
        assert!(second.is_none(), "持锁期间他实例不得取得（S1 互斥）");
        drop(first); // = 持锁进程死亡（kill -9 同路径：OS 释放锁）
                     // Darwin 的 flock 后端释放存在短暂竞态（drop 返回 ≠ 内核立即可重取）：
                     // 同步模拟生产 standby 的 tick 重试语义，窗口内多次尝试再断言
        let mut third = None;
        for _ in 0..50 {
            third = try_acquire_scheduler_lock(&dir);
            if third.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            third.is_some(),
            "锁释放后等待方应接管（S4，含释放竞态重试）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S4 边界（tester-r1 FAIL①）：他实例动态 CRUD 只落盘——reload_from_disk
    /// 以共享文件为事实源，接管/调度不再用启动期旧快照。
    #[test]
    fn reload_from_disk_picks_up_external_crud() {
        let dir = std::env::temp_dir().join(format!(
            "hub-reload-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = TaskManager::new(dir.clone());
        assert!(mgr.list().is_empty()); // standby 启动期快照为空
                                        // 「他实例」写入共享 tasks.json
        let other = TaskManager::new(dir.clone());
        other
            .upsert(TaskSpec {
                id: "t-x".into(),
                name: "他实例建的".into(),
                cron: "0 0 1 * * *".into(),
                enabled: true,
                source: None,
                action: TaskAction::Inject {
                    driver: "claude".into(),
                    session_id: "s".into(),
                    text: "hi".into(),
                },
            })
            .unwrap();
        assert!(mgr.get("t-x").is_none()); // reload 前本实例内存无
        mgr.reload_from_disk();
        assert_eq!(mgr.get("t-x").unwrap().name, "他实例建的");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S2：调度权标志位读写（scheduler-status 端点数据源）。
    #[test]
    fn scheduler_primary_flag_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hub-schedflag-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = TaskManager::new(dir.clone());
        assert!(!mgr.scheduler_primary()); // 默认 standby
        mgr.set_scheduler_primary(true);
        assert!(mgr.scheduler_primary());
        mgr.set_scheduler_primary(false);
        assert!(!mgr.scheduler_primary());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
