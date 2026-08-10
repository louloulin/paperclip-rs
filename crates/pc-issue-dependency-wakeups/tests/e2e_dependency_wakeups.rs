//! End-to-end tests for `pc-issue-dependency-wakeups`.
//!
//! 包含：
//! - 纯函数 service 测试：idempotency key 构造
//! - Hook 测试：BeforeFind / AfterFindHit / AfterFindMiss 触发
//! - 真实 DB 集成测试：单 key / 多 key 查询

use pc_issue_dependency_wakeups::{
    build_issue_blockers_resolved_wake_idempotency_key, IssueDependencyWakeupHookEvent,
    IssueDependencyWakeupService, IDEMPOTENT_DEPENDENCY_WAKE_STATUSES,
    ISSUE_BLOCKERS_RESOLVED_WAKE_REASON, RecordingIssueDependencyWakeupHook,
};
use std::sync::Arc;
use uuid::Uuid;

// ============================================================================
// 常量测试
// ============================================================================

#[test]
fn r663_constants_match_node() {
    assert_eq!(ISSUE_BLOCKERS_RESOLVED_WAKE_REASON, "issue_blockers_resolved");
    assert_eq!(
        IDEMPOTENT_DEPENDENCY_WAKE_STATUSES,
        &["queued", "deferred_issue_execution", "claimed", "completed"]
    );
}

// ============================================================================
// Idempotency key 构造
// ============================================================================

#[test]
fn r663_build_idempotency_key_format() {
    let dependent = Uuid::new_v4();
    let blocker = Uuid::new_v4();
    let key = build_issue_blockers_resolved_wake_idempotency_key(dependent, blocker);
    assert_eq!(
        key,
        format!("issue_blockers_resolved_wake:{}:{}", dependent, blocker)
    );
}

#[test]
fn r663_build_idempotency_key_with_service() {
    let svc = IssueDependencyWakeupService::new();
    let dependent = Uuid::new_v4();
    let blocker = Uuid::new_v4();
    let key = svc.build_idempotency_key(pc_issue_dependency_wakeups::BuildIdempotencyKeyInput {
        dependent_issue_id: dependent,
        resolved_blocker_issue_id: blocker,
    });
    assert!(key.contains("issue_blockers_resolved_wake:"));
    assert!(key.contains(&dependent.to_string()));
    assert!(key.contains(&blocker.to_string()));
}

// ============================================================================
// Hook 测试
// ============================================================================

#[test]
fn r663_hook_before_and_after_build_key() {
    let hook = Arc::new(RecordingIssueDependencyWakeupHook::new());
    let svc = IssueDependencyWakeupService::with_hook(hook.clone());
    let dependent = Uuid::new_v4();
    let blocker = Uuid::new_v4();
    let key = svc.build_idempotency_key(pc_issue_dependency_wakeups::BuildIdempotencyKeyInput {
        dependent_issue_id: dependent,
        resolved_blocker_issue_id: blocker,
    });
    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        IssueDependencyWakeupHookEvent::BeforeBuildKey { .. }
    ));
    assert!(matches!(
        events[1],
        IssueDependencyWakeupHookEvent::AfterBuildKey { .. }
    ));
    if let IssueDependencyWakeupHookEvent::AfterBuildKey { key: k } = &events[1] {
        assert_eq!(k, &key);
    }
}

#[test]
fn r663_hook_clear() {
    let hook = Arc::new(RecordingIssueDependencyWakeupHook::new());
    let svc = IssueDependencyWakeupService::with_hook(hook.clone());
    let _ = svc.build_idempotency_key(pc_issue_dependency_wakeups::BuildIdempotencyKeyInput {
        dependent_issue_id: Uuid::new_v4(),
        resolved_blocker_issue_id: Uuid::new_v4(),
    });
    assert_eq!(hook.len(), 2);
    hook.clear();
    assert!(hook.is_empty());
}

#[test]
fn r663_default_service_uses_noop_hook() {
    let svc = IssueDependencyWakeupService::new();
    let hook = svc.hook();
    // Just exercise
    let _ = svc.build_idempotency_key(pc_issue_dependency_wakeups::BuildIdempotencyKeyInput {
        dependent_issue_id: Uuid::new_v4(),
        resolved_blocker_issue_id: Uuid::new_v4(),
    });
    hook.before_find(1);
    hook.after_find_miss(1);
}

// ============================================================================
// 真实 DB 集成测试
// ============================================================================

mod db_tests {
    use super::*;
    use pc_repos::{
        company::CompanyRepo, Db,
    };
    use pc_issue_dependency_wakeups::{
        find_existing_wake, find_existing_wake_for_any_key, ExistingIssueBlockersResolvedWake,
    };

    const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

    async fn connect() -> Db {
        Db::connect(DB_URL, 5, 1).await.expect("connect to db")
    }

    async fn make_company(db: &Db, tag: &str) -> Uuid {
        let repo = CompanyRepo::new(db);
        let name = format!("R663 Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_agent(db: &Db, company_id: Uuid) -> Uuid {
        // Use direct SQL since agent creation is complex
        let agent_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
             VALUES ($1, $2, $3, 'worker', 'claude_local', 'active')",
        )
        .bind(agent_id)
        .bind(company_id)
        .bind(format!("Agent {}", Uuid::new_v4()))
        .execute(db.pool())
        .await
        .expect("create agent");
        agent_id
    }

