//! `/api/autopilots*` 写面的**核心 CRUD** 端到端（M5-2 / LUM-1567）：真 PostgreSQL + 真 `router`。
//!
//! 本文件覆盖的 `DoD`（口径与取舍记在 `docs/50-M5-2-WRITE-FACE.md`）：
//!
//! | `DoD` | 用例 |
//! | --- | --- |
//! | ① create → detail 字段逐字一致（`autopilotToResponse`） | [`create_shape_matches_detail_and_rule_v1`] |
//! | ② 三态补丁（缺省 / 显式 `null` / 给值） | [`patch_three_state_semantics`] |
//! | ②⑤ 字段校验文案矩阵 | [`create_and_patch_validation_messages`] |
//! | ③ 规则版本 append-only（只有实质编辑才发版） | [`rule_versions_append_only_on_substantive_edits`] |
//! | 删除 = 归档（子行留作历史） | [`delete_archives_and_preserves_history`] |
//!
//! ④ 并发、⑤ 指派校验、⑥ 鉴权在 `crud_access.rs`；共享夹具在 `crud_support.rs`。
//!
//! 每条都是 `#[ignore]`：`MULTICA_TEST_DATABASE_URL` 未设置 → 打印跳过并 `return`；
//! **设了却连不上 → panic**（库坏了必须红，静默跳过会让「空跑」伪装成绿）。

use serde_json::{json, Value};
use uuid::Uuid;

use super::crud_support::{
    autopilot_state, child_counts, cleanup_all, create_one, create_payload, delivery_count,
    flat_error, rule_versions, seed_agent, seed_delivery, sorted_keys, subscriber, subscriber_ids,
    trigger_publisher, CREATE_URIS,
};
use super::support::{
    call, err_message, seed_collaborator, seed_outsider, seed_run, seed_schedule_trigger,
    seed_subscriber, seed_user, seed_workspace,
};
// ---------------------------------------------------------------------------
// ① create → detail
// ---------------------------------------------------------------------------

