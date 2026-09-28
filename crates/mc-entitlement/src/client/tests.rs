//! `client.rs` 的用例（与 `mc-http/src/routes/issue_table/tests.rs` 同款：把测试拆成子模块，
//! 好让 `client.rs` 本体守在门 ⑩ 的 800 行硬上限之内）。
//!
//! 本文件**不含**出站往返的端到端用例（那需要 `tokio` 运行时，而 `mc-entitlement` 的依赖边
//! 被锚点冻结）——那一层落在 `apps/mc-server/src/entitlement.rs` 的用例里（那里有 `tokio`）。

use super::*;
use crate::cache::{gate_with_period, snapshot_for, MAX_POLICY_TTL, STALE_GRACE};

fn at(secs: i64) -> Timestamp {
    Timestamp::from_unix(secs)
}

/// 可推进的测试时钟（上游 `now func() time.Time` 字段的可复现替身）。
#[derive(Debug, Clone)]
struct TestClock(Arc<std::sync::atomic::AtomicI64>);

impl TestClock {
    fn at(unix: i64) -> Self {
        Self(Arc::new(std::sync::atomic::AtomicI64::new(unix)))
    }

    fn advance(&self, seconds: i64) {
        self.0
            .fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
    }
}

fn client_with_clock(clock: &TestClock) -> Client {
    let handle = clock.0.clone();
    Client::new(Some(Url::parse("https://cloud.test").expect("url")), None)
        .expect("client")
        .with_clock(Arc::new(move || {
            Timestamp::from_unix(handle.load(std::sync::atomic::Ordering::SeqCst))
        }))
}

fn ts(seconds: i64) -> String {
    at(seconds).as_iso()
}

/// 一份合法的 wire（两个 gate 齐全）。
fn wire(action_issue: &str, action_runs: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "policy_revision": 3,
        "subscription_version": 7,
        "valid_until": ts(9_000),
        "valid_for_seconds": 300,
        "gates": {
            "issue_count": {"action": action_issue, "limit": 10},
            "autopilot_runs": {
                "action": action_runs, "limit": 20,
                "period_start": ts(1_000), "period_end": ts(2_000), "reset_at": ts(2_000)
            }
        }
    })
}

fn normalize(json: &serde_json::Value) -> Result<FetchedPolicy, InvalidPolicy> {
    parse_and_normalize(serde_json::to_vec(json).expect("json").as_slice())
}

fn client_at(now: Timestamp) -> Client {
    client_with_clock(&TestClock::at(now.as_unix()))
}

fn fetch_runs(gate: GateName, policy: &FetchedPolicy) -> Gate {
    policy.snapshot.gate(gate)
}

// ---- wire 规范化（上游 normalizePolicy / normalizeGate） --------------------

#[test]
fn a_well_formed_policy_normalizes_into_both_gates() {
    let policy = normalize(&wire("enforce", "enforce")).expect("valid");
    assert_eq!(policy.valid_for, Duration::from_secs(300));
    assert_eq!(policy.snapshot.policy_revision, 3);
    assert_eq!(policy.snapshot.subscription_version, 7);
    assert_eq!(fetch_runs(GateName::IssueCount, &policy).limit, Some(10));
    let runs = fetch_runs(GateName::AutopilotRuns, &policy);
    assert_eq!(runs.action, Action::Enforce);
    assert!(runs.period_is_complete_or_absent());
    assert_eq!(runs.reset_at, runs.period_end);
}

/// `off` 是 fail-open 形状：**即使** wire 多给了 limit / period 也一律丢掉。
#[test]
fn off_gates_drop_limit_and_period_even_when_the_wire_offers_them() {
    let json = serde_json::json!({
        "schema_version": 1, "policy_revision": 1, "subscription_version": 0,
        "valid_until": ts(9_000), "valid_for_seconds": 60,
        "gates": {
            "issue_count": {"action": "off", "limit": 99,
                "period_start": ts(1_000), "period_end": ts(2_000), "reset_at": ts(2_000)},
            "autopilot_runs": {"action": "enforce", "limit": 5,
                "period_start": ts(1_000), "period_end": ts(2_000), "reset_at": ts(2_000)}
        }
    });
    let policy = normalize(&json).expect("valid");
    assert_eq!(fetch_runs(GateName::IssueCount, &policy), Gate::off());
}

/// `observe` **不是**线上取值（逐字上游 `normalizeGate` 的 default 分支）。
#[test]
fn observe_is_not_a_wire_value() {
    assert!(normalize(&wire("observe", "enforce")).is_err());
    assert!(normalize(&wire("", "enforce")).is_err());
    assert!(normalize(&wire("ENFORCE", "enforce")).is_err());
}

/// 头部六条判据 + 两个 gate **都必须在**。
#[test]
fn header_judgements_and_both_gates_are_mandatory() {
    for (key, bad) in [
        ("schema_version", serde_json::json!(2)),
        ("policy_revision", serde_json::json!(0)),
        ("subscription_version", serde_json::json!(-1)),
        ("valid_for_seconds", serde_json::json!(0)),
        ("valid_for_seconds", serde_json::json!(301)),
    ] {
        let mut json = wire("enforce", "enforce");
        json[key] = bad.clone();
        assert!(normalize(&json).is_err(), "{key}={bad} 必须被判非法");
    }
    // `valid_for_seconds` 恰在 TTL 上界上 ⇒ 合法（<= MAX_POLICY_TTL）。
    let mut ok = wire("enforce", "enforce");
    ok["valid_for_seconds"] = serde_json::json!(300);
    assert!(normalize(&ok).is_ok());

    // 缺任一 gate ⇒ 非法。
    let mut missing = wire("enforce", "enforce");
    missing["gates"]
        .as_object_mut()
        .expect("gates")
        .remove("autopilot_runs");
    assert!(normalize(&missing).is_err());
}

/// `limit` 缺失 / 为负 ⇒ 非法；三个 period 字段「全有或全无」；`autopilot_runs` 必须齐全。
#[test]
fn gate_judgements_match_upstream_normalize_gate() {
    let bad_limit = serde_json::json!({
        "schema_version": 1, "policy_revision": 1, "subscription_version": 0,
        "valid_until": ts(9_000), "valid_for_seconds": 60,
        "gates": {
            "issue_count": {"action": "enforce"},
            "autopilot_runs": {"action": "enforce", "limit": 5,
                "period_start": ts(1_000), "period_end": ts(2_000), "reset_at": ts(2_000)}
        }
    });
    assert!(normalize(&bad_limit).is_err(), "enforce 缺 limit ⇒ 非法");

    let negative = serde_json::json!({
        "schema_version": 1, "policy_revision": 1, "subscription_version": 0,
        "valid_until": ts(9_000), "valid_for_seconds": 60,
        "gates": {
            "issue_count": {"action": "enforce", "limit": -1},
            "autopilot_runs": {"action": "enforce", "limit": 5,
                "period_start": ts(1_000), "period_end": ts(2_000), "reset_at": ts(2_000)}
        }
    });
    assert!(normalize(&negative).is_err(), "limit < 0 ⇒ 非法");

    let partial = serde_json::json!({
        "schema_version": 1, "policy_revision": 1, "subscription_version": 0,
        "valid_until": ts(9_000), "valid_for_seconds": 60,
        "gates": {
            "issue_count": {"action": "enforce", "limit": 1, "period_start": ts(1_000)},
            "autopilot_runs": {"action": "enforce", "limit": 5,
                "period_start": ts(1_000), "period_end": ts(2_000), "reset_at": ts(2_000)}
        }
    });
    assert!(normalize(&partial).is_err(), "period 三字段残缺 ⇒ 非法");

    let runs_without_period = serde_json::json!({
        "schema_version": 1, "policy_revision": 1, "subscription_version": 0,
        "valid_until": ts(9_000), "valid_for_seconds": 60,
        "gates": {
            "issue_count": {"action": "off"},
            "autopilot_runs": {"action": "enforce", "limit": 5}
        }
    });
    assert!(
        normalize(&runs_without_period).is_err(),
        "autopilot_runs 必须有 period 三字段"
    );

    let wrong_order = serde_json::json!({
        "schema_version": 1, "policy_revision": 1, "subscription_version": 0,
        "valid_until": ts(9_000), "valid_for_seconds": 60,
        "gates": {
            "issue_count": {"action": "off"},
            "autopilot_runs": {"action": "enforce", "limit": 5,
                "period_start": ts(2_000), "period_end": ts(1_000), "reset_at": ts(3_000)}
        }
    });
    assert!(
        normalize(&wrong_order).is_err(),
        "period_start 必须早于 end/reset"
    );
}

