//! `/api/autopilots*` 写面的**并发 / 指派 / 鉴权**端到端（M5-2 / LUM-1567）：真 PostgreSQL + 真 `router`。
//!
//! 本文件覆盖的 `DoD`：
//!
//! | `DoD` | 用例 |
//! | --- | --- |
//! | ④ 并发两条「写订阅者」不互相踩掉 | [`subscriber_replace_is_atomic_under_concurrency`] |
//! | ④ 并发授权不丢 | [`concurrent_collaborator_grants_all_land`] |
//! | ⑤ squad / agent 指派校验（`validateAutopilotAssigneeForSave`） | [`assignee_validation_for_agent_and_squad`] |
//! | ⑥ 非成员 404 / 无写权 403 / 协作者能写不能转授权 | [`access_refusals_and_collaborator_lifecycle`] |
//!
//! ① ② ③ 与删除语义在 `crud.rs`；共享夹具在 `crud_support.rs`；核对记录见 `docs/50-M5-2-WRITE-FACE.md`。
//!
//! 每条都是 `#[ignore]`：`MULTICA_TEST_DATABASE_URL` 未设置 → 打印跳过并 `return`；
//! **设了却连不上 → panic**（库坏了必须红，静默跳过会让「空跑」伪装成绿）。

use serde_json::{json, Value};
use uuid::Uuid;

use super::crud_support::{
    cleanup_all, create_one, create_payload, flat_error, rule_versions, seed_agent, seed_squad,
    sorted_keys, subscriber, subscriber_ids, CREATE_URIS,
};
use super::support::{call, err_message, seed_outsider, seed_user, seed_workspace};
// ---------------------------------------------------------------------------
// ④ 并发
// ---------------------------------------------------------------------------

/// `DoD` ④：两条并发「整表替换订阅者」不得互相踩掉 —— 终态必须是**某一次请求的完整集合**
/// （不是两半、不是空），行数与集合大小一致，且没有 500（advisory 锁排序错误会以死锁暴露）。
///
/// 两条请求都在事务外先读 `prev`，再在同一事务里按 UUID 升序拿 advisory 锁；后到者要么在
/// `FOR UPDATE` 后看到别人已提交的 `updated_at` ⇒ 409（扁平体），要么自己的 `prev` 本来就是
/// 提交后的状态 ⇒ 200。两种结果都**不能**留下半截状态。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn subscriber_replace_is_atomic_under_concurrency() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip subscriber_replace_is_atomic_under_concurrency: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let first = seed_user(&pool, ws, "member").await;
    let second = seed_user(&pool, ws, "member").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;

    let mut payload = create_payload("concurrent", agent, "run_only");
    payload["subscribers"] = json!([subscriber(owner)]);
    let id = create_one(&app, ws, owner, payload).await;
    let uri = format!("/api/autopilots/{id}");

    let left = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"subscribers": [subscriber(owner), subscriber(first)]})),
    );
    let right = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"subscribers": [subscriber(owner), subscriber(second)]})),
    );
    let ((left_status, left_body), (right_status, right_body)) = tokio::join!(left, right);

    for (status, body) in [(left_status, &left_body), (right_status, &right_body)] {
        assert!(
            status == 200 || status == 409,
            "只有 200 / 409 两种合法结果，实得 {status}: {body}"
        );
        if status == 409 {
            assert_eq!(
                flat_error(body),
                (
                    "the autopilot changed while it was being edited; reload and try again.",
                    "autopilot_update_conflict"
                )
            );
        }
    }
    assert!(left_status == 200 || right_status == 200, "至少一次要成功");

    let final_ids = subscriber_ids(&pool, id).await;
    let mut with_first = vec![owner, first];
    let mut with_second = vec![owner, second];
    with_first.sort_unstable();
    with_second.sort_unstable();
    assert!(
        final_ids == with_first || final_ids == with_second,
        "终态必须是某一次请求的完整集合（不得半截）: {final_ids:?}"
    );

    // 整表替换不是实质变更 ⇒ 版本不增（两条都改订阅者，仍只有 v1）。
    assert_eq!(rule_versions(&pool, id).await.len(), 1);

    cleanup_all(&pool, ws, &[owner, first, second]).await;
}