/// `DoD` ①：create 的响应必须是 `autopilotToResponse` 的**写面形态**（16 个基础键 + `subscribers`，
/// 三条列表专属列与两个权限位缺席），且 detail 的 `autopilot` 对象逐字段一致。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // ① 的断言就是「17 个键逐字对照」，拆散反而看不出全景
async fn create_shape_matches_detail_and_rule_v1() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip create_shape_matches_detail_and_rule_v1: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let watcher = seed_user(&pool, ws, "member").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;

    let mut payload = create_payload("nightly triage", agent, "create_issue");
    payload["description"] = json!("把昨晚失败的任务收一遍");
    payload["assignee_type"] = json!("agent");
    payload["issue_title_template"] = json!("{{date}} nightly");
    payload["subscribers"] = json!([subscriber(watcher)]);

    let (status, body) = call(&app, "POST", CREATE_URIS[0], ws, owner, Some(payload)).await;
    assert_eq!(status, 201);

    // 键集合（`omitempty` 的边界也是契约）。
    assert_eq!(
        sorted_keys(&body),
        sorted_keys(&json!({
            "id": "", "workspace_id": "", "title": "", "description": "", "project_id": "",
            "assignee_type": "", "assignee_id": "", "status": "", "pause_reason": "",
            "execution_mode": "", "issue_title_template": "", "created_by_type": "",
            "created_by_id": "", "last_run_at": "", "created_at": "", "updated_at": "",
            "subscribers": [],
        })),
        "写面响应 = 16 个基础键 + subscribers（列表三列与 can_write/can_manage_access 缺席）: {body}"
    );

    let id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    assert_eq!(body["workspace_id"], json!(ws));
    assert_eq!(body["title"], json!("nightly triage"));
    assert_eq!(body["description"], json!("把昨晚失败的任务收一遍"));
    assert_eq!(body["assignee_type"], json!("agent"));
    assert_eq!(body["assignee_id"], json!(agent));
    assert_eq!(body["status"], json!("active"));
    assert_eq!(body["execution_mode"], json!("create_issue"));
    assert_eq!(body["issue_title_template"], json!("{{date}} nightly"));
    assert_eq!(body["created_by_type"], json!("member"));
    assert_eq!(body["created_by_id"], json!(owner));
    // 无 `omitempty` 的三列：未设置时是**显式 null**，不是缺字段。
    assert!(!body["description"].is_null());
    assert_eq!(body["project_id"], Value::Null);
    assert_eq!(body["pause_reason"], Value::Null);
    assert_eq!(body["last_run_at"], Value::Null);
    assert!(
        body.get("pause_reason").is_some() && body.get("project_id").is_some(),
        "显式 null 必须保留键: {body}"
    );
    let subs = body["subscribers"].as_array().expect("array");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["user_type"], json!("member"));
    assert_eq!(subs[0]["user_id"], json!(watcher));
    assert!(subs[0]["created_at"].is_string(), "created_at: {}", subs[0]);

    // 详情：`autopilot` 逐字段一致；详情**额外**盖两个权限位（读面契约），触发器/协作者信封在。
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
    for key in sorted_keys(&body) {
        assert_eq!(
            detail["autopilot"][&key], body[&key],
            "① create 与 detail 的 `{key}` 必须逐字段一致"
        );
    }
    assert_eq!(detail["autopilot"]["can_write"], json!(true));
    assert_eq!(detail["autopilot"]["can_manage_access"], json!(true));
    assert_eq!(detail["triggers"], json!([]));
    assert_eq!(detail["collaborators"], json!([]));

    // 创建**就是**一次实质发布：v1 规则版本，发布者 = 创建者，快照四列。
    let versions = rule_versions(&pool, id).await;
    assert_eq!(versions.len(), 1, "create ⇒ 恰好 v1: {versions:?}");
    assert_eq!(versions[0].1, Some(owner));
    assert_eq!(
        versions[0].2,
        json!({
            "assignee_type": "agent",
            "assignee_id": agent.to_string(),
            "status": "active",
            "execution_mode": "create_issue",
        })
    );

    // 双形态注册键：不带尾斜杠的形态必须同样可达（且真的建了第二条）。
    let second = json!({
        "title": "second",
        "assignee_id": agent.to_string(),
        "execution_mode": "run_only",
    });
    let (status, body2) = call(&app, "POST", CREATE_URIS[1], ws, owner, Some(second)).await;
    assert_eq!(status, 201, "alias 形态必须 201: {body2}");
    assert_ne!(body2["id"], json!(id));

    cleanup_all(&pool, ws, &[owner, watcher]).await;
}

// ---------------------------------------------------------------------------
// ② 三态补丁
// ---------------------------------------------------------------------------

/// `DoD` ②：「键不出现 = 不改 / 显式 `null` = 清空（或保持）/ 给值 = 覆盖」三态逐列。
///
/// 直赋值列（`issue_title_template` / `project_id`）在**缺省**时必须回填 `prev`，否则一次 PATCH
/// 就把它们清空；`description` 是 `COALESCE` 列 ⇒ `null` 与缺省都保持，只有给字符串才覆盖。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 三态 × 若干列，逐条列在同一个用例里才好对照
async fn patch_three_state_semantics() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip patch_three_state_semantics: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;

    let mut payload = create_payload("t0", agent, "create_issue");
    payload["description"] = json!("d1");
    payload["issue_title_template"] = json!("{{date}}-t");
    let id = create_one(&app, ws, owner, payload).await;
    let uri = format!("/api/autopilots/{id}");
    let alias_uri = format!("/api/autopilots/{id}/");

    // (1) 缺省 = 不改：title 变了，`description` / 模板原样（模板若没回填 `prev` 就会变 null）。
    let (status, body) = call(&app, "PATCH", &uri, ws, owner, Some(json!({"title": "t1"}))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], json!("t1"));
    assert_eq!(body["description"], json!("d1"));
    assert_eq!(body["issue_title_template"], json!("{{date}}-t"));
    assert_eq!(
        rule_versions(&pool, id).await.len(),
        1,
        "只改 title 是装饰性编辑 ⇒ 不发版"
    );

    // (2) 给值 = 覆盖（`description` 是 run 的 PROMPT ⇒ 实质变更）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"description": "d2"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["description"], json!("d2"));
    assert_eq!(
        rule_versions(&pool, id).await.len(),
        2,
        "description 是实质列"
    );

    // (3) 显式 `null` = 保持（`COALESCE(null, description)`）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"description": null})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["description"], json!("d2"));
    assert_eq!(
        rule_versions(&pool, id).await.len(),
        2,
        "null 不改列 ⇒ 不发版"
    );

    // (4) 直赋值列给 `null` = **清空**（上游 `ptrToText(nil)`）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"issue_title_template": null})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["issue_title_template"], Value::Null);
    assert!(body.get("issue_title_template").is_some(), "{body}");

    // (5) 空补丁 = 200 且全字段不变（`updated_at` 会前进 —— 它就是乐观并发的比较列）。
    let before = body.clone();
    let (status, body) = call(&app, "PATCH", &uri, ws, owner, Some(json!({}))).await;
    assert_eq!(status, 200, "{body}");
    for key in [
        "title",
        "description",
        "assignee_id",
        "status",
        "execution_mode",
    ] {
        assert_eq!(body[&key], before[&key], "空补丁不得改 `{key}`");
    }
    assert_ne!(
        body["updated_at"], before["updated_at"],
        "UPDATE 总是推进 updated_at"
    );

    // (6) 尾斜杠别名同样可达（双形态）。
    let (status, body) = call(
        &app,
        "PATCH",
        &alias_uri,
        ws,
        owner,
        Some(json!({"title": "t2"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], json!("t2"));

    // (7) 未知键被忽略（上游 `json.Unmarshal` 只认识自己的字段）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"priority": "high"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], json!("t2"));

    cleanup_all(&pool, ws, &[owner]).await;
}

/// ②/⑤ 的文案矩阵：每条拒绝都是**本地错误体**（嵌套 `{"error":{"message":…}}`）+ 上游文案。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 文案矩阵，逐条断言才有价值
async fn create_and_patch_validation_messages() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip create_and_patch_validation_messages: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;
    let outsider = seed_outsider(&pool).await;
    let id = create_one(&app, ws, owner, create_payload("t", agent, "run_only")).await;
    let uri = format!("/api/autopilots/{id}");

    // ---- create 的字段门槛（顺序逐字：body → title → assignee_id → execution_mode → 模板）----
    let cases: [(Value, &str); 5] = [
        (
            json!({"assignee_id": agent.to_string(), "execution_mode": "run_only"}),
            "title is required",
        ),
        (
            json!({"title": "t", "execution_mode": "run_only"}),
            "assignee_id is required",
        ),
        (
            json!({"title": "t", "assignee_id": agent.to_string()}),
            "execution_mode is required",
        ),
        (
            json!({"title": "t", "assignee_id": agent.to_string(), "execution_mode": "bogus"}),
            "execution_mode must be create_issue or run_only",
        ),
        (
            json!({"title": "t", "assignee_id": agent.to_string(), "execution_mode": "run_only",
                   "issue_title_template": "{{nope}}"}),
            "unknown template variable",
        ),
    ];
    for (payload, expected) in cases {
        let (status, body) = call(
            &app,
            "POST",
            CREATE_URIS[0],
            ws,
            owner,
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, 400, "{payload} ⇒ {body}");
        assert!(
            err_message(&body).contains(expected),
            "{payload} ⇒ 期望含 {expected:?}，实得 {:?}",
            err_message(&body)
        );
    }

    // 订阅者数组的三档校验 + 去重（同一 user 两次 ⇒ 落库一行）。
    let dup = [subscriber(owner), subscriber(owner)];
    let cases: [(Value, &str); 3] = [
        (
            json!([{"user_type": "agent", "user_id": owner.to_string()}]),
            "subscribers[0].user_type must be 'member'",
        ),
        (
            json!([{"user_type": "member", "user_id": ""}]),
            "subscribers[0].user_id is required",
        ),
        (
            json!([{"user_type": "member", "user_id": "nope"}]),
            "subscribers[0].user_id must be a valid uuid",
        ),
    ];
    for (subs, expected) in cases {
        let mut payload = create_payload("t", agent, "run_only");
        payload["subscribers"] = subs;
        let (status, body) = call(
            &app,
            "POST",
            CREATE_URIS[0],
            ws,
            owner,
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, 400, "{payload} ⇒ {body}");
        assert!(err_message(&body).contains(expected), "{body}");
    }
    let mut payload = create_payload("dedup", agent, "run_only");
    payload["subscribers"] = json!(dup);
    let dedup_id = create_one(&app, ws, owner, payload).await;
    assert_eq!(
        subscriber_ids(&pool, dedup_id).await,
        vec![owner],
        "重复 user_id 去重（首见者胜）"
    );

    // 非本 workspace 的成员进订阅者列表 ⇒ 400（下标文案）。
    let mut payload = create_payload("t", agent, "run_only");
    payload["subscribers"] = json!([subscriber(outsider)]);
    let (status, body) = call(&app, "POST", CREATE_URIS[0], ws, owner, Some(payload)).await;
    assert_eq!(status, 400, "{body}");
    // 这条来自 `mc-autopilot` 的 `Validation` 错误 ⇒ 带 `mc-errors` 的 Display 前缀
    // （`validation error: `），与 crud.rs 里直接 `bad_request(...)` 的裸文案不同。
    assert_eq!(
        err_message(&body),
        "validation error: subscribers[0] is not a member of this workspace"
    );

    // ---- patch 的字段门槛 ----
    let cases: [(Value, &str); 5] = [
        (
            json!({"assignee_type": "squad"}),
            "assignee_id is required when changing assignee_type",
        ),
        (json!({"assignee_id": null}), "assignee_id cannot be null"),
        (
            json!({"assignee_id": "nope"}),
            "assignee_id must be a valid uuid",
        ),
        (
            json!({"assignee_type": "member"}),
            "assignee_type must be agent or squad",
        ),
        (
            json!({"project_id": Uuid::new_v4().to_string()}),
            "project_id must reference a project in this workspace",
        ),
    ];
    for (payload, expected) in cases {
        let (status, body) = call(&app, "PATCH", &uri, ws, owner, Some(payload.clone())).await;
        assert_eq!(status, 400, "{payload} ⇒ {body}");
        assert!(
            err_message(&body).contains(expected),
            "{payload} ⇒ 期望含 {expected:?}，实得 {:?}",
            err_message(&body)
        );
    }

    // 非对象 payload：上游 `json.Unmarshal` 的 `invalid request body`（`null` 是**合法**的零值体）。
    let (status, body) = call(&app, "PATCH", &uri, ws, owner, Some(json!("x"))).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(err_message(&body), "validation error: invalid request body");
    let (status, body) = call(&app, "POST", CREATE_URIS[0], ws, owner, Some(Value::Null)).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(err_message(&body), "validation error: title is required");

    // 路径参数非 UUID ⇒ 400（不是 404）。
    let (status, body) = call(
        &app,
        "PATCH",
        "/api/autopilots/not-a-uuid",
        ws,
        owner,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        err_message(&body),
        "validation error: autopilot id must be a valid uuid"
    );

    cleanup_all(&pool, ws, &[owner, outsider]).await;
}