    async fn insert_wakeup_request(
        db: &Db,
        company_id: Uuid,
        agent_id: Uuid,
        idempotency_key: &str,
        status: &str,
    ) -> Uuid {
        let wakeup_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_wakeup_requests \
             (id, company_id, agent_id, source, status, idempotency_key, requested_at) \
             VALUES ($1, $2, $3, 'automation', $4, $5, now())",
        )
        .bind(wakeup_id)
        .bind(company_id)
        .bind(agent_id)
        .bind(status)
        .bind(idempotency_key)
        .execute(db.pool())
        .await
        .expect("insert wakeup");
        wakeup_id
    }

    async fn reset_tables(db: &Db) {
        sqlx::query(
            "DELETE FROM agent_wakeup_requests WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R663 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset wakeups");
        sqlx::query(
            "DELETE FROM agents WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R663 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset agents");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'R663 Co %'")
            .execute(db.pool())
            .await
            .expect("reset companies");
    }

    #[tokio::test]
    async fn r663_db_find_existing_hit_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fh").await;
        let agent_id = make_agent(&db, company_id).await;
        let key = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        let wakeup_id = insert_wakeup_request(&db, company_id, agent_id, &key, "queued").await;

        let result = find_existing_wake(&db, company_id, &key)
            .await
            .expect("query");
        let wake = result.expect("should find wake");
        assert_eq!(wake.id, wakeup_id);
        assert_eq!(wake.status, "queued");
    }

    #[tokio::test]
    async fn r663_db_find_existing_miss_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fm").await;

        let result = find_existing_wake(&db, company_id, "nonexistent:key")
            .await
            .expect("query");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn r663_db_find_existing_filters_by_status_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fs").await;
        let agent_id = make_agent(&db, company_id).await;
        let key = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        // Insert with status "cancelled" (not in IDEMPOTENT list)
        insert_wakeup_request(&db, company_id, agent_id, &key, "cancelled").await;

        let result = find_existing_wake(&db, company_id, &key)
            .await
            .expect("query");
        assert!(result.is_none(), "cancelled status should be filtered out");
    }

    #[tokio::test]
    async fn r663_db_find_existing_all_idempotent_statuses_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fi").await;
        let agent_id = make_agent(&db, company_id).await;

        for status in IDEMPOTENT_DEPENDENCY_WAKE_STATUSES {
            let key = build_issue_blockers_resolved_wake_idempotency_key(
                Uuid::new_v4(),
                Uuid::new_v4(),
            );
            let wakeup_id = insert_wakeup_request(&db, company_id, agent_id, &key, status).await;
            let result = find_existing_wake(&db, company_id, &key)
                .await
                .expect("query");
            let wake = result.expect("should find");
            assert_eq!(wake.id, wakeup_id);
            assert_eq!(wake.status, *status);
        }
    }

    #[tokio::test]
    async fn r663_db_find_for_any_key_hit_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fa").await;
        let agent_id = make_agent(&db, company_id).await;

        let key1 = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        let key2 = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        let key3 = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        let wakeup_id = insert_wakeup_request(&db, company_id, agent_id, &key2, "claimed").await;

        let result = find_existing_wake_for_any_key(&db, company_id, &[key1, key2.clone(), key3])
            .await
            .expect("query");
        let wake = result.expect("should find");
        assert_eq!(wake.id, wakeup_id);
        assert_eq!(wake.idempotency_key, Some(key2));
        assert_eq!(wake.status, "claimed");
    }

    #[tokio::test]
    async fn r663_db_find_for_any_key_miss_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ma").await;

        let result = find_existing_wake_for_any_key(
            &db,
            company_id,
            &[
                "nonexistent:1".to_string(),
                "nonexistent:2".to_string(),
            ],
        )
        .await
        .expect("query");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn r663_db_find_for_any_key_empty_returns_none() {
        let db = connect().await;
        let result =
            find_existing_wake_for_any_key(&db, Uuid::new_v4(), &[]).await.expect("query");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn r663_db_find_for_any_key_dedupes() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fd").await;
        let agent_id = make_agent(&db, company_id).await;

        let key = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        let wakeup_id = insert_wakeup_request(&db, company_id, agent_id, &key, "queued").await;

        // Pass duplicates
        let result = find_existing_wake_for_any_key(&db, company_id, &[key.clone(), key.clone(), key.clone()])
            .await
            .expect("query");
        let wake = result.expect("should find");
        assert_eq!(wake.id, wakeup_id);
    }

    #[tokio::test]
    async fn r663_db_service_find_existing_hit_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "sh").await;
        let agent_id = make_agent(&db, company_id).await;
        let key = build_issue_blockers_resolved_wake_idempotency_key(Uuid::new_v4(), Uuid::new_v4());
        let wakeup_id = insert_wakeup_request(&db, company_id, agent_id, &key, "completed").await;

        let hook = Arc::new(RecordingIssueDependencyWakeupHook::new());
        let svc = IssueDependencyWakeupService::with_hook(hook.clone());
        let result = svc
            .find_existing(
                &db,
                pc_issue_dependency_wakeups::FindExistingWakeInput {
                    company_id,
                    idempotency_key: key,
                },
            )
            .await
            .expect("query");
        let wake = result.expect("should find");
        assert_eq!(wake.id, wakeup_id);

        let events = hook.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            IssueDependencyWakeupHookEvent::BeforeFind { .. }
        ));
        assert!(matches!(
            events[1],
            IssueDependencyWakeupHookEvent::AfterFindHit { .. }
        ));
    }

    #[tokio::test]
    async fn r663_db_service_find_existing_miss_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "sm").await;

        let hook = Arc::new(RecordingIssueDependencyWakeupHook::new());
        let svc = IssueDependencyWakeupService::with_hook(hook.clone());
        let result = svc
            .find_existing(
                &db,
                pc_issue_dependency_wakeups::FindExistingWakeInput {
                    company_id,
                    idempotency_key: "missing:key".to_string(),
                },
            )
            .await
            .expect("query");
        assert!(result.is_none());

        let events = hook.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            IssueDependencyWakeupHookEvent::AfterFindMiss { .. }
        ));
    }
}
