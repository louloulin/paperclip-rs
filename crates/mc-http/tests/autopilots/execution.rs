//! 执行面读/触发路由 e2e（M5-4 / LUM-1569）：#15 `POST …/trigger`、#16 `GET …/runs`、
//! #17 `GET …/runs/:runId`。
//!
//! 派发**服务层**的三块（`create_issue` / `run_only` / `sync`）在 `dispatch.rs` 里直接调
//! `mc_autopilot::dispatch` 的 API 测（那三条链没有对应的 HTTP 面）；这里只钉**路由契约**：
//!
//! 1. **路由形态**：401 / 400 / 405、单形态（尾斜杠必须是 404，不是重定向）；路径参数非 UUID
//!    时 **autopilot 先于 run id** 报错（上游顺序）；
//! 2. **鉴权分层**：读面任何成员都行；#15 走与写面同一条 `requireAutopilotWrite`
//!    （非成员 404 掩盖存在性 / 普通成员 403），且**鉴权早于状态检查**（未启用的 autopilot
//!    对普通成员仍是 403，不是 400 —— 否则等于泄露状态）；
//! 3. **数据面**：列表是 **slim**（`trigger_payload` 恒 `null`）、按 `created_at DESC`、
//!    `total` = 本页条数、`limit` 上限 100、`limit=abc` 退回默认 20；详情带 `trigger_payload`；
//!    跨 autopilot 的 runId 与不存在的 runId 都折成同一个 404。
//!
//! 触发成功的判据用「assignee 解析不出来 ⇒ run 落 `skipped` + `reason_code=target_unavailable`
//! 但仍 200」这条路径 —— 它是**唯一**不需要 agent/runtime fixture 就能走完 dispatch 的形态，
//! 恰好也是准入闸最该钉的语义（跳过 ≠ 失败）。
//!
//! ⚠️ 配额 **429** 的形状不在这里测：`install_policy_provider` 是进程级 `OnceLock`，
//! `usage.rs` 已经占了那个名额（它按工作区作答，本文件的临时工作区永远拿 `off`）。
//! 429 的响应体/`Retry-After` 由 `src/routes/autopilots/execution.rs` 的**单元测试**
//! 直接钉 `quota_exceeded_response`（handler 调的就是那一个函数）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{
    app_with_db, call, cleanup, connect, err_message, seed_autopilot, seed_outsider, seed_run,
    seed_user, seed_workspace,
};
use super::triggers::{probe_status, upstream_message};

/// #15–#17 的 `(方法, URI 模板)`：三条都是**单形态**。
const EXECUTION_ROUTES: [(&str, &str); 3] = [
    ("POST", "/api/autopilots/{ap}/trigger"),
    ("GET", "/api/autopilots/{ap}/runs"),
    ("GET", "/api/autopilots/{ap}/runs/{run}"),
];

/// 把模板里的占位符替换成真 id。
fn route_of(template: &str, autopilot_id: Uuid, run_id: Uuid) -> String {
    template
        .replace("{ap}", &autopilot_id.to_string())
        .replace("{run}", &run_id.to_string())
}

/// 带 `Idempotency-Key` 的触发请求（`support::call` 不支持自定义头）。
async fn keyed_trigger(
    app: &axum::Router,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    key: String,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(super::support::USER_ID_HEADER, user_id.to_string())
        .header(super::support::WORKSPACE_HEADER, workspace_id.to_string())
        .header("Idempotency-Key", &key)
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (
        status,
        super::support::body_json(res.into_body()).await,
    )
}

