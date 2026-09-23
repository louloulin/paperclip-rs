//! `crate::project` 的测试：离线单测 + 真库 PG 集成测试。
//!
//! 拆出本文件的原因同 `search.rs`（门 ⑩ 800 行/文件的硬上限）。

use super::search::{build_project_search_query, extract_snippet, split_search_terms, SearchArg};
use super::*;
use crate::RepoError;



    #[test]
    fn escape_like_covers_backslash_percent_underscore() {
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }

    #[test]
    fn split_search_terms_drops_empty_and_splits_unicode_space() {
        assert_eq!(split_search_terms("  a\u{3000}b "), vec!["a", "b"]);
        assert!(split_search_terms("   ").is_empty());
    }

    #[test]
    fn search_sql_numbers_placeholders_and_ranks() {
        let (sql, args) = build_project_search_query("Road Map", &split_search_terms("Road Map"), false);
        // $1 短语 + $2 workspace + $3/$4 两个词 + limit + offset
        assert_eq!(args.len(), 6);
        assert!(matches!(args[0], SearchArg::Text(_)));
        assert!(matches!(args[1], SearchArg::Uuid(_)));
        assert!(sql.contains("ELSE 5 END"));
        assert!(sql.contains("p.status NOT IN ('completed', 'cancelled')"));
        assert!(sql.contains("LIMIT $5 OFFSET $6"));
    }

    #[test]
    fn search_sql_single_term_has_no_multiword_tiers() {
        let (sql, args) = build_project_search_query("road", &split_search_terms("road"), true);
        assert_eq!(args.len(), 4, "$1 短语 + $2 workspace + limit + offset");
        assert!(!sql.contains("THEN 3"), "单词查询没有 tier 3");
    }

    #[test]
    fn extract_snippet_centers_on_match_and_marks_truncation() {
        let text = "x".repeat(60) + "needle" + &"y".repeat(200);
        let snippet = extract_snippet(&text, "needle");
        assert!(snippet.starts_with("..."));
        assert!(snippet.ends_with("..."));
        assert!(snippet.contains("needle"));
    }

    #[test]
    fn extract_snippet_is_cjk_safe() {
        let text = "中文内容".repeat(10) + "关键字" + &"尾部".repeat(60);
        let snippet = extract_snippet(&text, "关键字");
        assert!(snippet.contains("关键字"));
        assert!(snippet.chars().count() <= 126, "窗口 = idx-40..idx+len+80 + '...'");
    }

    #[test]
    fn extract_snippet_falls_back_to_first_term() {
        let text = format!("{}beta {}", "a".repeat(30), "c".repeat(200));
        let snippet = extract_snippet(&text, "beta gamma");
        assert!(snippet.contains("beta"));
    }

    #[test]
    fn write_error_classifies_sqlstates() {
        assert!(matches!(
            map_write_err(sqlx::Error::RowNotFound),
            WriteError::Repo(RepoError::NotFound)
        ));
    }

// ---------------------------------------------------------------------------
// 真库集成测试（`MULTICA_TEST_DATABASE_URL`；`--ignored` 才跑）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;
    use uuid::Uuid;

    struct Fx {
        db: Db,
        ws: Id,
        user_id: Uuid,
    }

    async fn setup() -> Option<Fx> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let ws: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m4-1-proj', $1) RETURNING id",
        )
        .bind(format!("itest-m4-1-p-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m4-1-proj', $1) RETURNING id"#,
        )
        .bind(format!("itest-m4-1-p-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some(Fx {
            db,
            ws: Id::from(ws),
            user_id,
        })
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(fx) => fx,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    async fn teardown(fx: &Fx) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(fx.ws.0)
            .execute(fx.db.pool())
            .await;
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(fx.user_id)
            .execute(fx.db.pool())
            .await;
    }

    fn new_project(ws: Id, title: &str) -> NewProject {
        NewProject {
            workspace_id: ws,
            title: title.to_string(),
            description: None,
            icon: None,
            status: "planned".to_string(),
            priority: "none".to_string(),
            lead_type: None,
            lead_id: None,
            start_date: None,
            due_date: None,
        }
    }

    async fn seed_issue(fx: &Fx, project_id: Uuid, title: &str, status: &str, number: i32) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO issue (workspace_id, title, status, creator_type, creator_id, \
                 project_id, number) \
             VALUES ($1, $2, $3, 'member', $4::uuid, $5, $6) RETURNING id",
        )
        .bind(fx.ws.0)
        .bind(title)
        .bind(status)
        .bind(fx.user_id)
        .bind(project_id)
        .bind(number)
        .fetch_one(fx.db.pool())
        .await
        .expect("seed issue")
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_project_crud_update_semantics_and_stats() {
        let fx = fixture!();
        let repo = ProjectRepo::new(fx.db.clone());

        let mut new = new_project(fx.ws, "Roadmap Q3");
        new.description = Some("desc".into());
        new.priority = "high".into();
        new.status = "in_progress".into();
        let created = repo.create(&new).await.expect("create");
        assert!(created.start_date.is_none() && created.due_date.is_none());

        // CHECK 违反（status 不在枚举内）→ CheckViolation（400），不是 500。
        let mut bad = new_project(fx.ws, "bad status");
        bad.status = "nope".into();
        assert!(matches!(
            repo.create(&bad).await,
            Err(WriteError::CheckViolation)
        ));

        // list：status / priority 过滤。
        assert_eq!(
            repo.list(fx.ws, Some("in_progress"), None)
                .await
                .expect("list by status")
                .len(),
            1
        );
        assert!(repo
            .list(fx.ws, None, Some("urgent"))
            .await
            .expect("list by priority")
            .is_empty());

        // update：title/status/priority 的 None = 保持原值；其余 None = 写 NULL。
        let updated = repo
            .update(&ProjectUpdate {
                id: created.id,
                workspace_id: fx.ws,
                title: None,
                description: None,
                icon: None,
                status: Some("completed".into()),
                priority: None,
                lead_type: Some("member".into()),
                lead_id: None,
                start_date: Some(
                    chrono::NaiveDate::from_ymd_opt(2026, 9, 23).expect("valid date"),
                ),
                due_date: None,
            })
            .await
            .expect("update");
        assert_eq!(updated.title, "Roadmap Q3", "COALESCE 保持 title");
        assert_eq!(updated.status, "completed");
        assert_eq!(updated.priority, "high", "COALESCE 保持 priority");
        assert!(updated.description.is_none(), "直接赋值 ⇒ NULL");
        assert_eq!(updated.lead_type.as_deref(), Some("member"));

        // 租户守卫：另一个 workspace 的 id 打不到行 → NotFound。
        assert!(matches!(
            repo.update(&ProjectUpdate {
                id: updated.id,
                workspace_id: Id(Uuid::new_v4()),
                title: Some("nope".into()),
                description: None,
                icon: None,
                status: None,
                priority: None,
                lead_type: None,
                lead_id: None,
                start_date: None,
                due_date: None,
            })
            .await,
            Err(WriteError::Repo(RepoError::NotFound))
        ));

        // issue_stats：1 总 / 1 终态（`terminal_status_keys` = done + cancelled + 自定义）。
        seed_issue(&fx, created.id, "i1", "done", 1).await;
        seed_issue(&fx, created.id, "i2", "todo", 2).await;
        let keys = crate::issue::IssueRepo::new(fx.db.clone())
            .terminal_status_keys(fx.ws)
            .await
            .expect("terminal keys");
        let stats = repo
            .issue_stats(fx.ws, &[created.id], &keys)
            .await
            .expect("stats");
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].total_count, 2);
        assert_eq!(stats[0].done_count, 1);
        assert!(repo
            .issue_stats(fx.ws, &[], &keys)
            .await
            .expect("empty stats")
            .is_empty());

        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_search_tiers_and_include_closed() {
        let fx = fixture!();
        let repo = ProjectRepo::new(fx.db.clone());

        // A：标题等于短语（rank 0）；B：标题包含短语 + 已完成；C：只有 description 命中。
        let a = repo
            .create(&new_project(fx.ws, "Road Map"))
            .await
            .expect("create a");
        let mut b_new = new_project(fx.ws, "Road Map Q3 2026");
        b_new.status = "completed".into();
        repo.create(&b_new).await.expect("create b");
        let mut c_new = new_project(fx.ws, "Q3 planning");
        c_new.description = Some("the road map for Q3 lives here".into());
        let c = repo.create(&c_new).await.expect("create c");

        // include_closed=false：completed 的 B 被过滤掉。
        let hits = repo
            .search(fx.ws, "road map", 20, 0, false)
            .await
            .expect("search open");
        let titles: Vec<&str> = hits.iter().map(|h| h.project.title.as_str()).collect();
        assert_eq!(titles, vec!["Road Map", "Q3 planning"], "标题命中先于描述命中");
        assert_eq!(hits[0].match_source, "title");
        assert_eq!(hits[1].match_source, "description");

        // include_closed=true：B 回归（rank 更低，排在描述命中之后）。
        let hits = repo
            .search(fx.ws, "road map", 20, 0, true)
            .await
            .expect("search all");
        let titles: Vec<&str> = hits.iter().map(|h| h.project.title.as_str()).collect();
        assert_eq!(titles, vec!["Road Map", "Road Map Q3 2026", "Q3 planning"]);

        // 多词（非短语）："road" + "lives" 只命中 C 的描述。
        let hits = repo
            .search(fx.ws, "lives road", 20, 0, false)
            .await
            .expect("search terms");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].project.id, c.id, "a = {}", a.id);

        // limit / offset 由调用方原样传入（仓储不 clamp）。
        let hits = repo
            .search(fx.ws, "road map", 1, 1, true)
            .await
            .expect("search paged");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].project.title, "Road Map Q3 2026");

        // 其它 workspace 搜不到。
        assert!(repo
            .search(Id(Uuid::new_v4()), "road map", 20, 0, true)
            .await
            .expect("other ws")
            .is_empty());

        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_delete_cascade_clears_chat_sessions_views_and_pins() {
        let fx = fixture!();
        let repo = ProjectRepo::new(fx.db.clone());
        let pool = fx.db.pool();
        let other = repo
            .create(&new_project(fx.ws, "keep me"))
            .await
            .expect("create other");
        let target = repo
            .create(&new_project(fx.ws, "delete me"))
            .await
            .expect("create target");

        // chat_session 挂在 target 上（agent 只为满足 NOT NULL）。
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent(workspace_id, name, runtime_mode) VALUES ($1, 'itest-m4-1', 'local') \
             RETURNING id",
        )
        .bind(fx.ws.0)
        .fetch_one(pool)
        .await
        .expect("seed agent");
        let session_keep: Uuid = sqlx::query_scalar(
            "INSERT INTO chat_session(workspace_id, agent_id, creator_id, project_id) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(fx.ws.0)
        .bind(agent_id)
        .bind(fx.user_id)
        .bind(target.id)
        .fetch_one(pool)
        .await
        .expect("seed session");

        // issue_view（project 作用域）+ 它的侧栏 pin。
        let view_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue_view(workspace_id, owner_id, name, scope_type, scope_id, query) \
             VALUES ($1, $2, 'itest view', 'project', $3, '{}'::jsonb) RETURNING id",
        )
        .bind(fx.ws.0)
        .bind(fx.user_id)
        .bind(target.id)
        .fetch_one(pool)
        .await
        .expect("seed view");
        sqlx::query(
            "INSERT INTO pinned_item(workspace_id, user_id, item_type, item_id) \
             VALUES ($1, $2, 'view', $3)",
        )
        .bind(fx.ws.0)
        .bind(fx.user_id)
        .bind(view_id)
        .execute(pool)
        .await
        .expect("seed pin");

        assert_eq!(repo.delete_cascade(target.id, fx.ws).await.expect("delete"), 1);

        let project_left: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM project WHERE id = $1")
            .bind(target.id)
            .fetch_one(pool)
            .await
            .expect("count project");
        assert_eq!(project_left, 0);
        let session_project: Option<Uuid> =
            sqlx::query_scalar("SELECT project_id FROM chat_session WHERE id = $1")
                .bind(session_keep)
                .fetch_one(pool)
                .await
                .expect("session survives");
        assert!(session_project.is_none(), "chat_session.project_id 清空但行保留");
        let view_left: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM issue_view WHERE id = $1")
            .bind(view_id)
            .fetch_one(pool)
            .await
            .expect("count view");
        assert_eq!(view_left, 0);
        let pin_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM pinned_item WHERE item_type = 'view' AND item_id = $1",
        )
        .bind(view_id)
        .fetch_one(pool)
        .await
        .expect("count pin");
        assert_eq!(pin_left, 0);

        // 别的 project 不受影响；重复删 → NotFound（上游 404 "project not found"）。
        assert!(repo
            .get_in_workspace(other.id, fx.ws)
            .await
            .expect("other still there")
            .is_some());
        assert!(matches!(
            repo.delete_cascade(target.id, fx.ws).await,
            Err(RepoError::NotFound)
        ));

        teardown(&fx).await;
        fx.db.close().await;
    }
}
