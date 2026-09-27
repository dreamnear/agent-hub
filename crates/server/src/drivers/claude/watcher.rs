//! jobs 目录 watcher（notify，debounce 300ms）+ 兜底轮询（Tick）。
//! 错误路径：jobs_dir 不存在 → watcher warn! 后正常退出；broadcast Lagged → warn! 继续。

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tokio::{
    sync::{broadcast, mpsc},
    task::{self, JoinHandle},
};

const DEBOUNCE: Duration = Duration::from_millis(300);

/// WS 推送的顶层事件，前端收到任一事件即重拉 /api/agents。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum HubEvent {
    #[serde(rename = "jobs_changed")]
    JobsChanged { id: String },
    #[serde(rename = "tick")]
    Tick,
}

pub fn spawn_watch(jobs_dir: PathBuf, tx: broadcast::Sender<HubEvent>) -> JoinHandle<()> {
    task::spawn(async move {
        // macOS FSEvents 回调给 canonical 路径（/var/... 的 symlink → /private/var/...），
        // 先归一化 jobs_dir，保证 watch/strip_prefix/starts_with 与回调路径一致，
        // 否则事件全部被路径过滤丢弃（tester-r2 FAIL-4 根因）。
        let jobs_dir = jobs_dir.canonicalize().unwrap_or(jobs_dir);
        let arc_dir = Arc::new(jobs_dir.clone());
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<String>();
        // 各 job state.json 的写→ fs 事件；新短 id 子目录新建可能在 tempdir/FSEvents 下被遗漏，
        // 故两个解析分支都送消息，确保 file_stem 与相对路径任一命中都能派发。
        let (watcher, _real_dir): (RecommendedWatcher, PathBuf) = {
            let arc_for_cb = arc_dir.clone();
            let ev_for_cb = ev_tx.clone();
            let try_make = |dir: PathBuf| {
                let arc = arc_for_cb.clone();
                let ev = ev_for_cb.clone();
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(ev_msg) = res {
                        for path in &ev_msg.paths {
                            // 分支一：jobs_dir/<id>/... → 取 id 段
                            let id_from_rel = path
                                .strip_prefix(&*arc)
                                .ok()
                                .and_then(|rel| rel.components().next())
                                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                                .filter(|s| !s.is_empty() && s != ".");
                            if let Some(id) = id_from_rel {
                                let _ = ev.send(id);
                            } else if let Some(stem) =
                                path.file_stem().map(|s| s.to_string_lossy().into_owned())
                            {
                                // 分支二：叶文件 file_stem 兜底（FSEvents 对新子目录追踪延迟）
                                if path.starts_with(&*arc) {
                                    let _ = ev.send(stem);
                                }
                            }
                        }
                    }
                })
                .and_then(|mut w| {
                    w.watch(&dir, RecursiveMode::Recursive)?;
                    Ok((w, dir.clone()))
                })
            };
            let canonical = jobs_dir.canonicalize().unwrap_or_else(|_| jobs_dir.clone());
            match try_make(jobs_dir.clone()).or_else(|_| try_make(canonical.clone())) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "watcher init failed, exiting task");
                    return;
                }
            }
        };

        let _arc_watcher = Arc::new(std::mem::ManuallyDrop::new(watcher));
        let mut debounce: HashMap<String, Instant> = HashMap::new();
        let mut flush = tokio::time::interval(DEBOUNCE);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        flush.tick().await;

        loop {
            tokio::select! {
                _ = flush.tick() => {}
                some = ev_rx.recv() => {
                    match some {
                        Some(id) => { debounce.entry(id).or_insert(Instant::now()); }
                        None => { tracing::warn!("job event channel closed, watcher exiting"); return; }
                    }
                }
            }

            let expired: Vec<String> = debounce
                .iter()
                .filter(|(_, at)| at.elapsed() >= DEBOUNCE)
                .map(|(id, _)| id.clone())
                .collect();
            for id in expired {
                debounce.remove(&id);
                if let Err(e) = tx.send(HubEvent::JobsChanged { id }) {
                    tracing::warn!("jobs broadcast send failed, continuing: {:?}", e);
                }
            }
        }
    })
}

pub fn spawn_poll(tx: broadcast::Sender<HubEvent>, every: Duration) -> JoinHandle<()> {
    task::spawn(async move {
        let mut interval = tokio::time::interval(every);
        loop {
            interval.tick().await;
            if let Err(e) = tx.send(HubEvent::Tick) {
                tracing::warn!("poll broadcast send 失败, continuing: {:?}", e);
            }
        }
    })
}