/// `DoD` ④ 的另一半：并发**增量**授权（`POST …/collaborators`）互相不得覆盖 —— 4 个成员同时授权，
/// 4 行必须全部落地（`ON CONFLICT` 只刷新 `granted_by`，不是「后写覆盖先写」）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn concurrent_collaborator_grants_all_land() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip concurrent_collaborator_grants_all_land: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;
    let id = create_one(&app, ws, owner, create_payload("grants", agent, "run_only")).await;
    let uri = format!("/api/autopilots/{id}/collaborators");

    let members = [
        seed_user(&pool, ws, "member").await,
        seed_user(&pool, ws, "member").await,
        seed_user(&pool, ws, "member").await,
        seed_user(&pool, ws, "member").await,
    ];
    let requests = members.map(|member| {
        call(
            &app,
            "POST",
            &uri,
            ws,
            owner,
            Some(json!({"user_id": member.to_string()})),
        )
    });
    let results = futures_join(requests).await;
    for (member, (status, body)) in members.iter().zip(results.iter()) {
        assert_eq!(*status, 201, "{member} ⇒ {body}");
        // 响应带的是**整张**列表 ⇒ 这里不能断言「恰好 1 行」：并发期间别人的授权可能已经落库。
        let granted = body["collaborators"].as_array().expect("array");
        assert!(
            granted.iter().any(|row| row["user_id"] == json!(member)),
            "本次授权必须出现在返回的列表里: {body}"
        );
    }

    let granted: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM autopilot_collaborator WHERE autopilot_id = $1 ORDER BY user_id",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .expect("select collaborators");
    let mut expected = members.to_vec();
    expected.sort_unstable();
    assert_eq!(granted, expected, "4 条并发授权一条都不能丢");

    cleanup_all(
        &pool,
        ws,
        &[owner, members[0], members[1], members[2], members[3]],
    )
    .await;
}

/// 四个 future 并发跑（`tokio::join!` 只到 4 元，写个小工具免得每处都展开）。
async fn futures_join<F: std::future::Future>(futures: [F; 4]) -> [F::Output; 4] {
    let [a, b, c, d] = futures;
    let (a, b, c, d) = tokio::join!(a, b, c, d);
    [a, b, c, d]
}

// ---------------------------------------------------------------------------
// ⑤ 指派校验
// ---------------------------------------------------------------------------

