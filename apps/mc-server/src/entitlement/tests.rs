//! M9-9（`LUM-1824`）的用例：**套餐/配额矩阵 18 格** + 两个消费者同策略 + 出站往返 + 回归保护。
//!
//! 拆成子模块是为了让 `entitlement.rs` 本体守在门 ⑩ 的 800 行硬上限之内
//! （与 `mc-http/src/routes/issue_table/tests.rs`、`mc-entitlement/src/client/tests.rs` 同款）。
//!
//! 🔴 **一个进程只能装一次平面**（两个 `OnceLock`）⇒ 本文件里**只有一条**用例会装平面，
//! 其余用例要么走不装平面的分支，要么直接用**未安装**的 `Client` 句柄。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use mc_core::Timestamp;
use mc_entitlement::cache::{gate_with_period, PolicySnapshot, MAX_POLICY_TTL, STALE_GRACE};
use mc_entitlement::types::off_decision;
use mc_entitlement::{Action, Decision, Gate, GateName, Provider, Reason, RefreshOutcome};
use serde_json::json;

use super::*;

fn at(secs: i64) -> Timestamp {
    Timestamp::from_unix(secs)
}

/// 可推进的测试时钟（上游 `now func() time.Time` 字段的可复现替身）。
#[derive(Debug, Clone)]
struct TestClock(Arc<AtomicI64>);

impl TestClock {
    fn at(unix: i64) -> Self {
        Self(Arc::new(AtomicI64::new(unix)))
    }

    fn advance_secs(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }

    fn as_clock(&self) -> Arc<dyn Fn() -> Timestamp + Send + Sync> {
        let handle = self.0.clone();
        Arc::new(move || Timestamp::from_unix(handle.load(Ordering::SeqCst)))
    }
}

fn config_with(pairs: &[(&str, &str)]) -> EntitlementConfig {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    EntitlementConfig::from_env_with(move |name| {
        owned
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })
}

fn mc_cloud_url() -> &'static str {
    mc_http::state::cloud::CLOUD_URL_ENV
}

/// 一个**必然连不上**的基址（本机 1 号端口）⇒ 「不可达」格是确定性的、离线可复现的。
const UNREACHABLE: &str = "http://127.0.0.1:1";

fn offline_client(clock: &TestClock) -> Arc<Client> {
    Arc::new(
        Client::from_validated_base_url(UNREACHABLE, None)
            .expect("client")
            .with_clock(clock.as_clock()),
    )
}

/// 某个 action 下的两个 gate（period 三字段齐全 —— `autopilot_runs` 的硬要求）。
fn snapshot_for_action(action: Action, limit: i64) -> PolicySnapshot {
    PolicySnapshot {
        policy_revision: 5,
        subscription_version: 11,
        cloud_valid_until: at(9_000_000),
        gates: [
            gate_with_period(action, limit, at(1_000), at(2_000)),
            gate_with_period(action, limit, at(1_000), at(2_000)),
        ],
    }
}

// ---------------------------------------------------------------------------
// 缓存态：三档（新鲜 / 陈旧宽限 / 不可达）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheState {
    /// 新鲜：命中即用。
    Fresh,
    /// 陈旧但仍在宽限期内，且处于失败退避期 ⇒ `enforce` 被降级为 `observe`。
    StaleGrace,
    /// 不可达：没有任何可用策略。
    Unreachable,
}

impl CacheState {
    const ALL: [Self; 3] = [Self::Fresh, Self::StaleGrace, Self::Unreachable];

    const fn label(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::StaleGrace => "stale_grace",
            Self::Unreachable => "unreachable",
        }
    }

    const fn reason(self) -> Reason {
        match self {
            Self::Fresh => Reason::CacheFresh,
            Self::StaleGrace => Reason::Stale,
            Self::Unreachable => Reason::Unavailable,
        }
    }
}

