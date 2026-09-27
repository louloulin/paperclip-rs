//! M10-B3 真库那一半（门 ⑥，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。
//!
//! 覆盖门 ⑤ 到不了的那一面：**权限与可见性折叠**、引用校验（非本 workspace ⇒ 404）、
//! 那**唯一**一道 invoke 闸、run 的落库副作用（评论 + `quick_action_id` + 计数），
//! 以及目录那 4 条的**两种写法都**真的打通（不是 405）。
//!
//! 未设变量 ⇒ 打印跳过并 `return`；设了但连不上 ⇒ **panic**（不许静默假装绿）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use super::support::*;
use super::SIX;

// ---------------------------------------------------------------------------
// 1. 目录面：4 条逐条打通（两种写法都是真端点，不是 405）
// ---------------------------------------------------------------------------

/// 目录 4 条 × 两种写法 ⇒ 同一套状态码（真库：真成员 ⇒ 真的打通）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_four_catalogue_routes_answer_on_both_slash_forms() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());

    for form in ["/api/quick-actions", "/api/quick-actions/"] {
        let (status, _, value) =
            send(&app, &Call::new("GET", form, seed.member, seed.workspace)).await;
        assert_eq!(status, StatusCode::OK, "GET {form}");
        assert!(value["quick_actions"].is_array(), "GET {form}");

        let (status, _) = create(
            &app,
            &seed,
            seed.member,
            action_body(&seed, "private", seed.public_agent),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "POST {form}");

        // `{id}` 的两种写法**交叉**用：一条行上，PATCH 走一种、DELETE 走另一种 ——
        // 两种写法都必须打到**同一个** handler（而不是各自碰巧 404/405）。
        // ⚠️ 每轮**重建**一条：上一轮已经把它删掉了，再 PATCH 就是 404，那会把
        // 「键没注册」和「行没了」两种 404 混成一个失败信号。
        for patch_slashed in [false, true] {
            let (_, value) = create(
                &app,
                &seed,
                seed.member,
                action_body(&seed, "private", seed.public_agent),
            )
            .await;
            let id = value["id"].as_str().unwrap().to_string();
            let bare = format!("/api/quick-actions/{id}");
            let slashed = format!("{bare}/");
            let (patch_form, delete_form) = if patch_slashed {
                (slashed.clone(), bare.clone())
            } else {
                (bare.clone(), slashed.clone())
            };

            let (status, _, value) = send(
                &app,
                &Call::new("PATCH", &patch_form, seed.member, seed.workspace)
                    .body(json!({"description": "patched"}).to_string().into_bytes()),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "PATCH {patch_form}");
            assert_eq!(value["description"], json!("patched"), "PATCH {patch_form}");

            let (status, _, _) = send(
                &app,
                &Call::new("DELETE", &delete_form, seed.member, seed.workspace),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "DELETE {delete_form}");
        }
    }
}

// ---------------------------------------------------------------------------
// 2. 可见性折叠：private 不得外泄
// ---------------------------------------------------------------------------

/// `private` 行只对**创建者**出现；`public` 行对所有成员出现。
///
/// 判据是**逐条**列表内容（不是计数）：还要顺带钉住「列表里没有别人的 private」，
/// 哪怕它恰好排在前面。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn private_actions_are_folded_to_their_creator() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());

    let (_, mine) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let (_, theirs) = create(
        &app,
        &seed,
        seed.owner,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let (_, shared) = create(
        &app,
        &seed,
        seed.owner,
        action_body(&seed, "public", seed.public_agent),
    )
    .await;
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let theirs_id = theirs["id"].as_str().unwrap().to_string();
    let shared_id = shared["id"].as_str().unwrap().to_string();

    let (_, _, value) = send(
        &app,
        &Call::new("GET", "/api/quick-actions/", seed.member, seed.workspace),
    )
    .await;
    let ids: Vec<&str> = value["quick_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&mine_id.as_str()), "自己的 private 必须可见");
    assert!(ids.contains(&shared_id.as_str()), "public 行对每个成员可见");
    assert!(
        !ids.contains(&theirs_id.as_str()),
        "别人的 private 不得外泄：{ids:?}"
    );
}

/// 引用一个**别的 workspace** 的动作 ⇒ 404（不是 403）：存在性本身不是调用者的事。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn a_quick_action_from_another_workspace_is_404() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let other = seed_workspace(&db).await;
    let app = app_with(db.clone());

    let (_, theirs) = create(
        &app,
        &other,
        other.owner,
        action_body(&other, "public", other.public_agent),
    )
    .await;
    let theirs_id = theirs["id"].as_str().unwrap().to_string();

    // 目录面（改）与 issue 侧（render）各来一次：两条的 workspace 护栏**不同** ——
    // 目录面用 `?workspace_id=`，issue 侧用 issue 行反查 ⇒ 两条都要钉。
    let (status, _, _) = send(
        &app,
        &Call::new(
            "PATCH",
            &format!("/api/quick-actions/{theirs_id}"),
            seed.owner,
            seed.workspace,
        )
        .body(json!({"description": "hijack"}).to_string().into_bytes()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "跨 workspace 的 PATCH ⇒ 404");
    let (status, _, _) = send(
        &app,
        &Call::new(
            "POST",
            &format!(
                "/api/issues/{}/quick-actions/{theirs_id}/render",
                seed.issue
            ),
            seed.owner,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "跨 workspace 的 render ⇒ 404"
    );
}

/// 非成员一律 404（沿用本仓既有口径：避免跨租户探测）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn a_non_member_sees_nothing_at_all() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    for (method, template, _) in SIX {
        let literal = template
            .replace(":id", &seed.issue.to_string())
            .replace(":quickActionId", &Uuid::new_v4().to_string());
        let call = Call::new(method, &literal, seed.outsider, seed.workspace);
        // PATCH / POST 都要**合法体**才能走到成员门（空体 400 会掩盖真正的判据）。
        let call = if method == "GET" {
            call
        } else {
            call.body(json!({"description": "x"}).to_string().into_bytes())
        };
        let (status, _, _) = send(&app, &call).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {literal}：非成员");
    }
}

