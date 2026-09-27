//! 跨会话任务聚合测试（P5 preview 方案 A）：真实 jsonl 形态 fixture。
use std::fs;

use server::drivers::claude::tasks::aggregate_project_tasks;

fn write_session(dir: &std::path::Path, name: &str, lines: &[String]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{name}.jsonl")), lines.join("\n")).unwrap();
}

/// 构造一条 harness 形态的 tool_use 行。
fn tool_use(name: &str, id: &str, input_json: &str) -> String {
    format!(
        r#"{{"message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input_json}}}]}}}}"#
    )
}

/// 构造一条 harness 形态的 tool_result 行。
fn tool_result(use_id: &str, text: &str) -> String {
    serde_json::json!({
        "message": { "content": [{ "type": "tool_result", "tool_use_id": use_id, "content": text }] }
    })
    .to_string()
}

fn ids(tasks: &[server::drivers::claude::tasks::TaskEntry]) -> Vec<String> {
    tasks.iter().map(|t| t.task_id.clone()).collect()
}

#[tokio::test]
async fn merges_updates_across_sessions_and_ranks_completed_highest() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    // 会话 A：Create #5 + in_progress（完成记录在会话 B）
    write_session(
        &proj,
        "session-a",
        &[
            tool_use("TaskCreate", "toolu_a", r#"{"subject":"落 src/ 源码工程"}"#),
            tool_result("toolu_a", "Task #5 created successfully: 落 src/ 源码工程"),
            tool_use(
                "TaskUpdate",
                "toolu_b",
                r#"{"taskId":"5","status":"in_progress"}"#,
            ),
        ],
    );
    // 会话 B：#5 completed（跨会话补齐的关键）
    write_session(
        &proj,
        "session-b",
        &[tool_use(
            "TaskUpdate",
            "toolu_c",
            r#"{"taskId":"5","status":"completed"}"#,
        )],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-a",
    )
    .await
    .expect("事件池非空应有权威视角");
    assert_eq!(ids(&tasks), vec!["5".to_string()]);
    assert_eq!(tasks[0].status, "completed");
}