/// 把客户端摆到某个缓存态（**离线**：只动缓存与时钟，一个请求都不发）。
fn arrange(client: &Client, clock: &TestClock, workspace: Id, action: Action, state: CacheState) {
    match state {
        CacheState::Fresh => {
            let fetched = mc_entitlement::client::FetchedPolicy {
                snapshot: snapshot_for_action(action, 20),
                valid_for: MAX_POLICY_TTL,
            };
            assert_eq!(
                client.store(workspace, &fetched),
                mc_entitlement::cache::PutOutcome::Stored
            );
        }
        CacheState::StaleGrace => {
            let fetched = mc_entitlement::client::FetchedPolicy {
                snapshot: snapshot_for_action(action, 20),
                valid_for: MAX_POLICY_TTL,
            };
            assert_eq!(
                client.store(workspace, &fetched),
                mc_entitlement::cache::PutOutcome::Stored
            );
            // 推出新鲜期（仍留在 15m 宽限内）并推进失败退避。
            clock.advance_secs(i64::try_from(MAX_POLICY_TTL.as_secs()).expect("secs") + 30);
            client.store_failure(workspace);
        }
        CacheState::Unreachable => {
            // 从未有过任何策略，且最近一次刷新失败 ⇒ 退避中的空壳。
            client.store_failure(workspace);
        }
    }
}

/// 某一格的**期望结论**（3 个 Action × 3 个缓存态 = 9 行，乘 2 个 gate = **18 格**）。
///
/// 逐字来自上游：`enforce` 只有在**新鲜**时才会拦截；陈旧降级为 `observe`；
/// 拿不到策略时一律 `off`（fail-open）。
const fn expected(action: Action, state: CacheState) -> (Action, bool) {
    match (action, state) {
        (_, CacheState::Fresh) => (action, matches!(action, Action::Enforce)),
        // 陈旧宽限：只把 `enforce` 降级；`observe` / `off` 各自原样（降级是恒等的）。
        (Action::Enforce, CacheState::StaleGrace) => (Action::Observe, false),
        (Action::Observe | Action::Off, CacheState::StaleGrace) => (action, false),
        (_, CacheState::Unreachable) => (Action::Off, false),
    }
}

// ---------------------------------------------------------------------------
// 三态装配
// ---------------------------------------------------------------------------

/// 没有合法基址的两种形态：缺 / 空 / 全空白 与 「非空但非法」⇒ **一律不装平面**。
///
/// 这是 `DoD` 第 3 条（回归保护）的第一半：此时 `limit-usage` 恒 204、`autopilots/usage`
/// 恒 `{"action":"off"}`。
#[test]
fn without_a_valid_base_url_the_plane_is_never_installed() {
    for pairs in [
        vec![],
        vec![(mc_cloud_url(), "")],
        vec![(mc_cloud_url(), "   ")],
        vec![(mc_cloud_url(), "nope")],
        // 凭据 / query / fragment 三件套（`mc_cloud::config::validate` 拒）。
        vec![(mc_cloud_url(), "https://user:pass@cloud.test")],
        vec![(mc_cloud_url(), "https://cloud.test?token=x")],
    ] {
        let handles = start(&config_with(&pairs));
        assert!(!handles.is_configured(), "{pairs:?} ⇒ 不是「已配置」");
        assert!(!handles.is_wired(), "{pairs:?} ⇒ 绝不许装平面");
        assert!(handles.plane().is_none(), "{pairs:?} ⇒ 没有平面可交出去");
        // 进程里此刻**仍然**是默认平面：任何工作区的 quota 都是 off。
        let workspace = Id::new();
        assert!(!mc_autopilot::quota::is_enabled(workspace));
        assert!(mc_autopilot::quota::policy_for(workspace).is_none());
        assert_eq!(
            mc_autopilot::quota::off_usage().action,
            mc_autopilot::quota::ACTION_OFF
        );
    }
}

// ---------------------------------------------------------------------------
// 18 格矩阵 + 两个消费者同策略（本进程**唯一**装平面的用例）
// ---------------------------------------------------------------------------

