//! `GET /health`（M10-1）的可判定用例 —— **零数据库**。
//!
//! 上游判据来源：`server/cmd/server/integration_test.go::TestHealth`（`90e0bdf` L212）——
//! `http.Get(testServer.URL + "/health")` ⇒ 200 且 `status == "ok"`；它被抽成
//! `contracts/golden/health/001-TestHealth-L212.json`（⑨ 门的那一条 `unmounted → pass`）。
//! 那条 fixture 的 `json_subset` 是**空对象** ⇒ 它只证明"挂上了且 200"，**不**证明字段集；
//! 字段级判据只有一处，就是本文件 + `docs/64` §2.1 的逐字契约。
//!
//! ## 为什么全部用例都不需要真库
//!
//! `/health` 的 handler **不挂** `State<AppState>`（见 `live.rs`）⇒ 类型上就拿不到 `Db`。
//! 本文件把这一点做成**可观测**的判据：`AppState` 的库用**真 URL 但不可达**的
//! `connect_lazy` 装（与 `mc-conformance` stateless 层逐字同款），任何一次真查询都会在
//! 5s acquire 超时后失败 ⇒ 200 只可能来自"根本没碰库"。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{process_started_at, LiveResponse, COMMIT};
use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

/// 真 URL、**不可达**地址 —— `mc-conformance/src/harness.rs::STATELESS_URL` 的同款形状。
/// `connect_lazy` 不拨号、不建库；一旦有人真查询，5s 后超时失败。
const UNREACHABLE_DB: &str = "postgres://health-probe:health-probe@127.0.0.1:1/health_probe";

/// 装一个 `AppState`：库**不可达**，其余按 `mc-conformance` 的 stateless 层同款。
fn state() -> Arc<AppState> {
    let db = mc_db::Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-2103"));
    Arc::new(AppState::new(
        db,
        RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            ..Default::default()
        },
        realtime,
        ws,
    ))
}

/// 只装**探针面**（`probes::router`）—— 单元级判据，不受其他切片影响。
fn probe_router() -> Router {
    let state = state();
    crate::routes::probes::router(state.clone()).with_state(state)
}

/// 装**全量** router（与 `apps/mc-server/src/main.rs:175` 同款装配）——
/// 判据是"`mount_slice_probes()` 的 `.merge` 真的把这条键挂到了根路径上"。
/// ⑨ 的 fixture 走的正是这条路径（`via: router`）。
fn full_router() -> Router {
    let state = state();
    crate::routes::router(state.clone()).with_state(state)
}

/// `GET <uri>` ⇒ `(status, headers, body-as-json)`。
async fn get_json(router: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("dispatch");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, value)
}

/// `DateTime<Utc>` ← `SystemTime`（`std::fs::Metadata::modified`）。
fn utc(t: std::time::SystemTime) -> DateTime<Utc> {
    t.into()
}

// ---- 1. 契约：200 + `status == "ok"` + pid + started_at ----------------------------

/// 上游 `TestHealth` 的逐条断言 + 本片契约里**必现**的两项（`pid` / `started_at`）。
#[tokio::test]
async fn live_is_200_with_status_ok_pid_and_started_at() {
    let (status, headers, body) = get_json(&full_router(), "/health").await;

    assert_eq!(status, StatusCode::OK, "上游 TestHealth 逐字：expected 200");
    assert_eq!(
        body["status"],
        json!("ok"),
        "上游 TestHealth 逐字：status ok"
    );
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json")),
        "上游 writeJSON 逐字设 Content-Type: application/json，实际 {:?}",
        headers.get(header::CONTENT_TYPE)
    );

    // `pid` 是**本进程**的 pid（上游 `os.Getpid()`）。omitempty 只省略 0 ⇒ 真实进程里必现。
    let pid = body["pid"].as_u64().expect("pid 必现（进程 pid 非 0）");
    assert_eq!(
        pid,
        u64::from(std::process::id()),
        "pid 必须是回答者自己的 pid"
    );

    // `started_at` 是进程启动时刻（RFC3339 秒级、Z 结尾）—— 形态与「非请求时刻」在
    // `started_at_is_process_scoped_not_request_scoped` 里逐条判；这里只判**必现 + 可解析**。
    let started_at = body["started_at"].as_str().expect("started_at 必现");
    let parsed = DateTime::parse_from_rfc3339(started_at).expect("started_at 是 RFC3339");
    assert!(
        started_at.ends_with('Z'),
        "上游 time.RFC3339 + UTC ⇒ Z 结尾，实际 {started_at}"
    );
    assert!(
        !started_at.contains('.'),
        "上游 time.RFC3339 是**秒级**（不带纳秒），实际 {started_at}"
    );
    assert_eq!(parsed.offset().local_minus_utc(), 0, "必须是 UTC");
}