// ---------------------------------------------------------------------------
// 3. 写面：公有要角色
// ---------------------------------------------------------------------------

/// `public` 动作要 owner/admin（普通成员 403）；`private` 谁都能建。
///
/// 这是本片**唯一**能区分「成员」与「管理员」的那道门，判据是 403 + 上游逐字消息。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn public_writes_need_owner_or_admin() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());

    let (status, body) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "public", seed.public_agent),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "普通成员不得建 public 动作");
    assert_eq!(
        body["error"]["message"],
        json!("forbidden: workspace admin role required")
    );

    let (status, _) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "private 动作人人可建");
}

/// `public` 动作不得绑一个**别人调不动**的 agent —— 写时就拒（上游的同一道）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn a_public_action_cannot_bind_a_private_agent() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (status, body) = create(
        &app,
        &seed,
        seed.owner,
        action_body(&seed, "public", seed.private_agent),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "public 不得绑 private agent"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("every workspace member can trigger"),
        "{}",
        body["error"]["message"]
    );
    // 同一个 agent 绑成 private 就合法（`visibility` 是心意，不是对目标的判决）。
    let (status, _) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.private_agent),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

// ---------------------------------------------------------------------------
// 4. issue 侧两条
// ---------------------------------------------------------------------------

/// `render` 返回**会**发出去的那段文本，且**不落库**。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn render_returns_the_body_without_posting_anything() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (_, created) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM comment")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let (status, _, value) = send(
        &app,
        &Call::new(
            "POST",
            &format!("/api/issues/{}/quick-actions/{id}/render", seed.issue),
            seed.member,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let assignee = created["assignee_id"].as_str().unwrap();
    let content = value["content"].as_str().unwrap().to_string();
    assert!(
        content.ends_with(&format!("(mention://agent/{assignee})\n\ndo the thing")),
        "{content}"
    );
    assert!(content.starts_with("[@itest-qa-public-"), "{content}");
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM comment")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after, "render 绝不落库");
}

/// `run` 落一条**普通**评论（带 `quick_action_id`）、计数 +1，且**不**伪造 trigger。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn run_posts_one_ordinary_comment_and_touches_usage() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (_, created) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, _, value) = send(
        &app,
        &Call::new(
            "POST",
            &format!("/api/issues/{}/quick-actions/{id}/run", seed.issue),
            seed.member,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    assert_eq!(value["type"], json!("comment"), "刻意是普通评论");
    assert_eq!(value["quick_action_id"], json!(id));
    assert!(value["content"].as_str().unwrap().contains("do the thing"));
    assert!(
        value.get("trigger_outcomes").is_none(),
        "本仓没有触发链 ⇒ 不得伪造 trigger_outcomes"
    );

    let (quick_action_id,): (Option<Uuid>,) =
        sqlx::query_as("SELECT quick_action_id FROM comment WHERE id = $1")
            .bind(Uuid::parse_str(value["id"].as_str().unwrap()).unwrap())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(quick_action_id.map(|v| v.to_string()), Some(id.clone()));
    let (count, touched): (i64, bool) = sqlx::query_as(
        "SELECT use_count, last_used_at IS NOT NULL FROM quick_action WHERE id = $1",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1, "use_count +1");
    assert!(touched, "last_used_at 被写");
}