// ---------------------------------------------------------------------------
// ③ 规则版本
// ---------------------------------------------------------------------------

/// `DoD` ③：`autopilot_rule_version` **只增不改**，且只有实质列变化才发版；实质编辑同时把所有触发器
/// 的配置责任人转给本次编辑者。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)]
async fn rule_versions_append_only_on_substantive_edits() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip rule_versions_append_only_on_substantive_edits: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let admin = seed_user(&pool, ws, "admin").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;
    let other_agent = seed_agent(&pool, ws, owner, true, false).await;
    let id = create_one(&app, ws, owner, create_payload("t", agent, "run_only")).await;
    let uri = format!("/api/autopilots/{id}");
    let trigger = seed_schedule_trigger(&pool, id, "0 9 * * *", "1 hour").await;
    sqlx::query("UPDATE autopilot_trigger SET published_by_type = 'member', published_by_id = $2 WHERE id = $1")
        .bind(trigger)
        .bind(other_agent)
        .execute(&pool)
        .await
        .expect("stamp trigger publisher");

    let v1 = rule_versions(&pool, id).await;
    assert_eq!(v1.len(), 1);

    // 装饰性编辑（title）⇒ 不发版、不转责任。
    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        owner,
        Some(json!({"title": "renamed"})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(rule_versions(&pool, id).await.len(), 1);
    assert_eq!(trigger_publisher(&pool, id).await, Some(other_agent));

    // 实质编辑（status = paused）⇒ 发版 + 转责任给编辑者（admin）。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        admin,
        Some(json!({"status": "paused"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], json!("paused"));
    let versions = rule_versions(&pool, id).await;
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[1].1, Some(admin), "发布者 = 本次编辑者");
    assert_eq!(versions[1].2["status"], json!("paused"));
    assert_eq!(trigger_publisher(&pool, id).await, Some(admin));
    // append-only：v1 逐字节没动。
    assert_eq!(versions[0], v1[0], "v1 必须原样保留（append-only）");

    // 同值再 PATCH（无实际变化）⇒ 不发版。
    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        admin,
        Some(json!({"status": "paused"})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(rule_versions(&pool, id).await.len(), 2);

    // 改派（who）⇒ 发版。
    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        admin,
        Some(json!({"assignee_id": other_agent.to_string()})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["assignee_id"], json!(other_agent));
    assert_eq!(rule_versions(&pool, id).await.len(), 3);

    // 换执行模式（what）⇒ 发版；快照跟着走。
    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        ws,
        admin,
        Some(json!({"execution_mode": "create_issue"})),
    )
    .await;
    assert_eq!(status, 200);
    let versions = rule_versions(&pool, id).await;
    assert_eq!(versions.len(), 4);
    assert_eq!(
        versions[3].2,
        json!({
            "assignee_type": "agent",
            "assignee_id": other_agent.to_string(),
            "status": "paused",
            "execution_mode": "create_issue",
        })
    );
    assert_eq!(versions[0], v1[0], "仍然只有追加");

    cleanup_all(&pool, ws, &[owner, admin]).await;
}