/// 上游 `writeJSON` 只写四个键（+ 无 omitempty 的 `status`）⇒ 不得多塞字段。
#[tokio::test]
async fn live_body_has_exactly_the_upstream_keys() {
    let (_, _, body) = get_json(&probe_router(), "/health").await;
    let obj = body.as_object().expect("JSON 对象");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();

    let mut expected: Vec<&str> = vec!["status", "pid", "started_at"];
    if !COMMIT.is_empty() {
        expected.push("commit");
    }
    expected.sort_unstable();
    assert_eq!(
        keys, expected,
        "上游 liveResponse 只有这四个键（后三者 omitempty）"
    );
}

// ---- 2. 「不触库」：库不可达也照样 200 ----------------------------------------------

/// `docs/64` §6.5 的 M10-1 专属验收：**不触库** —— 没有 `MULTICA_TEST_DATABASE_URL`、
/// 库地址不可达时也必须 200。
///
/// `AppState` 的库是 `connect_lazy(127.0.0.1:1)`（acquire 超时 5s）⇒ 若 handler 真的
/// 查库，本用例会在 5s 后拿到 500/超时，而不是 200。handler 连 `State` 都不挂 ⇒ 结构性成立。
#[tokio::test]
async fn live_is_200_even_when_the_database_is_unreachable() {
    assert!(
        std::env::var("MULTICA_TEST_DATABASE_URL").map_or(true, |v| v.trim().is_empty()),
        "本用例的前提是**没有**可用测试库；请在不带 MULTICA_TEST_DATABASE_URL 的环境里跑（门 ⑤ 正是如此）"
    );

    let started = std::time::Instant::now();
    let (status, _, body) = get_json(&probe_router(), "/health").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "库不可达也必须 200（liveness 不触库）"
    );
    assert_eq!(body["status"], json!("ok"));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "不得出现 acquire 超时（5s）级别的耗时，实测 {:?} —— 那说明有人碰了库",
        started.elapsed()
    );
}

/// 反向对照：同一份装配里**真的**有库句柄，只是不可达 —— 让上一条用例的前提可复核。
#[tokio::test]
async fn the_harness_database_really_is_a_real_but_unreachable_pool() {
    let state = state();
    // `Db` 本身存在且可克隆；只证明"这不是 placeholder 空壳"，不发起查询。
    let _clone: mc_db::Db = state.db.clone();
    assert!(
        UNREACHABLE_DB.contains("127.0.0.1:1"),
        "stateless 层同款：真 URL、不可达端口"
    );
}

// ---- 3. `started_at` 是**进程级常量**，不是请求时刻 --------------------------------