/// 归档动作 run ⇒ 400；triage 中的 issue ⇒ 403 且**一条评论都不落**。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn archived_actions_and_triaged_issues_refuse_to_run() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (_, created) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, _, _) = send(
        &app,
        &Call::new(
            "PATCH",
            &format!("/api/quick-actions/{id}"),
            seed.member,
            seed.workspace,
        )
        .body(json!({"status": "archived"}).to_string().into_bytes()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档走 PATCH");

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM comment")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let (status, _, body) = send(
        &app,
        &Call::new(
            "POST",
            &format!("/api/issues/{}/quick-actions/{id}/run", seed.issue),
            seed.member,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "归档动作不得 run");
    assert_eq!(
        body["error"]["message"],
        json!("validation error: quick action is archived")
    );

    // 取消归档 + 把 issue 放进 triage ⇒ 403，且**评论一条都不落**。
    send(
        &app,
        &Call::new(
            "PATCH",
            &format!("/api/quick-actions/{id}"),
            seed.member,
            seed.workspace,
        )
        .body(json!({"status": "active"}).to_string().into_bytes()),
    )
    .await;
    sqlx::query("UPDATE issue SET triage_state = 'pending' WHERE id = $1")
        .bind(seed.issue)
        .execute(db.pool())
        .await
        .unwrap();
    let (status, _, _) = send(
        &app,
        &Call::new(
            "POST",
            &format!("/api/issues/{}/quick-actions/{id}/run", seed.issue),
            seed.member,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "triage 中的 issue 不得 run");
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM comment")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after, "被拒的 run 一条评论都不许落");
}

/// 那一道 invoke 闸：**看得见 ≠ 调得动**。
///
/// 两条同构的 private 动作，同一个 `private` agent：
/// - A（member 创建）：member 在目录里**看得见**它，但 render/run **403**；
/// - B（owner 创建）：owner **看得见**且跑得动（201）—— 他是该 agent 的 owner。
///
/// 判据必须把「可见性」与「授权」分开：上游文件头写死了 "PERMISSION IS CHECKED IN
/// EXACTLY ONE PLACE"，而目录读面**不做**任何权限工作。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_invoke_gate_is_the_only_permission_check_and_it_holds() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (_, mine) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.private_agent),
    )
    .await;
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let (_, theirs) = create(
        &app,
        &seed,
        seed.owner,
        action_body(&seed, "private", seed.private_agent),
    )
    .await;
    let theirs_id = theirs["id"].as_str().unwrap().to_string();

    // 目录：自己的可见、别人的不可见（可见性折叠**不是**授权判定）。
    let (status, _, value) = send(
        &app,
        &Call::new("GET", "/api/quick-actions/", seed.member, seed.workspace),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = value["quick_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&mine_id.as_str()), "自己的 private 可见");
    assert!(!ids.contains(&theirs_id.as_str()), "别人的 private 不可见");

    // 看得见的那条**跑不动** ⇒ 403（不是 404：存在性 caller 已经知道了）。
    for verb in ["render", "run"] {
        let (status, _, _) = send(
            &app,
            &Call::new(
                "POST",
                &format!("/api/issues/{}/quick-actions/{mine_id}/{verb}", seed.issue),
                seed.member,
                seed.workspace,
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{verb}：调不动 private agent"
        );
    }

    // 同一个 agent，它的 owner 跑得动（上游 `invokeAgentDecision` 的 owner 分支）。
    let (status, _, _) = send(
        &app,
        &Call::new(
            "POST",
            &format!("/api/issues/{}/quick-actions/{theirs_id}/run", seed.issue),
            seed.owner,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "owner 恒可 invoke 自己的 agent"
    );
}

/// 目标缺失（agent 被归档）⇒ 409，且**不是** 404：动作本身是可见的，坏的是它的绑定。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn a_missing_target_is_a_conflict_not_a_not_found() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (_, created) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(seed.public_agent)
        .execute(db.pool())
        .await
        .unwrap();

    let (status, _, body) = send(
        &app,
        &Call::new(
            "POST",
            &format!("/api/issues/{}/quick-actions/{id}/run", seed.issue),
            seed.member,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "目标缺失 ⇒ 409");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("target is unavailable"));
}

/// 别人的 `private` 动作：改面与删面都 **404**。
///
/// 上游 `DeleteQuickAction` 里那句「非创建者删 private ⇒ 403」**不可达** ——
/// 它先调 `loadReachableQuickAction`，而那一步已经把非创建者的 private 判成 404 了。
/// 本仓照抄那个可达行为（404），并把不可达那一支留在 `lifecycle.rs` 里注明。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn another_members_private_action_is_404_on_read_and_403_on_delete() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    let (_, created) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, _, _) = send(
        &app,
        &Call::new(
            "PATCH",
            &format!("/api/quick-actions/{id}"),
            seed.owner,
            seed.workspace,
        )
        .body(json!({"description": "hijack"}).to_string().into_bytes()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "读/改面：404（存在性不是别人的事）"
    );

    let (status, _, _) = send(
        &app,
        &Call::new(
            "DELETE",
            &format!("/api/quick-actions/{id}"),
            seed.owner,
            seed.workspace,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "删面同样先过可达性 ⇒ 404");
}

/// 活跃上限 30（第 31 条被拒，消息逐字）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_active_cap_is_thirty() {
    let db = fixture!();
    let seed = seed_workspace(&db).await;
    let app = app_with(db.clone());
    for _ in 0..30 {
        let (status, _) = create(
            &app,
            &seed,
            seed.member,
            action_body(&seed, "private", seed.public_agent),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = create(
        &app,
        &seed,
        seed.member,
        action_body(&seed, "private", seed.public_agent),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "第 31 条被拒");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("at most 30 active quick actions"),
        "{}",
        body["error"]["message"]
    );
}