// ---------------------------------------------------------------------------
// 删除 = 归档
// ---------------------------------------------------------------------------

/// 删除语义：上游 `DeleteAutopilot` 只做 `ArchiveAutopilot` + 追加一条规则版本，
/// **执行历史全部保留**（run / task / webhook delivery / subscriber / collaborator）；
/// 列表按 `status <> 'archived'` 隐藏，详情仍可读；重复删除幂等且每次都留痕。
///
/// ※ `DoD` 原文写的是「`DeleteAutopilot` 的关联行清理（trigger / collaborator / subscriber）」，
/// 与钉住的上游实现相反（本地按上游实现，核对记录见 `docs/50-M5-2-WRITE-FACE.md` §5）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn delete_archives_and_preserves_history() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip delete_archives_and_preserves_history: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let member = seed_user(&pool, ws, "member").await;
    let agent = seed_agent(&pool, ws, owner, true, false).await;

    let id = create_one(&app, ws, owner, create_payload("doomed", agent, "run_only")).await;
    let alive = create_one(&app, ws, owner, create_payload("alive", agent, "run_only")).await;
    let uri = format!("/api/autopilots/{id}");

    // 先把执行历史攒齐：触发器等子行都挂在这个 autopilot 上。
    let trigger = seed_schedule_trigger(&pool, id, "0 9 * * *", "1 hour").await;
    seed_subscriber(&pool, id, member).await;
    seed_run(&pool, id, "completed", "1 hour").await;
    let versions_before = rule_versions(&pool, id).await.len();
    assert_eq!(versions_before, 1, "create 已写 v1");

    // 没有写权的成员删不动（删除也是写 ⇒ 扁平 403，且发生在任何 DB 写入之前）。
    let (status, body) = call(&app, "DELETE", &uri, ws, member, None).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(flat_error(&body).1, "autopilot_forbidden");

    // 协作者与 webhook 投递就位后开始删。
    seed_collaborator(&pool, id, member, owner).await;
    seed_delivery(&pool, ws, id, trigger).await;
    assert_eq!(
        child_counts(&pool, id).await,
        (1, 1, 1, 1),
        "(trigger, subscriber, collaborator, run) 各 1 行"
    );
    assert_eq!(delivery_count(&pool, id).await, 1);

    // 204 且**无 body**。
    let (status, body) = call(&app, "DELETE", &uri, ws, owner, None).await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(body, Value::Null, "204 不带 body");

    // 库态：归档 + 清 `pause_reason`；子行一行不少。
    assert_eq!(
        autopilot_state(&pool, id).await,
        ("archived".to_string(), None),
        "删除 = 归档 + 清 pause_reason"
    );
    assert_eq!(
        child_counts(&pool, id).await,
        (1, 1, 1, 1),
        "执行历史（含 run/collaborator）必须原样保留"
    );
    assert_eq!(delivery_count(&pool, id).await, 1, "webhook_delivery 留档");

    // 归档也是实质状态变更 ⇒ 追加一条 `status='archived'` 的版本，发布者 = 删除者。
    let versions = rule_versions(&pool, id).await;
    assert_eq!(versions.len(), versions_before + 1, "删除也要留痕");
    let latest = versions.last().expect("last");
    assert_eq!(latest.1, Some(owner), "发布者 = 删除者");
    assert_eq!(latest.2["status"], json!("archived"));
    assert_eq!(versions[0].2["status"], json!("active"), "v1 未被改写");

    // 列表隐藏归档项、且不影响同 workspace 的其他 autopilot。
    let (status, list) = call(&app, "GET", "/api/autopilots/", ws, owner, None).await;
    assert_eq!(status, 200);
    let listed: Vec<&str> = list["autopilots"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert!(listed.contains(&alive.to_string().as_str()), "{list}");
    assert!(
        !listed.contains(&id.to_string().as_str()),
        "归档后不再出现在列表: {list}"
    );

    // 详情仍可读（归档是状态，不是删除）。
    let (status, detail) = call(&app, "GET", &uri, ws, owner, None).await;
    assert_eq!(status, 200, "归档后详情仍可读");
    assert_eq!(detail["autopilot"]["status"], json!("archived"));

    // 重复删除幂等：仍 204，且**再**追加一条版本（上游每次调用都记一笔）。
    let (status, _) = call(&app, "DELETE", &uri, ws, owner, None).await;
    assert_eq!(status, 204, "已归档再删仍 204");
    assert_eq!(
        rule_versions(&pool, id).await.len(),
        versions_before + 2,
        "每次都留痕"
    );

    // 不存在 → 404；非 UUID → 400（先解析路径参数）。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/autopilots/{}", Uuid::new_v4()),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert!(!err_message(&body).is_empty());
    let (status, body) = call(
        &app,
        "DELETE",
        "/api/autopilots/not-a-uuid",
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        err_message(&body),
        "validation error: autopilot id must be a valid uuid"
    );

    cleanup_all(&pool, ws, &[owner, member]).await;
}
