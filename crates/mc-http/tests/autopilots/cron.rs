//! `GET /api/autopilots/cron-preview`（M5-1）。
//!
//! 上游契约要点（`handler/autopilot_cron_preview.go`）：
//! - 错误体是**扁平**的 `{"error": "<msg>", "code": "<code>"}`（不是本仓统一的嵌套形状）；
//! - `code` 只有 `invalid_cron` / `invalid_timezone` 两个，顺序是「**先 expr 空判 → 再 tz 校验 →
//!   最后解析 expr**」，所以 `expr` 与 `tz` 同时非法时给的是 `invalid_timezone`；
//! - `next_runs` 是 `time.RFC3339`（秒精度、`Z` 结尾）的 3 个时刻；
//! - 语法合法但**永不触发**的表达式给 200 + 短/空数组，而不是错误。

use serde_json::{json, Value};

use super::support::{call, cleanup, seed_workspace};

/// 取一条预览（`expr`/`tz` 由调用方负责 URL 编码）。
async fn preview(
    app: &axum::Router,
    ws: uuid::Uuid,
    user: uuid::Uuid,
    query: &str,
) -> (axum::http::StatusCode, Value) {
    call(
        app,
        "GET",
        &format!("/api/autopilots/cron-preview?{query}"),
        ws,
        user,
        None,
    )
    .await
}

/// 每 15 分钟：3 个递增、秒精度 `Z` 结尾、分针落在 0/15/30/45。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn preview_returns_three_future_occurrences() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip preview_returns_three_future_occurrences: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    let (status, body) = preview(&app, ws, owner, "expr=%2A%2F15+%2A+%2A+%2A+%2A").await;
    assert_eq!(status, 200, "body={body}");
    let runs: Vec<&str> = body["next_runs"]
        .as_array()
        .expect("next_runs 是数组")
        .iter()
        .map(|v| v.as_str().expect("元素是字符串"))
        .collect();
    assert_eq!(runs.len(), 3, "previewCount=3: {body}");
    let now = chrono::Utc::now();
    let mut previous: Option<chrono::DateTime<chrono::Utc>> = None;
    for raw in runs {
        // `time.RFC3339` 秒精度 + `Z`：长度 20，例如 2026-09-23T14:05:00Z。
        assert_eq!(raw.len(), 20, "秒精度 RFC3339: {raw}");
        assert!(raw.ends_with('Z'), "UTC 用 Z 结尾: {raw}");
        let at = chrono::DateTime::parse_from_rfc3339(raw)
            .expect("可解析")
            .with_timezone(&chrono::Utc);
        assert!(at > now, "必须在未来: {raw}");
        assert_eq!(at.timestamp() % 900, 0, "每 15 分钟对齐: {raw}");
        if let Some(prev) = previous {
            assert!(at > prev, "必须严格递增: {raw}");
        }
        previous = Some(at);
    }

    cleanup(&pool, ws, &[owner]).await;
}

/// 时区参与计算：`0 9 * * *` 在 `Asia/Shanghai` 是 UTC 01:00，缺省（UTC）是 09:00。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn preview_applies_timezone_and_defaults_to_utc() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip preview_applies_timezone_and_defaults_to_utc: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    let expr = "0+9+%2A+%2A+%2A";
    let (status, body) = preview(&app, ws, owner, &format!("expr={expr}&tz=Asia%2FShanghai")).await;
    assert_eq!(status, 200, "body={body}");
    let first = chrono::DateTime::parse_from_rfc3339(body["next_runs"][0].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(first.time().format("%H:%M").to_string(), "01:00", "{body}");

    let (status, body) = preview(&app, ws, owner, &format!("expr={expr}&tz=UTC")).await;
    assert_eq!(status, 200);
    let first = chrono::DateTime::parse_from_rfc3339(body["next_runs"][0].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(first.time().format("%H:%M").to_string(), "09:00", "{body}");

    // 缺省 tz = UTC（不是服务端本地时区）。
    let (status, body) = preview(&app, ws, owner, &format!("expr={expr}")).await;
    assert_eq!(status, 200);
    let first = chrono::DateTime::parse_from_rfc3339(body["next_runs"][0].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(first.time().format("%H:%M").to_string(), "09:00", "{body}");

    cleanup(&pool, ws, &[owner]).await;
}

/// 四种 400 的 `code` 与**扁平错误体**；同时钉住「expr 空 → tz 非法 → expr 非法」的判定顺序。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn preview_error_codes_and_flat_body() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip preview_error_codes_and_flat_body: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    let cases: [(&str, &str); 4] = [
        // 缺 expr（`expr=` 与不传等价）。
        ("expr=", "invalid_cron"),
        // 4 字段（本仓与上游都是 5 字段）。
        ("expr=%2A+%2A+%2A+%2A", "invalid_cron"),
        // 分针越界。
        ("expr=60+%2A+%2A+%2A+%2A", "invalid_cron"),
        // 未知时区。
        (
            "expr=%2A+%2A+%2A+%2A+%2A&tz=Mars%2FOlympus",
            "invalid_timezone",
        ),
    ];
    for (query, expected_code) in cases {
        let (status, body) = preview(&app, ws, owner, query).await;
        assert_eq!(status, 400, "query={query} body={body}");
        assert_eq!(body["code"], expected_code, "query={query} body={body}");
        assert!(
            body["error"].is_string(),
            "扁平体：error 是字符串而不是对象: {body}"
        );
    }

    // expr 与 tz 同时非法 → 上游先校验 tz ⇒ invalid_timezone。
    let (status, body) = preview(&app, ws, owner, "expr=nonsense&tz=Mars%2FOlympus").await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "invalid_timezone", "{body}");

    // expr 为空且 tz 非法 → expr 先判 ⇒ invalid_cron。
    let (status, body) = preview(&app, ws, owner, "expr=&tz=Mars%2FOlympus").await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "invalid_cron", "{body}");

    // 缺 expr 时的文案与上游逐字一致。
    let (_, body) = preview(&app, ws, owner, "expr=").await;
    assert_eq!(body["error"], "expr is required");

    cleanup(&pool, ws, &[owner]).await;
}

/// 语法合法但永不触发（2 月 31 日）⇒ 200 + **空数组**（编辑器靠这个区分「永不开跑」与「写错」）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn preview_never_firing_expression_is_empty_array() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip preview_never_firing_expression_is_empty_array: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;

    let (status, body) = preview(&app, ws, owner, "expr=0+0+31+2+%2A").await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["next_runs"], json!([]), "永不触发不是错误: {body}");

    cleanup(&pool, ws, &[owner]).await;
}