// 18 格逐格断言在一个用例里（`install_*` 是进程级 `OnceLock`，拆开就互相污染）。
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn the_matrix_has_eighteen_cells_and_both_consumers_never_disagree() {
    let clock = TestClock::at(1_000_000);
    let client = offline_client(&clock);
    let plane = Arc::new(EntitlementPlane::new(Arc::clone(&client)));
    // 同一个 `Arc` 装进**两个**进程级槽（模块头「三个挂载点」）。
    assert!(
        mc_autopilot::quota::install_policy_provider(
            Arc::clone(&plane) as Arc<dyn QuotaPolicyProvider>
        ),
        "autopilot 槽在本进程只装一次"
    );
    assert!(
        mc_entitlement::client::install_provider(Arc::clone(&plane) as Arc<dyn Provider>),
        "entitlement 槽在本进程只装一次"
    );

    // 回归保护的第二半：**冷平面**对两个消费者都是 fail-open（不编造策略）。
    let cold = Id::new();
    assert_eq!(
        mc_autopilot::quota::off_usage().action,
        mc_autopilot::quota::ACTION_OFF,
        "冷平面下 usage 仍是 off 形状"
    );
    assert!(mc_autopilot::quota::policy_for(cold).is_none());
    assert_eq!(issue_limit_from(&plane.issue_count(cold)), None);
    assert!(mc_autopilot::quota::off_usage().used.is_none());

    let mut cells = 0usize;
    for action in [Action::Off, Action::Observe, Action::Enforce] {
        for state in CacheState::ALL {
            for gate in GateName::ALL {
                // 每格用**独立**工作区 ⇒ 格子之间零串扰。
                let workspace = Id::new();
                arrange(&client, &clock, workspace, action, state);
                let label = format!("{action}/{:?}/{}", state.label(), gate.as_str());

                // 判决一律**从已安装的槽**读 —— 这正是两个消费者各自会走的路。
                let decision = mc_entitlement::client::provider().gate(workspace, gate);
                let (expected_action, expected_enforcing) = expected(action, state);
                assert_eq!(
                    decision.gate.action, expected_action,
                    "{label}: action 不符（reason={:?}）",
                    decision.reason
                );
                assert_eq!(
                    decision.is_enforcing(),
                    expected_enforcing,
                    "{label}: 是否拦截不符"
                );
                assert_eq!(decision.reason, state.reason(), "{label}: reason 不符");
                assert!(
                    !decision.is_enforcing() || action == Action::Enforce,
                    "{label}: 绝不该有非 enforce 的拦截"
                );

                // 审计字段：**有策略**（含「策略说 off」）时必带修订号与订阅版本；
                // 完全拿不到策略时才是零值（`off_decision` 的形状）。
                if state == CacheState::Unreachable {
                    assert_eq!(decision.policy_revision, 0, "{label}: 无策略时没有修订号");
                    assert_eq!(decision.subscription_version, 0, "{label}");
                } else {
                    assert_eq!(decision.policy_revision, 5, "{label}");
                    assert_eq!(decision.subscription_version, 11, "{label}");
                }

                // ---- 消费者投影 ①：autopilot 面（`/api/autopilots/usage` + 准入）
                let via_provider = quota_policy_from(
                    &mc_entitlement::client::provider().gate(workspace, GateName::AutopilotRuns),
                );
                if gate == GateName::AutopilotRuns {
                    assert_eq!(
                        via_provider.as_ref().map(|policy| policy.action),
                        quota_policy_from(&decision)
                            .as_ref()
                            .map(|policy| policy.action),
                        "{label}: 同一格不得出现两个不同结论"
                    );
                }
                if gate == GateName::AutopilotRuns {
                    // 上游的降级只影响 `enforce`；`observe` 保持 `observe`；
                    // `off` 与「无策略」都没有 autopilot 策略。
                    let expected_policy = match (action, state) {
                        (Action::Off, _) | (_, CacheState::Unreachable) => None,
                        // `observe` 原样；`enforce` 只有新鲜的那一格真的拦截，
                        // 陈旧的那一格在上游的 `downgraded_when_stale` 里已降级。
                        (Action::Observe, _) | (Action::Enforce, CacheState::StaleGrace) => {
                            Some(QuotaAction::Observe)
                        }
                        (Action::Enforce, CacheState::Fresh) => Some(QuotaAction::Enforce),
                    };
                    assert_eq!(
                        via_provider.as_ref().map(|policy| policy.action),
                        expected_policy,
                        "{label}: autopilot 面的投影"
                    );
                    if let Some(policy) = via_provider.as_ref() {
                        assert_eq!(policy.limit, 20, "{label}: 降级只改 action，不改额度");
                        assert!(policy.period_start < policy.period_end, "{label}");
                        assert_eq!(policy.reset_at, policy.period_end, "{label}");
                        assert_eq!(policy.policy_revision, 5, "{label}");
                    }
                }

                // ---- 消费者投影 ②：issue-count 面（`/api/issues/limit-usage`）
                if gate == GateName::IssueCount {
                    let limit = issue_limit_from(
                        &mc_entitlement::client::provider().gate(workspace, GateName::IssueCount),
                    );
                    let expected_limit = match (action, state) {
                        (Action::Enforce, CacheState::Fresh) => Some(20),
                        _ => None,
                    };
                    assert_eq!(limit, expected_limit, "{label}: issue-count 面的投影");
                }

                cells += 1;
            }
        }
    }
    assert_eq!(cells, 18, "3 个 Action × 2 个 Gate × 3 个缓存态 = 18 格");

    // 两个消费者的**同格一致性**再钉一次（防「有人给两个面各写一份判定」）。
    for state in CacheState::ALL {
        let workspace = Id::new();
        arrange(&client, &clock, workspace, Action::Enforce, state);
        let from_plane = plane.issue_count(workspace);
        let from_slot = mc_entitlement::client::provider().gate(workspace, GateName::IssueCount);
        // ⚠️ 只比**判决**字段：`cloud_valid_until` 在 `off_decision` 里是
        // `Timestamp::default()`（= 此刻），两次调用天然差几纳秒（`types.rs` 冻结面）。
        assert_eq!(
            (
                from_plane.gate,
                from_plane.reason,
                from_plane.policy_revision,
                from_plane.subscription_version
            ),
            (
                from_slot.gate,
                from_slot.reason,
                from_slot.policy_revision,
                from_slot.subscription_version
            ),
            "{}: 适配器与已安装平面必须给同一份判决",
            state.label()
        );
        assert_eq!(
            plane.policy(workspace),
            mc_autopilot::quota::policy_for(workspace),
            "{}: 适配器与 autopilot 槽必须给同一份策略",
            state.label()
        );
    }

    // 🔴 替身面的判决**不得**驱动 issue-count 路由（模块头第 1 条禁令的运行时兑现）。
    let stub_decision = Decision {
        reason: Reason::Stub,
        ..off_decision(Reason::Stub)
    };
    assert_eq!(stub_decision.gate.action, Action::Off);
    assert_eq!(issue_limit_from(&stub_decision), None);

    // `start()` 的**合法基址**那一支：装机点确实会装、且装的就是已经装好的那个平面。
    let handles = start(&config_with(&[(mc_cloud_url(), "http://127.0.0.1:9999")]));
    assert!(handles.is_configured());
    // 进程级 `OnceLock` 已经在本用例里占位 ⇒ 本次 `start` 是 no-op，`wired` 诚实地为假。
    assert!(
        !handles.is_wired(),
        "第二次 install 必须是 no-op 而不是谎称装上"
    );
    assert!(handles.plane().is_some());
    handles.shutdown().await;
}

// ---------------------------------------------------------------------------
// 出站往返（真 axum 服务；本文件里**唯一**用到 tokio 运行时的地方）
// ---------------------------------------------------------------------------

/// 起一个本地策略端点；返回基址与（可选的）请求计数。
async fn policy_endpoint(body: serde_json::Value, status: u16) -> (String, Arc<AtomicI64>) {
    use axum::routing::get;
    let hits = Arc::new(AtomicI64::new(0));
    let counter = Arc::clone(&hits);
    let app = axum::Router::new().route(
        "/api/v1/internal/entitlement-policies/:workspace",
        get(move || {
            let counter = Arc::clone(&counter);
            let rendered = body.to_string();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let code =
                    axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK);
                let mut response = axum::response::Response::new(axum::body::Body::from(rendered));
                *response.status_mut() = code;
                response.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                response
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

fn valid_policy_body(limit: i64, version: i64) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "policy_revision": 2,
        "subscription_version": version,
        "valid_until": at(9_000_000).as_iso(),
        "valid_for_seconds": 300,
        "gates": {
            "issue_count": {"action": "enforce", "limit": limit},
            "autopilot_runs": {
                "action": "enforce", "limit": limit,
                "period_start": at(1_000).as_iso(),
                "period_end": at(2_000).as_iso(),
                "reset_at": at(2_000).as_iso()
            }
        }
    })
}