/// `DoD` ⑤：`validateAutopilotAssigneeForSave` 的 agent / squad 两条腿 —— 归档、runtime、
/// 成员范围、以及私密队长那条 invoke 门（403，且无 admin 越权）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)]
async fn assignee_validation_for_agent_and_squad() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip assignee_validation_for_agent_and_squad: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let admin = seed_user(&pool, ws, "admin").await;
    let ready = seed_agent(&pool, ws, owner, true, false).await;
    let no_runtime = seed_agent(&pool, ws, owner, false, false).await;
    let archived = seed_agent(&pool, ws, owner, true, true).await;
    // 队长属于**第三个人**（既不是 owner 也不是 admin）且 `private` ⇒ 连 admin 也调不动
    // （MUL-3963：`private` 只放行队长 owner，admin 不免检）。
    let stranger = seed_user(&pool, ws, "member").await;
    let foreign_private = seed_agent(&pool, ws, stranger, true, false).await;
    let (other_ws, _) = seed_workspace(&pool, "owner").await;
    let foreign_ws_agent = seed_agent(&pool, other_ws, owner, true, false).await;

    let squad = seed_squad(&pool, ws, ready, owner, false).await;
    let squad_no_runtime = seed_squad(&pool, ws, no_runtime, owner, false).await;
    let squad_archived = seed_squad(&pool, ws, ready, owner, true).await;
    let squad_private_leader = seed_squad(&pool, ws, foreign_private, owner, false).await;

    // squad 正常路径：落到 `assignee_type='squad'` + 快照跟着变。
    let mut payload = create_payload("squad job", squad, "run_only");
    payload["assignee_type"] = json!("squad");
    let id = create_one(&app, ws, owner, payload).await;
    let (status, detail) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{id}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(detail["autopilot"]["assignee_type"], json!("squad"));
    assert_eq!(detail["autopilot"]["assignee_id"], json!(squad));
    assert_eq!(
        rule_versions(&pool, id).await[0].2["assignee_type"],
        json!("squad")
    );

    // 负例矩阵：`(载荷, 状态码, 期望文案)`。
    let negatives: [(Value, u16, &str); 8] = [
        (
            json!({"title": "t", "assignee_id": no_runtime.to_string(), "execution_mode": "run_only"}),
            422,
            "assignee agent needs a runtime before this autopilot can be active",
        ),
        (
            json!({"title": "t", "assignee_id": archived.to_string(), "execution_mode": "run_only"}),
            422,
            "assignee agent is archived; pick a different agent",
        ),
        (
            json!({"title": "t", "assignee_id": foreign_ws_agent.to_string(), "execution_mode": "run_only"}),
            400,
            "assignee must be a valid agent in this workspace",
        ),
        (
            json!({"title": "t", "assignee_id": Uuid::new_v4().to_string(), "execution_mode": "run_only"}),
            400,
            "assignee must be a valid agent in this workspace",
        ),
        (
            json!({"title": "t", "assignee_type": "squad", "assignee_id": squad_archived.to_string(),
                   "execution_mode": "run_only"}),
            422,
            "squad is archived; pick a different squad",
        ),
        (
            json!({"title": "t", "assignee_type": "squad", "assignee_id": squad_no_runtime.to_string(),
                   "execution_mode": "run_only"}),
            422,
            "squad leader needs a runtime before this autopilot can be active",
        ),
        (
            json!({"title": "t", "assignee_type": "squad", "assignee_id": squad_private_leader.to_string(),
                   "execution_mode": "run_only"}),
            403,
            "cannot assign autopilot to squad with private leader",
        ),
        (
            json!({"title": "t", "assignee_type": "member", "assignee_id": ready.to_string(),
                   "execution_mode": "run_only"}),
            400,
            "assignee_type must be agent or squad",
        ),
    ];
    for (payload, expected_status, expected) in negatives {
        // 私密队长那条用 **admin** 调，才能证明「无 admin 越权」。
        let caller = if expected_status == 403 { admin } else { owner };
        let (status, body) = call(
            &app,
            "POST",
            CREATE_URIS[0],
            ws,
            caller,
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, expected_status, "{payload} ⇒ {body}");
        assert!(
            err_message(&body).contains(expected),
            "{payload} ⇒ 期望含 {expected:?}，实得 {:?}",
            err_message(&body)
        );
    }

    // 成对改派：squad → agent 时 `assignee_type` 与 `assignee_id` 必须同时给。
    let uri = format!("/api/autopilots/{id}");
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"assignee_type": "agent", "assignee_id": archived.to_string()})),
    )
    .await;
    assert_eq!(status, 422, "{body}");
    assert!(err_message(&body).contains("assignee agent is archived"));
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"assignee_type": "agent", "assignee_id": ready.to_string()})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["assignee_type"], json!("agent"));
    assert_eq!(body["assignee_id"], json!(ready));

    cleanup_all(&pool, ws, &[owner, admin, stranger]).await;
    cleanup_all(&pool, other_ws, &[]).await;
}

// ---------------------------------------------------------------------------
// ⑥ 鉴权 + 协作者生命周期
// ---------------------------------------------------------------------------

