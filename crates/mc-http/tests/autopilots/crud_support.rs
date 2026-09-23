//! M5-2（LUM-1567）写面 HTTP 用例的共享夹具：种子、探针、小工具。
//!
//! 拆成独立模块的原因：`crud.rs` 与 `crud_access.rs` 都要用这些夹具，而「每个文件 800 行」
//! 是 `docs/44` §7 的 ⑦ `file-size` 门禁硬上限。夹具只负责把「真库 + 真 router」的前置条件
//! 准备好，断言一律留在用例里（覆盖表见 `crud.rs` 头部的 `DoD` 对照表与
//! `docs/50-M5-2-WRITE-FACE.md`）。

use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::support::{call, cleanup};

/// 上游 `Route("/api/autopilots") + Post("/")` ⇒ 两个注册键（`slash_alias_audit.py` 会判 `MISSING_ALIAS`）。
pub(super) const CREATE_URIS: [&str; 2] = ["/api/autopilots/", "/api/autopilots"];

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 创建载荷：`agent` 指派 + 指定 `execution_mode`；其余字段由调用方按需追加。
pub(super) fn create_payload(title: &str, assignee_id: Uuid, execution_mode: &str) -> Value {
    json!({
        "title": title,
        "assignee_id": assignee_id.to_string(),
        "execution_mode": execution_mode,
    })
}

/// 订阅者 wire 形态（`user_type` 库层只允许 `member`）。
pub(super) fn subscriber(user_id: Uuid) -> Value {
    json!({"user_type": "member", "user_id": user_id.to_string()})
}

/// 扁平错误体 `{"error": "msg", "code": "…"}`（403 / 409 专用，见 `crud.rs` 模块文档）。
pub(super) fn flat_error(body: &Value) -> (&str, &str) {
    (
        body.get("error")
            .and_then(Value::as_str)
            .unwrap_or("<no error>"),
        body.get("code")
            .and_then(Value::as_str)
            .unwrap_or("<no code>"),
    )
}

/// 响应的键集合（升序）——「哪些字段**在不在**」也是契约（`omitempty`）。
pub(super) fn sorted_keys(body: &Value) -> Vec<String> {
    let mut keys: Vec<String> = body.as_object().expect("object").keys().cloned().collect();
    keys.sort_unstable();
    keys
}

// ---------------------------------------------------------------------------
// 种子 / 探针
// ---------------------------------------------------------------------------

/// 建 `kind='user'` 的 agent。`ready=false` 造「无 runtime」；`archived=true` 造「已归档」；
/// `owner_id` 决定 squad 队长那条 invoke 门（`private` 队长只有 owner 调得动）。
pub(super) async fn seed_agent(
    pool: &PgPool,
    workspace_id: Uuid,
    owner_id: Uuid,
    ready: bool,
    archived: bool,
) -> Uuid {
    let runtime_id = if ready {
        Some(
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_runtime \
                    (workspace_id, name, runtime_mode, provider, status, owner_id) \
                 VALUES ($1, $2, 'local', 'claude', 'online', $3) RETURNING id",
            )
            .bind(workspace_id)
            .bind(format!("itest-ap-rt-{}", Uuid::new_v4()))
            .bind(owner_id)
            .fetch_one(pool)
            .await
            .expect("insert agent_runtime"),
        )
    } else {
        None
    };
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, runtime_id, owner_id, permission_mode, kind, \
             status, archived_at) \
         VALUES ($1, $2, 'local', $3, $4, 'private', 'user', 'offline', $5) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner_id)
    .bind(if archived {
        Some(chrono::Utc::now())
    } else {
        None
    })
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

pub(super) async fn seed_squad(
    pool: &PgPool,
    workspace_id: Uuid,
    leader_id: Uuid,
    creator_id: Uuid,
    archived: bool,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO squad (workspace_id, name, leader_id, creator_id, archived_at) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-squad-{}", Uuid::new_v4()))
    .bind(leader_id)
    .bind(creator_id)
    .bind(if archived {
        Some(chrono::Utc::now())
    } else {
        None
    })
    .fetch_one(pool)
    .await
    .expect("insert squad")
}