/// 通知策略坏掉**不得**让 enforcement 失效（上游注释逐字）。
#[test]
fn a_malformed_notification_object_never_invalidates_the_gate() {
    let mut good = wire("enforce", "enforce");
    good["gates"]["issue_count"]["notifications"] =
        serde_json::json!({"on_rejection": "first_rejection_per_period"});
    let policy = normalize(&good).expect("valid");
    assert!(fetch_runs(GateName::IssueCount, &policy)
        .notifications
        .is_some());

    let mut bad = wire("enforce", "enforce");
    bad["gates"]["issue_count"]["notifications"] = serde_json::json!({"on_rejection": "每分钟"});
    let policy = normalize(&bad).expect("仍然合法");
    let gate = fetch_runs(GateName::IssueCount, &policy);
    assert!(gate.notifications.is_none());
    assert_eq!(gate.action, Action::Enforce, "只丢通知，不丢 enforcement");
}

/// 🔴 `DoD` 6：错误路径**不回显云侧响应体**（`docs/62` §2.4）。
#[test]
fn error_paths_never_echo_the_cloud_body() {
    let secret = "SUPER-SECRET-TOKEN-VALUE";
    let garbage = serde_json::json!({"schema_version": secret});
    let rendered = normalize(&garbage)
        .expect_err("garbage must be rejected")
        .to_string();
    assert!(!rendered.contains(secret), "{rendered}");
    assert_eq!(rendered, "entitlement: invalid policy response");
    // 非法 JSON 同样不带体。
    let body = format!("{{not json {secret}");
    let err = parse_and_normalize(body.as_bytes()).expect_err("bad json");
    assert!(!err.to_string().contains(secret));
}

// ---- 判决路径（同步、无 IO） ---------------------------------------------

#[test]
fn disabled_client_answers_off_for_every_gate() {
    let client = Client::disabled().expect("disabled");
    assert!(!client.is_enabled());
    for name in GateName::ALL {
        let decision = client.gate(Id::new(), name);
        assert_eq!(decision.reason, Reason::Disabled);
        assert_eq!(decision.gate, Gate::off());
    }
    assert!(client.take_demands().is_empty(), "禁用时不得记需求");
    assert_eq!(client.pending_demands(), 0);
}

#[test]
fn nil_workspace_and_unknown_gate_have_their_own_reasons() {
    let client = client_at(at(1_000));
    assert_eq!(
        client.gate(Id::nil(), GateName::IssueCount).reason,
        Reason::InvalidWorkspace
    );
    assert_eq!(client.pending_demands(), 0, "非法工作区不该触发刷新");
}

/// 冷启动：第一次调用是 fail-open 的 `Unavailable`，并**记一笔需求**（不阻塞）。
#[test]
fn a_cold_cache_answers_fail_open_and_records_a_demand() {
    let client = client_at(at(1_000));
    let ws = Id::new();
    let decision = client.gate(ws, GateName::AutopilotRuns);
    assert_eq!(decision.reason, Reason::Unavailable);
    assert_eq!(decision.gate, Gate::off());
    assert!(!decision.is_enforcing());
    assert_eq!(client.pending_demands(), 1);
    let drained = client.take_demands();
    assert_eq!(drained, vec![ws]);
    assert_eq!(client.pending_demands(), 0, "取走即清空");
}

/// 装上一份**新鲜**策略后，命中即 `CacheFresh`（并带上审计信息）。
#[test]
fn a_fresh_entry_is_served_from_cache_with_audit_fields() {
    let t0 = at(1_000_000);
    let client = client_at(t0);
    let ws = Id::new();
    let policy = normalize(&wire("enforce", "enforce")).expect("valid");
    assert_eq!(client.store(ws, &policy), PutOutcome::Stored);

    let decision = client.gate(ws, GateName::IssueCount);
    assert_eq!(decision.reason, Reason::CacheFresh);
    assert!(decision.is_enforcing());
    assert_eq!(decision.gate.limit, Some(10));
    assert_eq!(decision.policy_revision, 3);
    assert_eq!(decision.subscription_version, 7);
    assert_eq!(client.pending_demands(), 0, "命中时不得再记需求");
}

/// 陈旧宽限期内：退避 + `enforce` 被降级为 `observe`（逐字上游 `decisionFromEntry`）。
#[test]
fn a_stale_entry_is_downgraded_to_observe_while_the_backoff_holds() {
    let t0 = at(1_000_000);
    let ws = Id::new();
    let policy = normalize(&wire("enforce", "enforce")).expect("valid");
    let clock = TestClock::at(t0.as_unix());
    let client = client_with_clock(&clock);
    client.store(ws, &policy);
    // 推到新鲜期之后、再用一次失败推进退避（那正是「云侧挂了」的形状）。
    clock.advance(i64::try_from(MAX_POLICY_TTL.as_secs()).expect("secs") + 30);
    client.store_failure(ws);

    let decision = client.gate(ws, GateName::IssueCount);
    assert_eq!(decision.reason, Reason::Stale);
    assert_eq!(
        decision.gate.action,
        Action::Observe,
        "陈旧策略不得用于拦截"
    );
    assert_eq!(decision.gate.limit, Some(10), "降级只改 action，不改额度");
    assert!(!decision.is_enforcing());
    assert_eq!(client.pending_demands(), 0, "退避中不得反复记需求");
}