/// `DoD` ⑥：非成员一律 **404**（不是 403 —— 上游 `requireWorkspaceMember` 用 404 掩盖存在性）；
/// 成员但没有写权 → **403 扁平体**（`autopilot_forbidden`）；协作者**能写但不能转授权**。
/// 同时把两条协作者路由的完整生命周期走一遍（201 → 幂等 → 200 撤销 → 幂等）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)]
async fn access_refusals_and_collaborator_lifecycle() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip access_refusals_and_collaborator_lifecycle: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let member = seed_user(&pool, ws, "member").await;
    let grantee = seed_user(&pool, ws, "member").await;
    let outsider = seed_outsider(&pool).await;
    let (other_ws, other_member) = seed_workspace(&pool, "member").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;
    let id = create_one(
        &app,
        ws,
        owner,
        create_payload("guarded", agent, "run_only"),
    )
    .await;
    let uri = format!("/api/autopilots/{id}");
    let collab_uri = format!("/api/autopilots/{id}/collaborators");

    // 非成员：四条写路由全部 404（连 body 都不读 ⇒ 畸形 body 也 404）。
    let outsider_calls = [
        call(&app, "PATCH", &uri, ws, outsider, Some(json!({}))).await,
        call(&app, "DELETE", &uri, ws, outsider, None).await,
        // create 的字段门槛在成员门槛**之前**（与上游同序）⇒ 想验 404 就必须给合法 body，
        // 否则先撞 400 `title is required`。
        call(
            &app,
            "POST",
            CREATE_URIS[0],
            ws,
            outsider,
            Some(create_payload("nope", agent, "run_only")),
        )
        .await,
        call(&app, "POST", &collab_uri, ws, outsider, Some(json!({}))).await,
    ];
    for (status, body) in &outsider_calls {
        assert_eq!(*status, 404, "非成员必须 404（不泄露存在性）: {body}");
        assert!(!err_message(body).is_empty(), "{body}");
    }

    // 跨工作区：成员身份只对**自己的** workspace 有效 ⇒ 用别人的 id 也是 404。
    let (status, body) = call(&app, "PATCH", &uri, other_ws, other_member, Some(json!({}))).await;
    assert_eq!(status, 404, "{body}");

    // 本 workspace 的普通 member（非创建者、无授权）⇒ 403 扁平体，文案逐字。
    let (status, body) = call(&app, "PATCH", &uri, ws, member, Some(json!({"title": "x"}))).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        flat_error(&body),
        (
            "only the autopilot creator, a workspace admin, or a granted collaborator can manage this autopilot",
            "autopilot_forbidden"
        )
    );
    assert_eq!(
        sorted_keys(&body),
        vec!["code", "error"],
        "扁平体只有两个键"
    );

    // 改授权比「能写」更窄：普通 member 连自己的授权列表都改不了（另一条文案）。
    let (status, body) = call(
        &app,
        "POST",
        &collab_uri,
        ws,
        member,
        Some(json!({"user_id": member.to_string()})),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(flat_error(&body).1, "autopilot_forbidden");
    assert!(flat_error(&body)
        .0
        .starts_with("only the autopilot creator or a workspace admin"));

    // 创建者可以加协作者：201 + **整张**列表；重复授权幂等（仍一行，`granted_by` 刷新）。
    let (status, body) = call(
        &app,
        "POST",
        &collab_uri,
        ws,
        owner,
        Some(json!({"user_id": grantee.to_string()})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let rows = body["collaborators"].as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["user_type"], json!("member"));
    assert_eq!(rows[0]["user_id"], json!(grantee));
    assert_eq!(rows[0]["granted_by"], json!(owner));
    let (status, body) = call(
        &app,
        "POST",
        &collab_uri,
        ws,
        owner,
        Some(json!({"user_id": grantee.to_string()})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["collaborators"].as_array().expect("array").len(), 1);

    // 协作者**能写**（200）……
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        grantee,
        Some(json!({"title": "by collaborator"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], json!("by collaborator"));
    // ……但**不能**转授权（MUL-3807 的提权防护）。
    let (status, body) = call(
        &app,
        "POST",
        &collab_uri,
        ws,
        grantee,
        Some(json!({"user_id": member.to_string()})),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(flat_error(&body)
        .0
        .starts_with("only the autopilot creator or a workspace admin"));

    // 授权前的校验：`user_id` 缺失 / 非 UUID / 非本 workspace 成员。
    let cases: [(Value, &str); 3] = [
        (json!({}), "validation error: user_id is required"),
        (
            json!({"user_id": "nope"}),
            "validation error: user_id must be a valid uuid",
        ),
        (
            json!({"user_id": outsider.to_string()}),
            "validation error: user_id must be a member of this workspace",
        ),
    ];
    for (payload, expected) in cases {
        let (status, body) =
            call(&app, "POST", &collab_uri, ws, owner, Some(payload.clone())).await;
        assert_eq!(status, 400, "{payload} ⇒ {body}");
        assert_eq!(err_message(&body), expected);
    }

    // 撤销：200 + 空列表；非 UUID 的 `userId` 文案与新增路径**不同**（`user id`，带空格）。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{collab_uri}/not-a-uuid"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        err_message(&body),
        "validation error: user id must be a valid uuid"
    );
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{collab_uri}/{grantee}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["collaborators"], json!([]));
    // 再撤一次（行已不在）仍 200 —— 幂等。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{collab_uri}/{grantee}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    // 撤销之后协作者不再能写。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        grantee,
        Some(json!({"title": "again"})),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // 角色 admin（非创建者）在 ownership 腿上就放行，无需授权行。
    let admin = seed_user(&pool, ws, "admin").await;
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        admin,
        Some(json!({"title": "by admin"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], json!("by admin"));

    // 协作者路由的路径参数与主路由同源：非 UUID 的 autopilot id ⇒ 400（不是 404）。
    let (status, body) = call(
        &app,
        "POST",
        "/api/autopilots/not-a-uuid/collaborators",
        ws,
        owner,
        Some(json!({"user_id": grantee.to_string()})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        err_message(&body),
        "validation error: autopilot id must be a valid uuid"
    );

    cleanup_all(&pool, ws, &[owner, member, grantee, outsider, admin]).await;
    cleanup_all(&pool, other_ws, &[other_member]).await;
}