/// 一条 webhook 投递记录（删除只归档 ⇒ 它必须活下来）。
pub(super) async fn seed_delivery(
    pool: &PgPool,
    workspace_id: Uuid,
    autopilot_id: Uuid,
    trigger_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO webhook_delivery (workspace_id, autopilot_id, trigger_id, provider) \
         VALUES ($1, $2, $3, 'github') RETURNING id",
    )
    .bind(workspace_id)
    .bind(autopilot_id)
    .bind(trigger_id)
    .fetch_one(pool)
    .await
    .expect("insert webhook_delivery")
}

/// `autopilot_rule_version` 全量（`created_at` 升序）：`(id, published_by_id, config_summary)`。
pub(super) async fn rule_versions(
    pool: &PgPool,
    autopilot_id: Uuid,
) -> Vec<(Uuid, Option<Uuid>, Value)> {
    sqlx::query_as::<_, (Uuid, Option<Uuid>, Value)>(
        "SELECT id, published_by_id, config_summary FROM autopilot_rule_version \
         WHERE autopilot_id = $1 ORDER BY created_at, id",
    )
    .bind(autopilot_id)
    .fetch_all(pool)
    .await
    .expect("select autopilot_rule_version")
}

/// 订阅者集合（升序规范化 UUID）。
pub(super) async fn subscriber_ids(pool: &PgPool, autopilot_id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM autopilot_subscriber WHERE autopilot_id = $1 ORDER BY user_id",
    )
    .bind(autopilot_id)
    .fetch_all(pool)
    .await
    .expect("select autopilot_subscriber")
}

/// `(trigger, subscriber, collaborator, run)` 四张子表的行数。
pub(super) async fn child_counts(pool: &PgPool, autopilot_id: Uuid) -> (i64, i64, i64, i64) {
    sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT (SELECT count(*) FROM autopilot_trigger WHERE autopilot_id = $1), \
                (SELECT count(*) FROM autopilot_subscriber WHERE autopilot_id = $1), \
                (SELECT count(*) FROM autopilot_collaborator WHERE autopilot_id = $1), \
                (SELECT count(*) FROM autopilot_run WHERE autopilot_id = $1)",
    )
    .bind(autopilot_id)
    .fetch_one(pool)
    .await
    .expect("count autopilot children")
}

pub(super) async fn delivery_count(pool: &PgPool, autopilot_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM webhook_delivery WHERE autopilot_id = $1")
        .bind(autopilot_id)
        .fetch_one(pool)
        .await
        .expect("count webhook_delivery")
}

pub(super) async fn autopilot_state(pool: &PgPool, id: Uuid) -> (String, Option<String>) {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, pause_reason FROM autopilot WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("select autopilot state")
}

/// 触发器上的规则责任人（实质编辑会把它转给本次编辑者）。
pub(super) async fn trigger_publisher(pool: &PgPool, autopilot_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT published_by_id FROM autopilot_trigger WHERE autopilot_id = $1",
    )
    .bind(autopilot_id)
    .fetch_one(pool)
    .await
    .expect("select trigger publisher")
}

/// 建一个 autopilot 并返回 id（`201` 断言在内，省得每个用例都重写一遍）。
pub(super) async fn create_one(app: &Router, ws: Uuid, user: Uuid, payload: Value) -> Uuid {
    let (status, body) = call(app, "POST", CREATE_URIS[0], ws, user, Some(payload)).await;
    assert_eq!(status, 201, "create 必须 201: {body}");
    Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid")
}

/// 清场：本片自造的 squad / agent / `agent_runtime` 必须先删 —— `agent.workspace_id` 有外键，
/// 留着会让 `support::cleanup` 里的 `DELETE FROM workspace` **静默失败**并留下垃圾行。
pub(super) async fn cleanup_all(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    // autopilot 先走（trigger/subscriber/collaborator/run 都是它的级联子行）。
    let _ = sqlx::query("DELETE FROM autopilot WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for stmt in [
        "DELETE FROM squad WHERE workspace_id = $1",
        "DELETE FROM agent WHERE workspace_id = $1",
        "DELETE FROM agent_runtime WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(stmt).bind(workspace_id).execute(pool).await;
    }
    cleanup(pool, workspace_id, user_ids).await;
}
