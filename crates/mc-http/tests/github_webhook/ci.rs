//! 三族 CI 事件（`check_suite` / `check_run` / `status`，纯触发器）的端到端用例
//! （M8-4 的 `DoD`：三族事件各一条 + 「只对已镜像的 PR 入队」）。
//!
//! 与 `webhook.rs` 分文件是门 ⑩ 的结果（单文件 800 行硬上限）：`webhook.rs` 放入口/反例 +
//! `pull_request` 族 + `installation` 族，本文件放 CI 族的四条断言。
//!
//! ⚠️ 每条用例都持有串行许可（[`serial`]）：快照端口是进程全局的注入槽（见 `support.rs`）。

use std::sync::Arc;

use axum::http::StatusCode;
use mc_http::routes::github::webhook::{reset_pr_refresh_port, set_pr_refresh_port};
use serde_json::json;

use super::support::*;

// ---------------------------------------------------------------------------
// check_suite 族（纯触发器）
// ---------------------------------------------------------------------------

/// 用一帧真实 webhook 把 PR 行镜像出来（CI 用例的公共前置；`head_sha` 由调用方给）。
async fn mirror_pr_for_ci(app: &axum::Router, installation_id: i64, number: i32, head_sha: &str) {
    let frame = json!({
        "action": "opened",
        "pull_request": {
            "number": number, "html_url": format!("https://github.com/acme/api/pull/{number}"),
            "title": "t", "state": "open", "draft": false, "merged": false,
            "created_at": "2026-09-25T06:00:00Z", "updated_at": "2026-09-25T06:00:00Z",
            "head": { "ref": "feat/ci", "sha": head_sha }
        },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "installation": { "id": installation_id }
    });
    let (status, _) = call(
        app,
        signed_webhook_request(Some(WEBHOOK_SECRET), "pull_request", &frame),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

/// 一帧 CI 事件的公共骨架（`status` 没有 `check_suite` 段，用顶层 `sha`；其余两族带
/// `check_suite`）。`extra` 里的键逐个覆盖上去，让每个用例只写自己关心的那一段。
fn ci_frame(
    event: &str,
    installation_id: i64,
    extra: impl Into<serde_json::Value>,
) -> serde_json::Value {
    let mut frame = json!({
        "installation": { "id": installation_id },
        "repository": { "name": "api", "owner": { "login": "acme" } },
    });
    if event != "status" {
        frame["check_suite"] = json!({ "head_sha": "sha-a", "pull_requests": [] });
    }
    for (key, value) in extra.into().as_object().cloned().unwrap_or_default() {
        frame[key] = value;
    }
    frame
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn ci_direct_pr_numbers_enqueue_once_and_only_when_configured() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    let _ = seed_issue(
        &pool,
        workspace_id,
        user_id,
        7,
        &format!("{prefix}-7"),
        "todo",
    )
    .await;

    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    mirror_pr_for_ci(&app, installation_id, 5, "sha-a").await;
    let baseline = port.enqueued().len();

    // ① `check_suite` 直接带 PR 号 ⇒ 入队（reason 仍是 webhook）；同一号出现两次只入队一次。
    let (status, _) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "check_suite",
            &ci_frame(
                "check_suite",
                installation_id,
                json!({ "check_suite": { "head_sha": "sha-a", "pull_requests": [{ "number": 5 }, { "number": 5 }] } }),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        port.enqueued().len(),
        baseline + 1,
        "去重：同一号只入队一次"
    );
    assert_eq!(port.enqueued()[baseline].pr_number, 5);
    assert_eq!(
        port.enqueued()[baseline].reason,
        mc_vcs_github::port::RefreshReason::Webhook
    );

    // ② 未配置（`enabled() == false`）⇒ CI 事件一次都不入队（上游 `if !h.PRRefresh.Enabled()`）。
    reset_pr_refresh_port();
    set_pr_refresh_port(Arc::new(mc_vcs_github::port::DisabledPrRefresh));
    let (status, _) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "check_suite",
            &ci_frame(
                "check_suite",
                installation_id,
                json!({ "check_suite": { "head_sha": "sha-a", "pull_requests": [{ "number": 5 }] } }),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(port.enqueued().len(), baseline + 1, "未配置 ⇒ 不入队");

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn ci_status_events_resolve_the_pr_by_head_sha() {
    let _serial = serial().await;
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (workspace_id, user_id, prefix) = seed_workspace(&pool, "owner").await;
    let installation_id = unique_installation_id();
    seed_installation(&pool, workspace_id, installation_id, "acme").await;
    let _ = seed_issue(
        &pool,
        workspace_id,
        user_id,
        7,
        &format!("{prefix}-7"),
        "todo",
    )
    .await;

    let port = RecordingPort::new(true);
    set_pr_refresh_port(port.as_port());
    let (app, _state) = app_with(db, webhook_keys(Some(WEBHOOK_SECRET)));
    mirror_pr_for_ci(&app, installation_id, 5, "sha-a").await;
    let baseline = port.enqueued().len();

    // ① `status` 事件只带 SHA ⇒ 回查 `head_sha` 找到 PR 5，并把解析到的 SHA 带进入队载荷。
    let (status, _) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "status",
            &ci_frame("status", installation_id, json!({ "sha": "sha-a" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(port.enqueued().len(), baseline + 1);
    assert_eq!(port.enqueued()[baseline].pr_number, 5);
    assert_eq!(port.enqueued()[baseline].head_sha.as_deref(), Some("sha-a"));

    // ② 认不出的 SHA ⇒ 一次都不入队（本仓端口按 workspace 定位，只有已镜像的 PR 能入队）。
    let (status, _) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "status",
            &ci_frame("status", installation_id, json!({ "sha": "sha-unknown" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(port.enqueued().len(), baseline + 1);

    // ③ `installation.id == 0` 的载荷 ⇒ 直接返回（没有可归属的 workspace）。
    let (status, _) = call(
        &app,
        signed_webhook_request(
            Some(WEBHOOK_SECRET),
            "check_suite",
            &ci_frame(
                "check_suite",
                0,
                json!({ "check_suite": { "head_sha": "sha-a", "pull_requests": [{ "number": 5 }] } }),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(port.enqueued().len(), baseline + 1);

    cleanup(&pool, workspace_id, &[user_id]).await;
    reset_pr_refresh_port();
}
