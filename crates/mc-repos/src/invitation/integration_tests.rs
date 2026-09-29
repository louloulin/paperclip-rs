    use chrono::Duration;
use uuid::Uuid;

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;

use crate::invitation::*;
use crate::RepoError;
    use std::env;

    async fn connect() -> Option<(sqlx::PgPool, Id, Id)> {
        let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        // 准备一个 workspace + 一个邀请者 user
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-ws', $1) RETURNING id",
        )
        .bind(format!("itest-{}", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-inviter', $1) RETURNING id"#,
        )
        .bind(format!("itest-{}@example.com", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .ok()?;
        Some((pool, Id(workspace_id), Id(user_id)))
    }

    async fn cleanup_workspace(pool: &sqlx::PgPool, workspace_id: Uuid) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(workspace_id)
            .execute(pool)
            .await;
    }

    async fn cleanup_user(pool: &sqlx::PgPool, user_id: Uuid) {
        let _ = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(user_id)
            .execute(pool)
            .await;
    }

    /// 1. create + `get_by_token` 往返。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn create_then_get_by_token() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "x@example.com".into(),
                role: WorkspaceRole::Member,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        let fetched = repo
            .get_by_token(&row.token)
            .await
            .expect("get_by_token ok")
            .expect("present");
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.email, "x@example.com");
        assert_eq!(fetched.workspace_id, ws.0);

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }

    /// 2. accept → 插入 member row + `accepted_at` 标记 + UNIQUE 冲突幂等。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn accept_creates_member_and_is_idempotent() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        // 收件人 user
        let recipient: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-recipient', $1) RETURNING id"#,
        )
        .bind(format!("recipient-{}@example.com", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .expect("insert recipient");

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "ignored@example.com".into(),
                role: WorkspaceRole::Member,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        // 首次 accept
        let out1 = repo
            .accept(&row.token, Id(recipient))
            .await
            .expect("first accept ok");
        assert!(!out1.already_accepted);
        assert_eq!(out1.member.user_id, Id(recipient));
        assert_eq!(out1.member.workspace_id, ws);

        // 二次 accept 应幂等
        let out2 = repo
            .accept(&row.token, Id(recipient))
            .await
            .expect("second accept ok");
        assert!(out2.already_accepted);

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(recipient)
            .execute(&pool)
            .await
            .ok();
        pool.close().await;
    }

    /// 3. decline 标记 `revoked_at`。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn decline_marks_revoked() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "x@example.com".into(),
                role: WorkspaceRole::Guest,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        repo.decline(&row.token).await.expect("decline ok");
        let after = repo
            .get_by_token(&row.token)
            .await
            .expect("get ok")
            .expect("present");
        assert!(after.revoked_at.is_some());

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }

    /// 4. revoke by admin 标记 `revoked_at`（以及再次 revoke 返回 `NotFound`）。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn revoke_by_admin_marks_revoked() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "x@example.com".into(),
                role: WorkspaceRole::Admin,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        repo.revoke(Id(row.id), inviter).await.expect("revoke ok");
        // 再次 revoke → NotFound
        let err = repo.revoke(Id(row.id), inviter).await.unwrap_err();
        assert!(
            matches!(err, RepoError::NotFound),
            "expected NotFound, got {err:?}"
        );

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }

    /// 5. `count_recent_in_workspace` 速率窗口。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn count_recent_in_workspace_window() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        // 创建 3 条邀请
        for i in 0..3 {
            repo.create(NewInvitation {
                workspace_id: ws,
                email: format!("r{i}@example.com"),
                role: WorkspaceRole::Member,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");
        }

        let since_recent = Utc::now() - Duration::seconds(60);
        let count = repo
            .count_recent_in_workspace(ws, since_recent)
            .await
            .expect("count ok");
        assert!(count >= 3, "expected at least 3, got {count}");

        // 2 小时前的窗口应返回 0
        let since_old = Utc::now() - Duration::hours(2);
        let count_old = repo
            .count_recent_in_workspace(ws, since_old)
            .await
            .expect("count ok");
        assert!(count_old >= 3, "still >=3 since we created them just now");

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }
