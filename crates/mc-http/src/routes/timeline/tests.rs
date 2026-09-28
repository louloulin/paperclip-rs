//! M9-8（`LUM-1823`）的**证据面**：`GET /api/issues/:id/timeline` 的两条形态、两个
//! 上限、授权链。
//!
//! ## 为什么是子模块而不是就地 `#[cfg(test)] mod tests`
//!
//! 门 ⑩ 的**硬上限是单文件 800 行**（`scripts/file_size_check.py`），而本片的实现
//! （`super`）已经占掉 555 行，`docs/62` §6.5 的 M9-8 行又要求**真库造行**断言
//! 合并 / keyset / 截断三个语义 + 404/401/403。两者相加**必然**越过 800。
//!
//! 拆分的先例（本仓既有，不是本片发明的）：**D10**（`docs/32` §30，
//! `routes/cloud/subscriptions/tests/{support,db}.rs`）、`routes/onboarding/tests/`、
//! `routes/uploads/tests/`、`routes/cloud_runtime/tests/`。**写集偏离已登记**在
//! `docs/32` §9.13（M9-8 那一段）。
//!
//! ## 两半的判据**依据不同**（与 `cloud_runtime` / `onboarding` 同手法）
//!
//! - **`mod pure`**（门 ⑤，不碰库）：四参的「任一非空」判定、两种形态排序相反、
//!   同刻靠 `id` 决胜、**两侧独立截断不 clamp**、截断头取值逐格、序列化形状；
//! - **`mod db_tests`**（门 ⑥，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）：真库造
//!   `comment` + `activity_log` 两类行 ⇒ 端到端断言合并 / 形态 / 截断 / 授权链。
//!
//! 造行**只**碰两条查询真实读取的表 ⇒ **不依赖**别的波次去补 `activity_log` 的写者
//! （R-M9-4：本地只有 `agent/env.rs:80` 一个写者，上游也只有 3 处 `CreateActivity`）。

use super::*;

#[cfg(test)]
mod pure {
    use super::*;