#[tokio::test]
async fn calibrates_to_last_tasklist_snapshot_of_current_session() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    // 当前会话：Create #1..#4（#1/#2 已被 harness 清理）+ 最后一次 TaskList 只列 #3/#4
    write_session(
        &proj,
        "session-cur",
        &[
            tool_use("TaskCreate", "t1", r#"{"subject":"旧任务一"}"#),
            tool_result("t1", "Task #1 created successfully: 旧任务一"),
            tool_use("TaskCreate", "t2", r#"{"subject":"旧任务二"}"#),
            tool_result("t2", "Task #2 created successfully: 旧任务二"),
            tool_use("TaskCreate", "t3", r#"{"subject":"有效任务"}"#),
            tool_result("t3", "Task #3 created successfully: 有效任务"),
            tool_use("TaskCreate", "t4", r#"{"subject":"排队任务"}"#),
            tool_result("t4", "Task #4 created successfully: 排队任务"),
            // 真实流序：TaskList 的 tool_use 在其 result 之前；result 文本内换行为转义 \n（真实 jsonl 单行）
            tool_use("TaskList", "tl", "{}"),
            tool_result("tl", "#3 [completed] 有效任务\n#4 [pending] 排队任务"),
        ],
    );
    // 另一会话 Create #1（跨会话合并池）
    write_session(
        &proj,
        "session-old",
        &[tool_use(
            "TaskUpdate",
            "x",
            r#"{"taskId":"1","status":"completed"}"#,
        )],
    );

    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("有效快照应有权威视角");
    // TaskList 校准：#1/#2 被过滤，只剩有效集
    assert_eq!(ids(&tasks), vec!["3".to_string(), "4".to_string()]);
    assert_eq!(tasks[0].status, "completed");
    assert_eq!(tasks[1].status, "pending");
}

#[tokio::test]
async fn snapshot_adds_missing_tasks_and_overrides_event_subject_and_status() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    let mut lines = Vec::new();
    // r27：事件只有六项，快照才包含完整九项；旧事件状态与标题不能覆盖快照。
    for id in [27, 28, 32, 33, 34, 35] {
        let uid = format!("t{id}");
        lines.push(tool_use("TaskCreate", &uid, r#"{"subject":"旧标题"}"#));
        lines.push(tool_result(
            &uid,
            &format!("Task #{id} created successfully"),
        ));
        lines.push(tool_use(
            "TaskUpdate",
            &format!("u{id}"),
            &format!(r#"{{"taskId":"{id}","status":"completed"}}"#),
        ));
    }
    lines.push(tool_use("TaskList", "tl", "{}"));
    let snapshot = (27..=35)
        .map(|id| {
            let status = if id == 28 { "pending" } else { "completed" };
            format!("#{id} [{status}] 最新任务{id}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    lines.push(tool_result("tl", &snapshot));
    write_session(&proj, "session-cur", &lines);
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("有效快照应有权威视角");
    assert_eq!(
        ids(&tasks),
        (27..=35).map(|id| id.to_string()).collect::<Vec<_>>()
    );
    for task in tasks {
        assert_eq!(task.subject, format!("最新任务{}", task.task_id));
        assert_eq!(
            task.status,
            if task.task_id == "28" {
                "pending"
            } else {
                "completed"
            }
        );
    }
}

#[tokio::test]
async fn snapshot_without_any_create_events_is_still_authoritative() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    write_session(
        &proj,
        "session-cur",
        &[
            tool_use("TaskList", "tl", "{}"),
            tool_result("tl", "#29 [completed] 子会话任务"),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("有效快照应有权威视角");
    assert_eq!(ids(&tasks), vec!["29"]);
    assert_eq!(tasks[0].subject, "子会话任务");
    assert_eq!(tasks[0].status, "completed");
}

#[tokio::test]
async fn newer_snapshot_supersedes_fresh_tasks_it_excludes() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    write_session(
        &proj,
        "session-cur",
        &[
            tool_use("TaskCreate", "t8", r#"{"subject":"长期任务"}"#),
            tool_result("t8", "Task #8 created successfully"),
            tool_use("TaskList", "tl1", "{}"),
            tool_result("tl1", "#8 [pending] 长期任务"),
            // 快照 A 后新建 #9，但更新的快照 B（权威）已不含 #9（被清理/删除）
            tool_use("TaskCreate", "t9", r#"{"subject":"已清理"}"#),
            tool_result("t9", "Task #9 created successfully"),
            tool_use("TaskList", "tl2", "{}"),
            tool_result("tl2", "#8 [pending] 长期任务"),
            // 快照 B 后新建 #10 → 叠加保留
            tool_use("TaskCreate", "t10", r#"{"subject":"新鲜任务"}"#),
            tool_result("t10", "Task #10 created successfully"),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("有效快照应有权威视角");
    // review-ui-r7 边界：#9 不得借第一次快照后的 fresh 身份复活
    assert_eq!(ids(&tasks), vec!["8", "10"]);
}

#[tokio::test]
async fn deleted_update_removes_task_from_pool_and_snapshot_view() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    // 事件：Create #1/#2 + #1 completed；快照（旧）仍列 #1；其后 #1 deleted
    write_session(
        &proj,
        "session-cur",
        &[
            tool_use("TaskCreate", "t1", r#"{"subject":"被删除"}"#),
            tool_result("t1", "Task #1 created successfully"),
            tool_use("TaskCreate", "t2", r#"{"subject":"保留"}"#),
            tool_result("t2", "Task #2 created successfully"),
            tool_use("TaskUpdate", "u1", r#"{"taskId":"1","status":"completed"}"#),
            tool_use("TaskList", "tl", "{}"),
            tool_result("tl", "#1 [completed] 被删除\n#2 [pending] 保留"),
            // 删除发生在快照之后：权威快照口径也必须被删除语义覆盖
            tool_use("TaskUpdate", "u2", r#"{"taskId":"1","status":"deleted"}"#),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("有效快照应有权威视角");
    assert_eq!(ids(&tasks), vec!["2"]);

    // 无快照口径：事件池同样剔除 deleted
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    write_session(
        &proj,
        "session-cur",
        &[
            tool_use("TaskCreate", "t1", r#"{"subject":"被删除"}"#),
            tool_result("t1", "Task #1 created successfully"),
            tool_use("TaskCreate", "t2", r#"{"subject":"保留"}"#),
            tool_result("t2", "Task #2 created successfully"),
            tool_use("TaskUpdate", "u1", r#"{"taskId":"1","status":"deleted"}"#),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("事件池非空应有权威视角");
    assert_eq!(ids(&tasks), vec!["2"]);
}

#[tokio::test]
async fn distinguishes_authoritative_empty_invalid_and_fallback_views() {
    // 权威空快照：Some(空)（API 序列化为 []，前端不得回退复活旧任务）
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    write_session(
        &proj,
        "session-cur",
        &[
            tool_use("TaskCreate", "t1", r#"{"subject":"旧任务"}"#),
            tool_result("t1", "Task #1 created successfully"),
            tool_use("TaskList", "tl", "{}"),
            tool_result("tl", "No tasks found"),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("权威空快照不是无视角");
    assert!(tasks.is_empty());

    // 目录/会话缺失：None（前端回退单会话口径）
    let tmp = tempfile::tempdir().unwrap();
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-missing",
    )
    .await;
    assert!(tasks.is_none());
}

#[tokio::test]
async fn snapshot_keeps_tasks_created_after_it_in_the_current_session() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    write_session(
        &proj,
        "session-cur",
        &[
            // 快照前创建 #9（harness 已清理，不在快照）→ 应被权威快照过滤
            tool_use("TaskCreate", "t9", r#"{"subject":"已清理"}"#),
            tool_result("t9", "Task #9 created successfully"),
            tool_use("TaskList", "tl", "{}"),
            tool_result("tl", "#1 [completed] 快照任务甲\n#2 [pending] 快照任务乙"),
            // 快照后新建 #10 → 叠加保留（否则要等下一次 TaskList 才可见）
            tool_use("TaskCreate", "t10", r#"{"subject":"新鲜任务"}"#),
            tool_result("t10", "Task #10 created successfully"),
            tool_use(
                "TaskUpdate",
                "u10",
                r#"{"taskId":"10","status":"in_progress"}"#,
            ),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-cur",
    )
    .await
    .expect("有效快照应有权威视角");
    assert_eq!(ids(&tasks), vec!["1", "2", "10"]);
    let fresh = tasks.iter().find(|t| t.task_id == "10").unwrap();
    assert_eq!(fresh.subject, "新鲜任务");
    assert_eq!(fresh.status, "in_progress");
}

#[tokio::test]
async fn distinguishes_empty_missing_subject_and_invalid_snapshots() {
    for (snapshot, status) in [
        ("#1 [pending]", "pending"),
        ("#1 [unknown] 标题", "completed"),
        ("#1 malformed", "completed"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects").join("-Users-demo-proj");
        write_session(
            &proj,
            "session-cur",
            &[
                tool_use("TaskCreate", "t1", r#"{"subject":"事件标题"}"#),
                tool_result("t1", "Task #1 created successfully"),
                tool_use("TaskUpdate", "u1", r#"{"taskId":"1","status":"completed"}"#),
                tool_use("TaskList", "tl", "{}"),
                tool_result("tl", snapshot),
            ],
        );
        let tasks = aggregate_project_tasks(
            tmp.path(),
            std::path::Path::new("/Users/demo/proj"),
            "session-cur",
        )
        .await
        .expect("事件池非空应有权威视角");
        assert_eq!(ids(&tasks), vec!["1"]);
        assert_eq!(tasks[0].subject, "事件标题");
        assert_eq!(tasks[0].status, status);
    }
}

#[tokio::test]
async fn returns_all_events_when_no_tasklist_call_present() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    write_session(
        &proj,
        "session-x",
        &[
            tool_use("TaskCreate", "t1", r#"{"subject":"任务一"}"#),
            tool_result("t1", "Task #1 created successfully: 任务一"),
            tool_use("TaskCreate", "t2", r#"{"subject":"任务二"}"#),
            tool_result("t2", "Task #2 created successfully: 任务二"),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-x",
    )
    .await
    .expect("事件池非空应有权威视角");
    // 无 TaskList 调用 → 全量显示历史事件
    assert_eq!(ids(&tasks), vec!["1".to_string(), "2".to_string()]);
}

#[tokio::test]
async fn keeps_events_when_snapshot_format_is_unrecognized() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    // 当前会话 TaskList 快照形态不匹配（无 #N 行 → 解析出的 tid 与 realId 无交集）
    write_session(
        &proj,
        "session-y",
        &[
            tool_use("TaskCreate", "t1", r#"{"subject":"任务一"}"#),
            tool_result("t1", "Task #1 created successfully: 任务一"),
            tool_use("TaskCreate", "t2", r#"{"subject":"任务二"}"#),
            tool_result("t2", "Task #2 created successfully: 任务二"),
            tool_use("TaskList", "tl", "{}"),
            tool_result("tl", "- [ ] checkbox 形态待办甲\n- [x] checkbox 形态待办乙"),
        ],
    );
    let tasks = aggregate_project_tasks(
        tmp.path(),
        std::path::Path::new("/Users/demo/proj"),
        "session-y",
    )
    .await
    .expect("事件池非空应有权威视角");
    // 无法识别的文本不是权威快照，不覆盖已解析的事件。
    assert_eq!(ids(&tasks), vec!["1".to_string(), "2".to_string()]);
}