/// 端点只按 `workspace` 这**一个**路径段定位 ⇒ 请求里不得出现第二处租户维度。
#[tokio::test]
async fn refresh_fetches_normalizes_and_fills_the_cache() {
    let clock = TestClock::at(1_000_000);
    let (base, hits) = policy_endpoint(valid_policy_body(7, 4), 200).await;
    let client = Arc::new(
        Client::from_validated_base_url(&base, None)
            .expect("client")
            .with_clock(clock.as_clock()),
    );
    let plane = EntitlementPlane::new(Arc::clone(&client));
    let workspace = Id::new();

    // 需求驱动的兑现：`gate()` 记一笔，刷新器一轮把它变成一份可用的策略。
    assert_eq!(plane.issue_count(workspace).reason, Reason::Unavailable);
    assert_eq!(client.pending_demands(), 1);
    plane.serve_demands().await;
    assert_eq!(client.pending_demands(), 0);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let decision = plane.issue_count(workspace);
    assert_eq!(decision.reason, Reason::CacheFresh);
    assert!(decision.is_enforcing());
    assert_eq!(decision.gate.limit, Some(7));
    assert_eq!(decision.policy_revision, 2);
    assert_eq!(decision.subscription_version, 4);
    assert_eq!(
        plane.policy(workspace).map(|policy| policy.limit),
        Some(7),
        "同一个平面必须让两个消费者都拿到这份策略"
    );

    // 单飞：同一工作区已经在飞时不重复发请求。
    client.request_refresh(workspace);
    let first = client.refresh(workspace).await;
    assert!(matches!(
        first,
        RefreshOutcome::Ok | RefreshOutcome::Error | RefreshOutcome::Request
    ));
    assert_eq!(client.in_flight_count(), 0, "刷新结束必须清掉在飞标记");
}

#[tokio::test]
async fn unreachable_endpoint_fails_open_and_never_claims_a_policy() {
    let clock = TestClock::at(1_000_000);
    let client = offline_client(&clock);
    let plane = EntitlementPlane::new(Arc::clone(&client));
    let workspace = Id::new();

    let outcome = client.refresh(workspace).await;
    assert_eq!(outcome, RefreshOutcome::Network, "连不上 ⇒ network");
    let decision = plane.issue_count(workspace);
    assert_eq!(decision.reason, Reason::Unavailable);
    assert_eq!(decision.gate, Gate::off());
    assert_eq!(
        plane.policy(workspace),
        None,
        "绝不为不存在的策略编一个出来"
    );
    assert_eq!(issue_limit_from(&decision), None);
    // 失败也推进了退避 ⇒ 下一次 `gate()` 不会立刻再记需求。
    assert_eq!(client.pending_demands(), 0);
}

#[tokio::test]
async fn a_rejecting_endpoint_is_reported_and_the_previous_snapshot_survives() {
    let clock = TestClock::at(1_000_000);
    let (base, _hits) = policy_endpoint(json!({"error": "nope"}), 503).await;
    let client = Arc::new(
        Client::from_validated_base_url(&base, None)
            .expect("client")
            .with_clock(clock.as_clock()),
    );
    let workspace = Id::new();
    assert_eq!(client.refresh(workspace).await, RefreshOutcome::Status5xx);
    assert_eq!(
        client.gate(workspace, GateName::IssueCount).reason,
        Reason::Unavailable
    );

    // 版本回退：新会话的 subscription_version 更小 ⇒ 拒绝写入、保留旧的。
    let first = mc_entitlement::client::FetchedPolicy {
        snapshot: snapshot_for_action(Action::Enforce, 20),
        valid_for: MAX_POLICY_TTL,
    };
    assert_eq!(
        client.store(workspace, &first),
        mc_entitlement::cache::PutOutcome::Stored
    );
    let older = mc_entitlement::client::FetchedPolicy {
        snapshot: PolicySnapshot {
            subscription_version: 1,
            ..snapshot_for_action(Action::Enforce, 99)
        },
        valid_for: MAX_POLICY_TTL,
    };
    assert_eq!(
        client.store(workspace, &older),
        mc_entitlement::cache::PutOutcome::VersionRegression
    );
    let decision = client.gate(workspace, GateName::IssueCount);
    assert_eq!(decision.gate.limit, Some(20), "回退的写入不得覆盖旧快照");
    assert!(decision.is_enforcing());

    // 过期的宽限窗口：宽限期一过，当前回话可以恢复（避免回滚后永远刷不出来）。
    clock.advance_secs(i64::try_from((MAX_POLICY_TTL + STALE_GRACE).as_secs()).expect("secs") + 60);
    assert_eq!(
        client.store(workspace, &older),
        mc_entitlement::cache::PutOutcome::Stored,
        "过了陈旧窗口，守卫失效"
    );
}