    fn q(keys: &[(&str, &str)]) -> HashMap<String, String> {
        keys.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn entry(id: &str, created_at: &str) -> TimelineEntry {
        TimelineEntry {
            entry_type: "comment".into(),
            id: id.into(),
            actor_type: "user".into(),
            actor_id: Uuid::nil().to_string(),
            created_at: created_at.into(),
            actor_name: String::new(),
            actor_avatar_url: String::new(),
            action: None,
            details: None,
            content: None,
            parent_id: None,
            updated_at: None,
            revision: 0,
            comment_type: None,
            quick_action_id: None,
            reactions: Vec::new(),
            attachments: Vec::new(),
            resolved_at: None,
            resolved_by_type: None,
            resolved_by_id: None,
            source_task_id: None,
            deleted_at: None,
        }
    }

    fn ids(entries: &[TimelineEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.id.as_str()).collect()
    }

    /// 四参**任一非空** ⇒ wrapped；**全缺或全空** ⇒ 裸数组（上游逐字 `!= ""`）。
    #[test]
    fn any_non_empty_keyset_param_selects_the_wrapped_shape() {
        for keys in [
            &[("limit", "20")][..],
            &[("before", "2026-01-01T00:00:00Z")][..],
            &[("after", "2026-01-01T00:00:00Z")][..],
            &[("around", "anchor-1")][..],
            &[
                ("limit", "20"),
                ("before", "b"),
                ("after", "a"),
                ("around", "x"),
            ][..],
        ] {
            assert!(
                TimelineQuery::from(&q(keys)).want_wrapped,
                "{keys:?} ⇒ wrapped"
            );
        }
        for keys in [
            &[("limit", "")][..],
            &[("before", "")][..],
            &[("after", "")][..],
            &[("around", "")][..],
            &[("limit", ""), ("before", ""), ("after", ""), ("around", "")][..],
            &[][..],
        ] {
            assert!(
                !TimelineQuery::from(&q(keys)).want_wrapped,
                "{keys:?} ⇒ 裸数组"
            );
        }
    }

    /// 🔴 四参**只**选形态、不裁窗口、不校验（上游已删掉时间游标分页，`#2128`→`#1929`）。
    #[test]
    fn the_keyset_params_never_validate_and_around_is_the_only_extra_effect() {
        for raw in ["0", "-1", "abc", "1.5", "9999999999999999999999"] {
            let params = TimelineQuery::from(&q(&[("limit", raw)]));
            assert!(params.want_wrapped, "{raw} 仍选 wrapped（不 400）");
            assert!(params.around.is_none());
        }
        assert_eq!(
            TimelineQuery::from(&q(&[("around", "anchor-1")]))
                .around
                .as_deref(),
            Some("anchor-1")
        );
    }

    /// 两种形态**排序相反**、装**同一批** entry；同一时刻**靠 `id` 决胜**。
    #[test]
    fn the_two_shapes_are_the_same_entries_in_opposite_orders() {
        let at = "2026-01-01T00:00:00Z";
        let comments = || vec![entry("c1", at), entry("c3", "2026-01-03T00:00:00Z")];
        let activities = || vec![entry("a2", "2026-01-02T00:00:00Z")];
        let asc = merge_entries(comments(), activities(), true);
        assert_eq!(
            ids(&asc),
            vec!["c1", "a2", "c3"],
            "裸数组 = ASC（最老在前）"
        );
        let desc = merge_entries(comments(), activities(), false);
        assert_eq!(
            ids(&desc),
            vec!["c3", "a2", "c1"],
            "wrapped = DESC（最新在前）"
        );
        let same = merge_entries(
            vec![entry("bbb", at), entry("aaa", at)],
            vec![entry("ccc", at), entry("aaa", at)],
            true,
        );
        assert_eq!(
            ids(&same),
            vec!["aaa", "aaa", "bbb", "ccc"],
            "同刻靠 id 决胜"
        );
    }

    /// 🔴 **两侧独立截断、绝不 clamp 到同一个 floor**（上游注释逐字）。
    ///
    /// 评论没触顶（3 行全留）而活动触顶（4 行只留 2 行）⇒ 合并后**超过**单侧上限。
    /// 共享 floor 会把已经取回、本可正常渲染的**评论**砍掉 —— 那是纯亏损。
    #[test]
    fn the_two_halves_are_capped_independently_and_never_clamped() {
        let (comments, comments_truncated) = take_newest(vec![1, 2, 3], 3);
        let (activities, activities_truncated) = take_newest(vec![1, 2, 3, 4], 2);
        assert_eq!(comments, vec![1, 2, 3], "未触顶 ⇒ 原样");
        assert!(!comments_truncated);
        assert_eq!(activities, vec![3, 4], "触顶 ⇒ 砍掉**最老**的");
        assert!(activities_truncated);
        let total = comments.len() + activities.len();
        assert_eq!(total, 5);
        assert!(total > 2, "合并后可以超过单侧上限（不 clamp）");
        assert_eq!(
            truncated_kinds(comments_truncated, activities_truncated),
            "activity"
        );
    }

    /// 截断头的取值逐格；探针行证明「还有更老的行」（恰好 N 行**不算**截断）。
    #[test]
    fn the_truncation_header_names_exactly_which_halves_were_cut() {
        assert_eq!(truncated_kinds(false, false), "", "都没响 ⇒ 不发那个头");
        assert_eq!(truncated_kinds(false, true), "activity");
        assert_eq!(truncated_kinds(true, false), "comment");
        assert_eq!(truncated_kinds(true, true), "activity,comment");
        assert_eq!(take_newest(vec![1, 2, 3], 3), (vec![1, 2, 3], false));
        assert_eq!(take_newest(vec![1, 2, 3, 4], 3), (vec![2, 3, 4], true));
    }

    /// 只有 `member` / `user` 两种「人」的写法会拿到展示身份（本地词表是 `user`）。
    #[test]
    fn only_human_actor_vocabularies_get_hydrated() {
        for actor in ["member", "user"] {
            assert!(is_human_actor(actor), "{actor}");
        }
        for actor in ["agent", "system", "plugin", "squad", "autopilot", ""] {
            assert!(!is_human_actor(actor), "{actor}");
        }
    }

    /// 序列化形状：可空列**不写键**（全仓惯例），`type` / `id` / 归属三件必写。
    #[test]
    fn the_entry_omits_absent_optional_fields() {
        let mut e = entry("c1", "2026-01-01T00:00:00Z");
        e.entry_type = "activity".into();
        e.action = Some("issue_created".into());
        let v = serde_json::to_value(&e).expect("serialize");
        assert_eq!(v["type"], "activity");
        assert_eq!(v["id"], "c1");
        assert!(
            v["actor_type"].is_string() && v["actor_id"].is_string() && v["created_at"].is_string()
        );
        for absent in [
            "actor_name",
            "content",
            "parent_id",
            "updated_at",
            "revision",
            "comment_type",
            "reactions",
            "attachments",
        ] {
            assert!(v.get(absent).is_none(), "{absent} 不得出现");
        }
        assert_eq!(v["action"], "issue_created");
    }
}

/// 真库用例（门 ⑥，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::routes::dashboard::failures::test_support::{pool, seed, Seed};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Duration;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    /// 只挂本片那一条键的 app（上限可缩 ⇒ 「上限真的响了」这条判据可覆盖）。
    fn app_at(db: mc_db::Db, hard_cap: usize) -> Router {
        use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
        let realtime = mc_realtime::RealtimeHandle::start(8);
        let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "timeline-test"));
        let state = Arc::new(AppState::new(
            db,
            RuntimeHandles {
                actors: mc_core::actor::ActorRegistry::new(),
                adapters: Arc::new(AdapterRegistry::default()),
            },
            ConfigSnapshot::default(),
            realtime,
            ws,
        ));
        router_with_limits(TimelineLimits { hard_cap }).with_state(state)
    }

    /// 发一个 GET，回 `(状态码, 截断头, JSON)`。`user = None` ⇒ 无会话。
    async fn call(
        app: &Router,
        path: &str,
        user: Option<Uuid>,
        workspace: Uuid,
    ) -> (StatusCode, Option<String>, Value) {
        let mut request = Request::builder()
            .uri(path)
            .header("x-workspace-id", workspace.to_string());
        if let Some(user) = user {
            request = request.header("x-multica-user-id", user.to_string());
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("route must be mounted");
        let status = response.status();
        let truncated = response
            .headers()
            .get(HEADER_TIMELINE_TRUNCATED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            truncated,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn new_issue(db: &mc_db::Db, workspace: Uuid, creator: Uuid, tag: &str) -> Uuid {
        let number = i32::try_from(Uuid::new_v4().as_u128() % 900_000).unwrap_or(1);
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO issue (workspace_id, number, identifier, title, creator_type, creator_id) \
             VALUES ($1, $2, $3, 'm98 fixture', 'user', $4) RETURNING id",
        )
        .bind(workspace)
        .bind(number)
        .bind(format!("M98-{tag}-{number}"))
        .bind(creator)
        .fetch_one(db.pool())
        .await
        .expect("insert issue")
    }

    /// 本 issue + 一个**别家** workspace（调用方在那边**也是** member）⇒ 用来钉 404。
    async fn seed_issues(db: &mc_db::Db, seed: &Seed) -> (Uuid, Uuid) {
        let slug = format!("m98-other-{}", Uuid::new_v4().simple());
        let other_ws: Uuid =
            sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
                .bind(&slug)
                .bind(&slug)
                .fetch_one(db.pool())
                .await
                .expect("insert workspace");
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
            .bind(other_ws)
            .bind(seed.member)
            .execute(db.pool())
            .await
            .expect("insert member");
        (
            new_issue(db, seed.workspace, seed.member, "a").await,
            new_issue(db, other_ws, seed.member, "b").await,
        )
    }

    async fn comment(db: &mc_db::Db, issue: Uuid, ws: Uuid, author: Uuid, minute: i64) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO comment (issue_id, workspace_id, author_type, author_id, content, type, \
                                  created_at, updated_at) \
             VALUES ($1, $2, 'user', $3, $4, 'comment', $5, $5) RETURNING id",
        )
        .bind(issue)
        .bind(ws)
        .bind(author)
        .bind(format!("comment {minute}"))
        .bind(Utc::now() + Duration::minutes(minute))
        .fetch_one(db.pool())
        .await
        .expect("insert comment")
    }

    async fn activity(db: &mc_db::Db, issue: Uuid, ws: Uuid, actor: Uuid, minute: i64) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO activity_log (workspace_id, issue_id, actor_type, actor_id, action, \
                                       details, created_at) \
             VALUES ($1, $2, 'member', $3, $4, $5, $6) RETURNING id",
        )
        .bind(ws)
        .bind(issue)
        .bind(actor)
        .bind(format!("action_{minute}"))
        .bind(serde_json::json!({ "n": minute }))
        .bind(Utc::now() + Duration::minutes(minute))
        .fetch_one(db.pool())
        .await
        .expect("insert activity")
    }

    /// 裸形态：两类行**合并**、按 `(created_at, id)` 升序、逐类字段、member 水合；
    /// 以及 🔴 非本 workspace 的 issue ⇒ **404**（不是 403、也不是空列表）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn the_flat_shape_merges_both_halves_ascending_and_hydrates_member_actors() {
        let Some(db) = pool().await else { return };
        let seed = seed(&db).await;
        let (issue, other) = seed_issues(&db, &seed).await;
        // 给作者一个头像，才能证明 `actor_avatar_url` 也被水合（而不是恒缺席）。
        sqlx::query(r#"UPDATE "user" SET avatar_url = 'avatars/m98.png' WHERE id = $1"#)
            .bind(seed.member)
            .execute(db.pool())
            .await
            .expect("set avatar");
        // 交错的 4 行：评论 0 / 活动 1 / 评论 2 / 活动 3。
        let c0 = comment(&db, issue, seed.workspace, seed.member, 0).await;
        let a1 = activity(&db, issue, seed.workspace, seed.member, 1).await;
        let c2 = comment(&db, issue, seed.workspace, seed.member, 2).await;
        let a3 = activity(&db, issue, seed.workspace, seed.member, 3).await;
        let app = app_at(db.clone(), 2000);
        let path = format!("/api/issues/{issue}/timeline");

        let (status, truncated, body) = call(&app, &path, Some(seed.member), seed.workspace).await;
        assert_eq!(status, 200);
        assert_eq!(truncated, None, "没顶到上限 ⇒ 不发那个头");
        let rows = body.as_array().expect("裸 JSON 数组");
        let got: Vec<String> = rows
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            got,
            vec![
                c0.to_string(),
                a1.to_string(),
                c2.to_string(),
                a3.to_string()
            ]
        );
        // 逐类字段：评论只带 comment 那几格、活动只带 activity 那几格。
        assert_eq!(rows[0]["type"], "comment");
        assert_eq!(rows[0]["content"], "comment 0");
        assert_eq!(rows[0]["comment_type"], "comment");
        assert!(rows[0].get("action").is_none(), "评论不得带 activity 字段");
        assert_eq!(rows[1]["type"], "activity");
        assert_eq!(rows[1]["action"], "action_1");
        assert_eq!(rows[1]["details"]["n"], 1);
        assert!(rows[1].get("content").is_none(), "活动不得带 comment 字段");
        // member actor 水合：`user` 词表的评论与 `member` 词表的活动**都**水合。
        for row in rows {
            assert!(row["actor_name"].is_string(), "两类都得有 actor_name");
            assert_eq!(row["actor_avatar_url"], "avatars/m98.png", "头像也水合");
        }
        // 反向：无头像的作者 ⇒ 键**不出现**（上游 `omitempty` / 全仓惯例），不是 `null`。
        sqlx::query(r#"UPDATE "user" SET avatar_url = NULL WHERE id = $1"#)
            .bind(seed.member)
            .execute(db.pool())
            .await
            .expect("clear avatar");
        let (_, _, body) = call(&app, &path, Some(seed.member), seed.workspace).await;
        let rows = body.as_array().expect("裸 JSON 数组");
        assert!(rows[0]["actor_name"].is_string());
        assert!(
            rows[0].get("actor_avatar_url").is_none(),
            "无头像 ⇒ 键不出现"
        );
        let (status, _, body) = call(
            &app,
            &format!("/api/issues/{other}/timeline"),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert_eq!(status, 404, "跨 workspace ⇒ 404");
        assert_eq!(body["error"]["code"], "not_found");
    }

    /// keyset 四参：任一非空 ⇒ wrapped（DESC + 游标恒 null + `has_more_after` 恒 false）；
    /// `around` 定位锚点；空值**不**切形态；非法值**不** 400。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn the_wrapped_shape_is_desc_with_null_cursors_and_around_locates_the_anchor() {
        let Some(db) = pool().await else { return };
        let seed = seed(&db).await;
        let (issue, _) = seed_issues(&db, &seed).await;
        let c0 = comment(&db, issue, seed.workspace, seed.member, 0).await;
        let _c1 = comment(&db, issue, seed.workspace, seed.member, 1).await;
        let a2 = activity(&db, issue, seed.workspace, seed.member, 2).await;
        let app = app_at(db.clone(), 2000);
        let path = format!("/api/issues/{issue}/timeline");

        let (status, _, body) = call(
            &app,
            &format!("{path}?limit=20"),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let entries = body["entries"].as_array().expect("wrapped");
        let got: Vec<String> = entries
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], a2.to_string(), "wrapped = DESC（最新在前）");
        assert_eq!(body["next_cursor"], Value::Null);
        assert_eq!(body["prev_cursor"], Value::Null);
        assert_eq!(body["has_more_after"], false);
        assert_eq!(body["has_more_before"], false, "没顶到上限");
        assert!(body.get("target_index").is_none(), "没给 around ⇒ 键不出现");

        let (_, _, body) = call(
            &app,
            &format!("{path}?around={c0}"),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert_eq!(body["target_index"], 2, "最老那条在 DESC 切片里是下标 2");
        let (_, _, body) = call(
            &app,
            &format!("{path}?around=nope"),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert!(body.get("target_index").is_none(), "未知锚点 ⇒ 键不出现");
        let (status, _, body) = call(
            &app,
            &format!("{path}?limit="),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            body.is_array(),
            "空值不得切 wrapped（上游 `q.Get(x) != \"\"`）"
        );
        let (status, _, body) = call(
            &app,
            &format!("{path}?limit=abc"),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200, "非法值也不 400（游标已是空操作）");
        assert!(body["entries"].is_array());
    }

    /// 🔴 两侧**独立**截断：1 条评论（未触顶）+ 3 条活动（触顶，上限 1）⇒ 头点名
    /// `"activity"`，而合并后**有 2 条**（> 上限）—— 共享 floor 会把评论砍掉。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn the_two_halves_truncate_independently_and_the_header_names_which() {
        let Some(db) = pool().await else { return };
        let seed = seed(&db).await;
        let (issue, _) = seed_issues(&db, &seed).await;
        let c0 = comment(&db, issue, seed.workspace, seed.member, 0).await;
        let _a1 = activity(&db, issue, seed.workspace, seed.member, 1).await;
        let _a2 = activity(&db, issue, seed.workspace, seed.member, 2).await;
        let a3 = activity(&db, issue, seed.workspace, seed.member, 3).await;
        let app = app_at(db.clone(), 1);
        let path = format!("/api/issues/{issue}/timeline");

        let (status, truncated, body) = call(&app, &path, Some(seed.member), seed.workspace).await;
        assert_eq!(status, 200);
        assert_eq!(truncated.as_deref(), Some("activity"), "头点名被砍的那几类");
        let rows = body.as_array().expect("裸 JSON 数组");
        let got: Vec<String> = rows
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            got,
            vec![c0.to_string(), a3.to_string()],
            "合并后 2 条 > 上限 1"
        );
        assert_eq!(rows[0]["content"], "comment 0", "评论一条都没丢");
        assert_eq!(rows[1]["action"], "action_3", "活动只留最新的那条");
        // wrapped 形态带同一个头，且 `has_more_before` **诚实**地报告 clamp。
        let (_, truncated, body) = call(
            &app,
            &format!("{path}?limit=5"),
            Some(seed.member),
            seed.workspace,
        )
        .await;
        assert_eq!(truncated.as_deref(), Some("activity"));
        assert_eq!(body["has_more_before"], true);
        assert_eq!(body["has_more_after"], false);
    }

    /// 授权链三级阶梯：401（无会话）→ 400（workspace 缺）→ 403（非 member）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn the_auth_chain_is_401_then_400_then_403() {
        let Some(db) = pool().await else { return };
        let seed = seed(&db).await;
        let (issue, _) = seed_issues(&db, &seed).await;
        let app = app_at(db, 2000);
        let path = format!("/api/issues/{issue}/timeline");

        let (status, _, body) = call(&app, &path, None, seed.workspace).await;
        assert_eq!(status, 401, "无会话 ⇒ 401（先于任何表访问）");
        assert_eq!(body["error"]["code"], "unauthorized");

        let request = Request::builder()
            .uri(&path)
            .header("x-multica-user-id", seed.member.to_string())
            .body(Body::empty())
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("mounted");
        assert_eq!(response.status(), 400, "workspace 四个来源都缺 ⇒ 400");

        let (status, _, body) = call(&app, &path, Some(seed.outsider), seed.workspace).await;
        assert_eq!(status, 403, "非 member ⇒ 403（**不是** 404）");
        assert_eq!(body["error"]["code"], "forbidden");
    }
}
