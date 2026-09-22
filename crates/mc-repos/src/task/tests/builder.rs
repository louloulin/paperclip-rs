use super::super::builder::NewBuilderSession;
use super::super::row::{ClientUsageUpsert, SaveDraftOutcome, SwitchRuntimeOutcome};
use super::*;
use mc_core::Id;
// ---------------------------------------------------------------------------
// agent-builder 与 client-usage 写路径

// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn builder_session_create_draft_and_switch_runtime() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let created = repo
        .create_builder_session(&NewBuilderSession {
            workspace_id: fx.workspace_id,
            creator_id: fx.user_id,
            runtime_id: fx.runtime_id,
            runtime_mode: "local".to_owned(),
            model: None,
        })
        .await
        .expect("create_builder_session");

    let (kind, system_key, carrier_runtime): (String, Option<String>, Uuid) =
        sqlx::query_as("SELECT kind, system_key, runtime_id FROM agent WHERE id = $1")
            .bind(created.builder_agent_id.0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert_eq!(kind, "system");
    assert_eq!(
        system_key.as_deref(),
        Some(format!("agent_builder:{}", created.session_id.0).as_str())
    );
    assert_eq!(carrier_runtime, fx.runtime_id.0);

    let explicit: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT explicitly_created_at FROM chat_session WHERE id = $1")
            .bind(created.session_id.0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert!(explicit.is_some(), "显式创建标记要落库");

    // 没有消息也没有草稿的会话不上列表；存一次草稿后出现。
    assert!(repo
        .list_builder_sessions(fx.workspace_id, fx.user_id)
        .await
        .unwrap()
        .iter()
        .all(|s| s.id != created.session_id.0));
    let draft = json!({"name": "Release notes", "permission_scope": "private"});
    assert_eq!(
        repo.save_builder_draft(created.session_id, fx.workspace_id, fx.user_id, &draft)
            .await
            .unwrap(),
        SaveDraftOutcome::Saved
    );
    let sessions = repo
        .list_builder_sessions(fx.workspace_id, fx.user_id)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, created.session_id.0);
    assert_eq!(sessions[0].runtime_id, Some(fx.runtime_id.0));
    assert_eq!(sessions[0].stored_draft.as_ref(), Some(&draft));

    // 切 runtime：载体 agent 重绑，chat_session.runtime_id 故意保持旧值。
    let other_runtime: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, status, \
             last_seen_at) VALUES ($1, 'itest-rt2', 'cloud', 'claude', 'online', now()) \
         RETURNING id",
    )
    .bind(fx.workspace_id.0)
    .fetch_one(fx.db.pool())
    .await
    .unwrap();
    let other = Id::from(other_runtime);
    assert_eq!(
        repo.switch_builder_runtime(
            created.session_id,
            fx.workspace_id,
            fx.user_id,
            other,
            "cloud",
        )
        .await
        .unwrap(),
        SwitchRuntimeOutcome::Rebound { runtime_id: other }
    );
    let (runtimes, mode, model): (Uuid, String, Option<String>) =
        sqlx::query_as("SELECT a.runtime_id, a.runtime_mode, a.model FROM agent a WHERE a.id = $1")
            .bind(created.builder_agent_id.0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert_eq!(runtimes, other.0);
    assert_eq!(mode, "cloud");
    assert!(
        model.is_none(),
        "换 runtime 要清 model（id 是 per-runtime 的）"
    );
    let stale_session_runtime: Option<Uuid> =
        sqlx::query_scalar("SELECT runtime_id FROM chat_session WHERE id = $1")
            .bind(created.session_id.0)
            .fetch_one(fx.db.pool())
            .await
            .unwrap();
    assert_eq!(stale_session_runtime, Some(fx.runtime_id.0));

    teardown(&fx).await;
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn builder_draft_rejects_non_carrier_and_unknown_session() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    // 普通用户 agent 的 chat 会话：形状不是 builder 载体。
    let plain_session: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_session (workspace_id, agent_id, creator_id, title) \
         VALUES ($1, $2, $3, 'plain') RETURNING id",
    )
    .bind(fx.workspace_id.0)
    .bind(fx.agent_id.0)
    .bind(fx.user_id.0)
    .fetch_one(fx.db.pool())
    .await
    .unwrap();
    assert_eq!(
        repo.save_builder_draft(
            Id::from(plain_session),
            fx.workspace_id,
            fx.user_id,
            &json!({}),
        )
        .await
        .unwrap(),
        SaveDraftOutcome::NotBuilderCarrier
    );
    assert_eq!(
        repo.save_builder_draft(
            Id::from(Uuid::now_v7()),
            fx.workspace_id,
            fx.user_id,
            &json!({})
        )
        .await
        .unwrap(),
        SaveDraftOutcome::SessionNotFound
    );
    assert_eq!(
        repo.switch_builder_runtime(
            Id::from(plain_session),
            fx.workspace_id,
            fx.user_id,
            fx.runtime_id,
            "local",
        )
        .await
        .unwrap(),
        SwitchRuntimeOutcome::NotBuilderCarrier
    );

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 9. quick-create 人工重试
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct DailyUsageRow {
    client_version: String,
    probe_result: Option<String>,
    #[allow(dead_code)]
    provider_summary: Option<serde_json::Value>,
    runtime_count: Option<i32>,
    #[allow(dead_code)]
    online_count: Option<i32>,
    #[allow(dead_code)]
    offline_count: Option<i32>,
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn client_usage_upsert_is_idempotent_and_probe_scoped() {
    let Some(fx) = setup().await else {
        println!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let repo = repo(&fx);
    let install_id = Uuid::new_v4();
    let base = ClientUsageUpsert {
        user_id: fx.user_id,
        client_type: "desktop".to_owned(),
        install_id,
        workspace_id: Some(fx.workspace_id),
        client_version: "1.0.0".to_owned(),
        os: "linux".to_owned(),
        runtime_probed_at: None,
        probe_result: None,
        runtime_count: None,
        provider_summary: None,
        online_count: None,
        offline_count: None,
    };
    repo.upsert_client_usage(&base).await.unwrap();
    // 第二次带探针：探针列被写，其余照旧。
    repo.upsert_client_usage(&ClientUsageUpsert {
        client_version: "1.0.1".to_owned(),
        runtime_probed_at: Some(chrono::Utc::now()),
        probe_result: Some("success".to_owned()),
        runtime_count: Some(2),
        provider_summary: Some(json!({"claude": 1})),
        online_count: Some(1),
        offline_count: Some(1),
        ..base.clone()
    })
    .await
    .unwrap();
    // 第三次不带探针：探针列必须保留，版本号要更新。
    repo.upsert_client_usage(&ClientUsageUpsert {
        client_version: "1.0.2".to_owned(),
        ..base.clone()
    })
    .await
    .unwrap();

    let rows: Vec<DailyUsageRow> = sqlx::query_as(
        "SELECT client_version, probe_result, provider_summary, runtime_count, online_count, \
             offline_count FROM client_usage_daily \
         WHERE user_id = $1 AND client_type = 'desktop' AND install_id = $2",
    )
    .bind(fx.user_id.0)
    .bind(install_id)
    .fetch_all(fx.db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "(user, client, install, 当天) 是主键");
    assert_eq!(rows[0].client_version, "1.0.2");
    assert_eq!(
        rows[0].probe_result.as_deref(),
        Some("success"),
        "无探针写入保留旧探针结果"
    );
    assert_eq!(rows[0].runtime_count, Some(2));

    teardown(&fx).await;
}

// ---------------------------------------------------------------------------
// 13. TaskStore 端口适配
// ---------------------------------------------------------------------------