#[tokio::test]
async fn an_oversized_or_malformed_body_is_invalid_policy_and_echoes_nothing() {
    let clock = TestClock::at(1_000_000);
    let huge = "x".repeat(70 * 1024);
    let (base, _hits) = policy_endpoint(json!({ "padding": huge }), 200).await;
    let client = Arc::new(
        Client::from_validated_base_url(&base, None)
            .expect("client")
            .with_clock(clock.as_clock()),
    );
    let workspace = Id::new();
    assert_eq!(
        client.refresh(workspace).await,
        RefreshOutcome::InvalidPolicy,
        "体超 64 KiB ⇒ invalid_policy"
    );
    assert_eq!(
        client.gate(workspace, GateName::IssueCount).reason,
        Reason::Unavailable
    );

    let (base, _hits) = policy_endpoint(json!({ "schema_version": 1 }), 200).await;
    let client = Arc::new(
        Client::from_validated_base_url(&base, None)
            .expect("client")
            .with_clock(clock.as_clock()),
    );
    assert_eq!(
        client.refresh(workspace).await,
        RefreshOutcome::InvalidPolicy,
        "缺两个 gate ⇒ invalid_policy"
    );
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let clock = TestClock::at(1_000_000);
    // 目标：一个**会**给出合法策略的端点。
    let (target, target_hits) = policy_endpoint(valid_policy_body(7, 4), 200).await;
    let app = axum::Router::new().route(
        "/api/v1/internal/entitlement-policies/:workspace",
        axum::routing::get(move || {
            let location = target.clone();
            async move {
                let mut response = axum::response::Response::new(axum::body::Body::empty());
                *response.status_mut() = axum::http::StatusCode::FOUND;
                if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
                    response
                        .headers_mut()
                        .insert(axum::http::header::LOCATION, value);
                }
                response
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = Arc::new(
        Client::from_validated_base_url(&format!("http://{addr}"), None)
            .expect("client")
            .with_clock(clock.as_clock()),
    );
    let workspace = Id::new();
    let outcome = client.refresh(workspace).await;
    assert_eq!(
        outcome,
        RefreshOutcome::Status,
        "302 原样当状态失败，不跟随"
    );
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "绝不许跟到别的源");
    assert_eq!(
        client.gate(workspace, GateName::IssueCount).reason,
        Reason::Unavailable
    );
}

/// 替身面能装进 `install_policy_provider` 的实参形态（`DoD` 第 6 条）：
/// `Stub` 是 `Provider`，经适配器即可成为 `QuotaPolicyProvider`。
#[test]
fn the_stub_is_usable_as_a_test_double_for_the_installed_plane() {
    let stub = mc_entitlement::stub::Stub::shared();
    stub.set(
        Id::new(),
        GateName::AutopilotRuns,
        Decision {
            gate: gate_with_period(Action::Enforce, 3, at(1_000), at(2_000)),
            reason: Reason::Stub,
            policy_revision: 1,
            subscription_version: 1,
            cloud_valid_until: at(9_000),
        },
    );
    let stub = Arc::new(StubPlane { inner: stub });
    // 形态检查：能当 `Arc<dyn QuotaPolicyProvider>` 用（编译期）+ 投影正确（运行期）。
    let as_provider: Arc<dyn QuotaPolicyProvider> = stub;
    let workspace = Id::new();
    assert_eq!(
        as_provider.policy(workspace),
        None,
        "没设过的工作区仍 fail-open"
    );
}

/// 借真实 `Stub` 装一个平面（只在本文件内可见）。
struct StubPlane {
    inner: Arc<mc_entitlement::stub::Stub>,
}

impl QuotaPolicyProvider for StubPlane {
    fn policy(&self, workspace_id: Id) -> Option<QuotaPolicy> {
        quota_policy_from(&self.inner.gate(workspace_id, GateName::AutopilotRuns))
    }
}