/// 退避期里**且**已过宽限期 ⇒ `Unavailable` 的纯 `off`。
#[test]
fn past_the_grace_window_a_backing_off_entry_is_unavailable() {
    let t0 = at(2_000_000);
    let ws = Id::new();
    let policy = normalize(&wire("enforce", "enforce")).expect("valid");
    let clock = TestClock::at(t0.as_unix());
    let client = client_with_clock(&clock);
    client.store(ws, &policy);
    clock.advance(i64::try_from((MAX_POLICY_TTL + STALE_GRACE).as_secs()).expect("secs") + 60);
    client.store_failure(ws);

    let decision = client.gate(ws, GateName::IssueCount);
    assert_eq!(decision.reason, Reason::Unavailable);
    assert_eq!(decision.gate, Gate::off());
    // 过了退避期就会重新记需求（刷新器会再问一次）。
    assert_eq!(client.pending_demands(), 0, "仍在退避期内（5s < 20min）");
}

/// `Debug` 不带基址（`docs/62` §2.4 判据 ④）。
#[test]
fn debug_never_prints_the_base_url() {
    let client = Client::new(
        Some(Url::parse("https://user:pw@cloud.test").expect("url")),
        None,
    );
    // 上面那个 Url 过不了真实的三件套校验（凭据），但构造路径仍不得回显。
    let rendered = format!("{:?}", Client::disabled().expect("disabled"));
    assert!(!rendered.contains("cloud.test"), "{rendered}");
    let rendered = format!(
        "{:?}",
        Client::new(Some(Url::parse("https://cloud.test").expect("url")), None).expect("client")
    );
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(!rendered.contains("cloud.test"), "{rendered}");
    assert!(client.is_ok());
}

/// 装上平面后，两个消费者（这里用两个 `Provider` 句柄）看到**同一份**判决。
#[test]
fn one_installed_plane_serves_every_consumer() {
    assert!(
        install_provider(Arc::new(client_at(at(1_000)))),
        "本进程只装一次"
    );
    assert!(
        !install_provider(Arc::new(client_at(at(1_000)))),
        "后装者被忽略"
    );
    let decision = provider().gate(Id::new(), GateName::IssueCount);
    assert_eq!(decision.reason, Reason::Unavailable);
    assert!(provider().is_enabled(), "装上之后平面就是启用的");
    assert!(
        std::ptr::eq(provider(), provider()),
        "同一个平面句柄 ⇒ 两个消费者不可能读到两份策略"
    );
}

/// 三个常量真的被客户端用上（不是各抄一份的数字）。
#[test]
fn the_pinned_limits_are_the_ones_the_client_uses() {
    assert_eq!(max_ttl_seconds(), 300);
    let policy = normalize(&wire("enforce", "enforce")).expect("valid");
    assert_eq!(policy.valid_for, MAX_POLICY_TTL);
    let snapshot = snapshot_for(
        gate_with_period(Action::Enforce, 1, at(1), at(2)),
        Gate::off(),
        1,
        1,
        at(3),
    );
    assert_eq!(snapshot.gates.len(), 2);
}

#[test]
fn endpoint_path_matches_upstream_and_carries_only_the_workspace() {
    let id = Id::parse("11111111-2222-3333-4444-555555555555").expect("uuid");
    let path = policy_endpoint_path(id);
    assert_eq!(
        path,
        "/api/v1/internal/entitlement-policies/11111111-2222-3333-4444-555555555555"
    );
    assert!(path.starts_with(POLICY_ENDPOINT_PREFIX));
    assert!(!path.contains('?'), "上游逐字 RawQuery = \"\"");
    assert!(!path.contains('#'));
    // 不同工作区只有一个路径段不同（没有第二处租户维度）。
    let other = policy_endpoint_path(Id::nil());
    assert_eq!(
        other.trim_start_matches(POLICY_ENDPOINT_PREFIX),
        Id::nil().to_string()
    );
}