/// 缺 `X-Multica-User-Id` → 401（三条路由都要，且**在同方法**上探）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn missing_user_header_is_401_on_every_execution_route() {
    let Some((pool, db)) = connect().await else {
        println!("skip missing_user_header_is_401_on_every_execution_route: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let run = seed_run(&pool, autopilot, "completed", "1 hour").await;

    for (method, template) in EXECUTION_ROUTES {
        let uri = route_of(template, autopilot, run);
        assert_eq!(
            probe_status(&app, method, &uri, None, Some(&ws.to_string())).await,
            401,
            "{method} {uri}"
        );
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 缺工作区头 / 工作区头非 UUID → 400。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn bad_workspace_header_is_400_on_every_execution_route() {
    let Some((pool, db)) = connect().await else {
        println!("skip bad_workspace_header_is_400_on_every_execution_route: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let run = seed_run(&pool, autopilot, "completed", "1 hour").await;

    for (method, template) in EXECUTION_ROUTES {
        let uri = route_of(template, autopilot, run);
        assert_eq!(
            probe_status(&app, method, &uri, Some(owner), None).await,
            400,
            "缺头 {method} {uri}"
        );
        assert_eq!(
            probe_status(&app, method, &uri, Some(owner), Some("not-a-uuid")).await,
            400,
            "坏 uuid {method} {uri}"
        );
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 405：`/runs` 上没有 POST、`/trigger` 上没有 GET、`/runs/:id` 上没有 DELETE。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn method_not_allowed_outside_the_route_table() {
    let Some((pool, db)) = connect().await else {
        println!("skip method_not_allowed_outside_the_route_table: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let run = seed_run(&pool, autopilot, "completed", "1 hour").await;

    let probes = [
        ("PUT", format!("/api/autopilots/{autopilot}/runs")),
        ("GET", format!("/api/autopilots/{autopilot}/trigger")),
        ("DELETE", format!("/api/autopilots/{autopilot}/runs/{run}")),
    ];
    for (method, uri) in probes {
        let (status, _) = call(&app, method, &uri, ws, owner, None).await;
        assert_eq!(status, 405, "{method} {uri}");
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 单形态路由的尾斜杠别名必须是 404（`slash_alias_audit.py` 会把它读成 `EXTRA_ALIAS`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn single_form_routes_reject_the_trailing_slash_alias() {
    let Some((pool, db)) = connect().await else {
        println!("skip single_form_routes_reject_the_trailing_slash_alias: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let run = seed_run(&pool, autopilot, "completed", "1 hour").await;

    for (method, uri) in [
        ("POST", format!("/api/autopilots/{autopilot}/trigger/")),
        ("GET", format!("/api/autopilots/{autopilot}/runs/")),
        ("GET", format!("/api/autopilots/{autopilot}/runs/{run}/")),
    ] {
        let (status, body) = call(&app, method, &uri, ws, owner, None).await;
        assert_eq!(status, 404, "{method} {uri} → {body}");
    }
    cleanup(&pool, ws, &[owner]).await;
}

/// 权限链：#16/#17 任何成员都读得到；#15 与写面同闸（非成员 404 / 普通成员 403 / 创建者 200）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn reads_are_member_wide_and_trigger_reuses_the_write_gate() {
    let Some((pool, db)) = connect().await else {
        println!("skip reads_are_member_wide_and_trigger_reuses_the_write_gate: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let plain_member = seed_user(&pool, ws, "member").await;
    let outsider = seed_outsider(&pool).await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let run = seed_run(&pool, autopilot, "completed", "1 hour").await;

    // ① 读面：普通成员 200（读权限是全工作区的）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/runs"),
        ws,
        plain_member,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 1);
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/runs/{run}"),
        ws,
        plain_member,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // ② 非成员：#15 404（`require_member` 掩盖存在性，与读面同一条腿）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/trigger"),
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "workspace");

    // ③ 成员但非创建者 / 非 admin / 未授权：#15 403（上游 `requireAutopilotTriggerInvoker` 文案）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/trigger"),
        ws,
        plain_member,
        None,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        err_message(&body),
        "forbidden: insufficient permission for this autopilot"
    );

    cleanup(&pool, ws, &[owner, plain_member, outsider]).await;
}

/// 鉴权**早于**状态检查：未启用的 autopilot 对普通成员仍是 403（不是 400），
/// 否则任何人靠状态码就能探出别的 autopilot 是死是活（上游 `loadAutopilotInWorkspace` →
/// `requireAutopilotTriggerInvoker` → 才轮到 `status != active`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn authorization_precedes_the_status_check() {
    let Some((pool, db)) = connect().await else {
        println!("skip authorization_precedes_the_status_check: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let plain_member = seed_user(&pool, ws, "member").await;
    let autopilot = seed_autopilot(&pool, ws, "paused", "member", owner).await;
    let uri = format!("/api/autopilots/{autopilot}/trigger");

    let (status, body) = call(&app, "POST", &uri, ws, plain_member, None).await;
    assert_eq!(status, 403, "{body}");

    // 创建者自己来 → 403 让位给 400（状态闸）。
    let (status, body) = call(&app, "POST", &uri, ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot is not active");

    cleanup(&pool, ws, &[owner, plain_member]).await;
}

/// 触发成功路径：assignee 解析不出 agent ⇒ run 落 `skipped`，但 HTTP 仍是 **200**
/// （上游 `errDispatchSkipped` 不是错误；UI 按 `status` + `reason_code` 弹 toast）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn trigger_returns_200_with_a_skipped_run_when_the_assignee_is_gone() {
    let Some((pool, db)) = connect().await else {
        println!("skip trigger_returns_200_with_a_skipped_run_when_the_assignee_is_gone: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    // `seed_autopilot` 的 `assignee_id` 是随机 uuid ⇒ `resolve_assignee_leader` 必然 Missing。
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/trigger"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "skipped");
    assert_eq!(body["reason_code"], "target_unavailable");
    assert_eq!(body["source"], "manual");
    assert_eq!(body["autopilot_id"], autopilot.to_string());
    // 跳过的 run 没有 issue / task，`completed_at` 也由 `update_skipped` 落下。
    assert!(body["issue_id"].is_null(), "{body}");
    assert!(body["task_id"].is_null(), "{body}");
    assert!(body["completed_at"].is_string(), "{body}");
    // `trigger_payload` 是**非 slim** 的形状 ⇒ 显式 null（手动触发没有载荷）。
    assert!(body["trigger_payload"].is_null(), "{body}");
    // 人读原因照发（`failure_reason`），与 `reason_code` 是两个字段。
    assert!(
        body["failure_reason"]
            .as_str()
            .is_some_and(|r| r.contains("assignee")),
        "{body}"
    );

    // 落库核对：`autopilot_run` 真有一行 `skipped`（不是只在响应里编的）。
    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT status, reason_code, failure_reason FROM autopilot_run \
         WHERE autopilot_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(autopilot)
    .fetch_one(&pool)
    .await
    .expect("load autopilot_run");
    assert_eq!(row.0, "skipped");
    assert_eq!(row.1.as_deref(), Some("target_unavailable"));
    assert!(row.2.is_some());

    // 同一条路由第二次照样 200（每次 manual 触发都是一条新 run：manual 没有幂等键，
    // `uq_autopilot_run_trigger_planned` / `uq_autopilot_run_webhook_delivery` 都管不到它）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/trigger"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot_run WHERE autopilot_id = $1")
        .bind(autopilot)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 2, "两次手动触发应是两条 run");

    cleanup(&pool, ws, &[owner]).await;
}

/// `Idempotency-Key` 的两条形状规则：`>255` → 400；空/缺 → 服务端自己生成（不影响 200）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn idempotency_key_length_is_validated() {
    let Some((pool, db)) = connect().await else {
        println!("skip idempotency_key_length_is_validated: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let uri = format!("/api/autopilots/{autopilot}/trigger");

    // 256 字符：恰好超一位 → 400（上游 `len(key) > 255`）。
    let (status, body) = keyed_trigger(&app, &uri, ws, owner, "k".repeat(256)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upstream_message(&body), "Idempotency-Key is too long");

    // 255 字符：合法。空串也合法（走 `req-{uuid}` 兜底）—— 两次都该是 200。
    for key in ["k".repeat(255), String::new()] {
        let (status, body) = keyed_trigger(&app, &uri, ws, owner, key.clone()).await;
        assert_eq!(status, StatusCode::OK, "key len {} → {body}", key.len());
    }

    cleanup(&pool, ws, &[owner]).await;
}

/// 列表：**slim** 投影（`trigger_payload` 恒 null）、`created_at DESC`、`total` = 本页条数。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn list_runs_is_slim_ordered_and_bounded() {
    let Some((pool, db)) = connect().await else {
        println!("skip list_runs_is_slim_ordered_and_bounded: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;

    // 三条 run：`triggered_at` 由新到旧，但 `created_at` 才是排序键 —— 这里让两者同向，
    // 并给最新那条带上 `trigger_payload`（webhook 载荷可能很大，列表必须摘掉它）。
    let oldest = seed_run(&pool, autopilot, "completed", "3 hours").await;
    let middle = seed_run(&pool, autopilot, "failed", "2 hours").await;
    let newest = seed_run(&pool, autopilot, "skipped", "1 hour").await;
    sqlx::query("UPDATE autopilot_run SET trigger_payload = $2::jsonb WHERE id = $1")
        .bind(newest)
        .bind(json!({"event": "push", "big": "x".repeat(4096)}).to_string())
        .execute(&pool)
        .await
        .expect("set trigger_payload");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/runs"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 3, "{body}");
    let runs = body["runs"].as_array().expect("runs array");
    assert_eq!(runs.len(), 3);
    let ids = runs
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            newest.to_string(),
            middle.to_string(),
            oldest.to_string()
        ],
        "created_at DESC"
    );
    // slim：键存在但值是 null（不是缺键 —— 上游 `RunResponse.TriggerPayload` 无 omitempty）。
    for run in runs {
        assert!(
            run.get("trigger_payload").is_some(),
            "slim 投影仍要带键: {run}"
        );
        assert!(run["trigger_payload"].is_null(), "slim 必须摘掉载荷: {run}");
    }

    cleanup(&pool, ws, &[owner]).await;
}

/// `limit` / `offset` 的 `strconv.Atoi` 口径：缺省 20、`>100` 夹到 100、非数字退回默认、
/// `offset` 只认 `>=0`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn limit_and_offset_follow_upstream_atoi_semantics() {
    let Some((pool, db)) = connect().await else {
        println!("skip limit_and_offset_follow_upstream_atoi_semantics: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    for ago in ["5 hours", "4 hours", "3 hours", "2 hours", "1 hour"] {
        seed_run(&pool, autopilot, "completed", ago).await;
    }
    let uri = format!("/api/autopilots/{autopilot}/runs");

    // 缺省：全部 5 条都回来（默认 20）。
    let (status, body) = call(&app, "GET", &uri, ws, owner, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 5);

    // limit=2 → 2 条；offset=2 → 跳过前 2 条（同向排序 ⇒ 第 3、4 条）。
    let (status, body) = call(&app, "GET", &format!("{uri}?limit=2"), ws, owner, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 2);
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?limit=2&offset=2"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 2);
    let page_two_first = body["runs"][0]["id"].as_str().unwrap().to_string();

    // 全量第 3 条应与 `offset=2` 的首条一致（排序稳定 ⇒ 可精确比对）。
    let (_, all) = call(&app, "GET", &format!("{uri}?limit=100"), ws, owner, None).await;
    assert_eq!(all["runs"][2]["id"].as_str().unwrap(), page_two_first);

    // 非数字 / 负数 / 0 都退回默认 20（Go `Atoi` 失败即保持默认；`limit <= 0` 同上）。
    for query in ["limit=abc", "limit=-1", "limit=0", "offset=abc"] {
        let (status, body) = call(&app, "GET", &format!("{uri}?{query}"), ws, owner, None).await;
        assert_eq!(status, 200, "{query} → {body}");
        assert_eq!(body["total"], 5, "{query} 应退回默认 limit/offset");
    }

    // `limit=500` 夹到 100：5 条全回（夹的是上限，不是报 400）。
    let (status, body) = call(&app, "GET", &format!("{uri}?limit=500"), ws, owner, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 5);

    cleanup(&pool, ws, &[owner]).await;
}

/// 详情：带 `trigger_payload` 原文；跨 autopilot 的 runId 与不存在的 runId 都是 404 `run`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn get_run_returns_the_payload_and_hides_other_autopilots_runs() {
    let Some((pool, db)) = connect().await else {
        println!("skip get_run_returns_the_payload_and_hides_other_autopilots_runs: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let mine = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let theirs = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let run = seed_run(&pool, mine, "completed", "1 hour").await;
    let foreign = seed_run(&pool, theirs, "completed", "1 hour").await;
    // 详情是**非 slim**：载荷必须回来，且 `result` 也在。
    sqlx::query(
        "UPDATE autopilot_run SET trigger_payload = $2::jsonb, result = $3::jsonb WHERE id = $1",
    )
    .bind(run)
    .bind(json!({"event": "push"}).to_string())
    .bind(json!({"ok": true}).to_string())
    .execute(&pool)
    .await
    .expect("set payload/result");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{mine}/runs/{run}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["trigger_payload"], json!({"event": "push"}));
    assert_eq!(body["result"], json!({"ok": true}));
    assert_eq!(body["status"], "completed");

    // 别人的 autopilot 下的 run：404，且文案与「run 不存在」逐字相同。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{mine}/runs/{foreign}"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "run");
    let (status, missing) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{mine}/runs/{}", Uuid::new_v4()),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(err_message(&body), err_message(&missing));

    cleanup(&pool, ws, &[owner]).await;
}

/// 非 UUID 路径参数 → 400，且 **autopilot id 的 400 早于 run id 的 400**（上游顺序）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn non_uuid_path_params_are_400_in_upstream_order() {
    let Some((pool, db)) = connect().await else {
        println!("skip non_uuid_path_params_are_400_in_upstream_order: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/runs/nope"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "run id must be a valid uuid");

    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/nope/runs/also-nope",
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot id must be a valid uuid");

    cleanup(&pool, ws, &[owner]).await;
}

/// 触发路由对**不存在**的 autopilot 是 404 `autopilot`（不是 403、也不是 500）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn trigger_on_a_missing_autopilot_is_not_found() {
    let Some((pool, db)) = connect().await else {
        println!("skip trigger_on_a_missing_autopilot_is_not_found: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{}/trigger", Uuid::new_v4()),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(upstream_message(&body), "autopilot");

    // 路径参数不是 uuid → 400（早于「查不到」）。
    let (status, body) = call(&app, "POST", "/api/autopilots/nope/trigger", ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "autopilot id must be a valid uuid");

    cleanup(&pool, ws, &[owner]).await;
}

/// 列表的空工作区形态：`{"runs":[],"total":0}`（不是 `null`、也不是 404）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn empty_run_list_is_an_empty_array_with_zero_total() {
    let Some((pool, db)) = connect().await else {
        println!("skip empty_run_list_is_an_empty_array_with_zero_total: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/autopilots/{autopilot}/runs"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"runs": [], "total": 0}));

    cleanup(&pool, ws, &[owner]).await;
}

