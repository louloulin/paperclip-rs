//! **顺序**这条契约本身：越过闸门 1 之前，任何拒绝都不得回显 task 派生的 issue id。
//!
//! 上游 `squad.go:1043-1051` 逐字写着为什么 `GetAgentTask` 的全局性让顺序变成安全判据：
//! 「拿一个不相干的任务 id，通过一个调用者能合法读到的 issue 探过来，就会套出**别的
//! workspace** 的 issue id」。本用例把这条性质钉成三发探测：
//!
//! | # | 形态 | 期望 | 为什么不回显 |
//! | --- | --- | --- | --- |
//! | A | 自封成**不是**这条 task 的 agent | 403 | 闸门 1 在「回显 task 的 issue id」那一步**之前** |
//! | B | 跨 workspace 的 task | 400 | 第 5 步的租户收窄（`JOIN agent`）先把它变成「查不到」 |
//! | C | **就是**这条 task 的 agent（对照） | 400 + 回显 | 过了闸门 1 之后，回显是**允许**的 —— 位置才是缺陷，回显不是 |

use axum::http::StatusCode;
use serde_json::json;

use crate::support::{
    app_with_db, cleanup, connect, err_message, insert_task, path, seed, send, written_rows,
    AGENT_ID_HEADER, TASK_ID_HEADER,
};

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn unauthorized_probes_never_echo_a_task_derived_issue_id() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let app = app_with_db(db);
    let fixture = seed(&pool).await;
    let other_issue = fixture.other_issue_id.to_string();
    let leader = fixture.leader_agent.to_string();

    // 这条 task 入队给 `other_agent`（**不是** squad 的 leader）、跑在 `other_issue_id` 上。
    let task = insert_task(
        &pool,
        fixture.other_agent,
        fixture.runtime_id,
        Some(fixture.other_issue_id),
        true,
        Some(fixture.squad_id),
    )
    .await;
    let task_s = task.to_string();
    let probe_body = Some(json!({"outcome": "action"}));

    // (A) 调用者自封成 `leader_agent` —— 不是这条 task 的 agent ⇒ 闸门 1 拦下 ⇒ 403，
    //     于是第 7 步（会回显 task 的 issue id 的那一步）根本走不到。
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        probe_body.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "闸门 1 必须先于「回显 task 的 issue id」那一步: {body}"
    );
    assert!(
        !body.to_string().contains(&other_issue),
        "越权探测不得套出 task 的 issue id: {body}"
    );

    // (B) 跨 workspace 的 task：连租户收窄（第 5 步）都过不去 ⇒ 400，
    //     且**绝不**回显另一个 workspace 的 issue id。
    let foreign_issue = fixture.foreign_issue_id.to_string();
    let foreign_agent = fixture.foreign_agent.to_string();
    let foreign_task = insert_task(
        &pool,
        fixture.foreign_agent,
        fixture.foreign_runtime_id,
        Some(fixture.foreign_issue_id),
        true,
        None,
    )
    .await;
    let foreign_task_s = foreign_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, foreign_task_s.as_str()),
            (AGENT_ID_HEADER, foreign_agent.as_str()),
        ],
        &path(fixture.issue_id),
        probe_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        !body.to_string().contains(&foreign_issue),
        "跨 workspace 探测不得套出别人的 issue id: {body}"
    );

    // (C) 对照：调用者**就是**这条 task 的 agent ⇒ 闸门 1 过 ⇒ 第 7 步照常回显。
    let other_agent = fixture.other_agent.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, task_s.as_str()),
            (AGENT_ID_HEADER, other_agent.as_str()),
        ],
        &path(fixture.issue_id),
        probe_body,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "对照：合法调用者 {body}");
    assert!(
        err_message(&body).contains(&other_issue),
        "过了闸门 1 之后才允许回显: {body}"
    );

    // 三发都没有写入。
    assert_eq!(
        written_rows(&pool, fixture.issue_id).await,
        0,
        "被拒的探测不得留下判决行"
    );

    cleanup(&fixture).await;
}