/// 上游注释（`health.go` L40-45）的业务判据：本地工具拿 `started_at` 跟自己启动进程的
/// 时刻比，识破"新进程没绑上、老进程还在服务"。若 `started_at` 写成 `Utc::now()`，
/// 这条判据归零 ⇒ 本用例用**跨秒**把它钉死。
///
/// 三条断言：
/// 1. **早于请求时间**：先取 `before`，等 1.1s（跨秒），再发请求；请求时刻的实现会得到
///    `trunc(before) + 1s > before` ⇒ 红。进程级单例取的是更早的时刻 ⇒ 恒 ≤ `before`。
/// 2. **进程启动之后**：下界取本测试二进制的 mtime —— 进程能跑起来 ⇒ 二进制必先存在，
///    所以"二进制落盘时刻"是一个**恒成立**的进程启动下界（留 2s 余量给文件系统时钟粒度）。
/// 3. **跨请求逐字相同**：两次请求比对字符串（锁定"每次请求现取"这个退化）。
#[tokio::test]
async fn started_at_is_process_scoped_not_request_scoped() {
    let router = probe_router();
    let before = Utc::now();

    tokio::time::sleep(Duration::from_millis(1100)).await;

    let (status, _, first) = get_json(&router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    let started_at = first["started_at"]
        .as_str()
        .expect("started_at 必现")
        .to_string();
    let parsed = DateTime::parse_from_rfc3339(&started_at)
        .expect("RFC3339")
        .with_timezone(&Utc);

    // ① 早于请求时间（跨秒 ⇒ 请求时刻的实现必然越界）。
    assert!(
        parsed <= before,
        "started_at({started_at}) 必须 ≤ 发请求之前的时刻({before}) —— \
         否则它就是请求时刻（上游注释要防的正是这个）"
    );

    // ② 在进程启动之后：二进制 mtime 是恒成立的进程启动下界。
    let exe = std::env::current_exe().expect("current_exe");
    let exe_mtime = std::fs::metadata(&exe)
        .and_then(|m| m.modified())
        .expect("测试二进制的 mtime");
    assert!(
        parsed >= utc(exe_mtime) - chrono::Duration::seconds(2),
        "started_at({started_at}) 早于测试二进制落盘时刻({}) —— 那说明它不是本进程的启动时刻",
        utc(exe_mtime)
    );

    // ③ 进程级单例：与 `process_started_at()` 的渲染逐字相同，且跨请求不变。
    assert_eq!(
        started_at,
        process_started_at().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "响应里的 started_at 必须就是进程级单例（不是另取一次 now）"
    );
    let (_, _, second) = get_json(&router, "/health").await;
    assert_eq!(
        second["started_at"], first["started_at"],
        "同一个进程的两次请求逐字相同（进程级常量）"
    );

    // ④ 两个不同的 router 实例（同一进程）也必须是同一个值 —— 单例不是"每个 router 一份"。
    let (_, _, from_another_router) = get_json(&probe_router(), "/health").await;
    assert_eq!(
        from_another_router["started_at"], first["started_at"],
        "started_at 是**进程**级，不是 router 级"
    );
}

// ---- 4. `commit` 的 omitempty 行为与构建期注入逐字一致 ------------------------------

/// 上游 dev 构建 `commit == ""` ⇒ 因为 `omitempty`，键**不出现**（不是 `"commit": ""`）。
#[tokio::test]
async fn commit_reflects_build_time_injection_with_omitempty() {
    let (_, _, body) = get_json(&probe_router(), "/health").await;
    let obj = body.as_object().expect("JSON 对象");

    if COMMIT.is_empty() {
        assert!(
            !obj.contains_key("commit"),
            "未注入 MC_BUILD_COMMIT ⇒ omitempty 生效，键不出现；实际 {body}"
        );
    } else {
        assert_eq!(
            obj.get("commit").and_then(Value::as_str),
            Some(COMMIT),
            "注入后的值必须逐字透出"
        );
    }
}

/// `omitempty` 的**纯**序列化契约（不经过 HTTP）：四个字段的省略行为逐个钉住。
///
/// 这条用例的价值在于它**不依赖**进程状态 —— 谁把 `skip_serializing_if` 删了（例如为了
/// "少一个 clippy 豁免"），这里立刻红。
#[test]
fn omitempty_contract_matches_upstream_live_response() {
    let all_zero = LiveResponse {
        status: "ok",
        pid: 0,
        commit: "",
        started_at: String::new(),
    };
    assert_eq!(
        serde_json::to_value(&all_zero).expect("serialize"),
        json!({ "status": "ok" }),
        "三个 omitempty 字段在零值下全部省略；status **没有** omitempty ⇒ 必现"
    );

    let filled = LiveResponse {
        status: "ok",
        pid: 4242,
        commit: "cafe1234",
        started_at: "2026-01-02T03:04:05Z".into(),
    };
    assert_eq!(
        serde_json::to_value(&filled).expect("serialize"),
        json!({
            "status": "ok",
            "pid": 4242,
            "commit": "cafe1234",
            "started_at": "2026-01-02T03:04:05Z",
        })
    );
}

// ---- 5. 形态门：只注册**无尾斜杠**那一形态（`docs/64` §1.4） ------------------------

/// 上游 `router.go:1399` 是 plain `r.Get("/health", …)` ⇒ chi 只服务 `/health`；
/// 本片多注册 `/health/` 就是 `EXTRA_ALIAS` 硬失败（本波 allowlist 是 0 数据行）。
///
/// 这条用例是那个静态判据（`scripts/slash_alias_audit.py`）的**运行时对照**：
/// 尾斜杠形态必须打不到本切片的 handler。
#[tokio::test]
async fn only_the_plain_form_is_served() {
    let router = probe_router();

    let (plain, _, _) = get_json(&router, "/health").await;
    assert_eq!(plain, StatusCode::OK, "无尾斜杠形态必须服务");

    let (slashed, _, _) = get_json(&router, "/health/").await;
    assert_ne!(
        slashed,
        StatusCode::OK,
        "带尾斜杠形态**不得**被本切片服务（上游 plain 注册只服务一种形态）"
    );
}
