//! 协议客户端 / 会话表 / 三态状态机的断言（`registration/tests.rs` 的续篇）。
//!
//! 装置（`FakePoster` / `FakeApi` / `FakeRegistrationStore` / `Harness`）留在父模块，
//! 于是 `backfill` 的用例与这里共用**同一个**定义点。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone as _, Utc};
use pretty_assertions::assert_eq;

use super::{
    harness, harness_with, id, poll_success_body, FakeApi, FakePoster, FakeRegistrationStore,
    Harness, PersistVerdict, CLIENT_SECRET,
};
use crate::lark::registration::{
    bot_name_preset, decorate_qr_code_url, form_body, percent_encode, reason,
    InstallSessionOutcome, InstallSessionState, InstallSessionStore, MemoryInstallSessionStore,
    PollResult, RegistrationClient, RegistrationConfig, RegistrationError,
    RegistrationServiceConfig, SessionNotFound, SessionStatus, DEFAULT_EXPIRE_SECONDS,
    DEFAULT_FEISHU_ACCOUNTS_DOMAIN, DEFAULT_LARK_ACCOUNTS_DOMAIN, DEFAULT_POLL_SECONDS,
    REGISTRATION_ENDPOINT,
};
use crate::lark::types::Region;

// =====================================================================
// 协议客户端
// =====================================================================

#[test]
fn form_body_percent_encodes_like_go() {
    assert_eq!(form_body(&[("action", "begin")]), "action=begin");
    assert_eq!(percent_encode("a b"), "a+b");
    assert_eq!(percent_encode("a/b"), "a%2Fb");
    assert_eq!(percent_encode("a~-_.b"), "a~-_.b");
}

#[test]
fn qr_url_carries_the_sdk_telemetry_params() {
    let url = decorate_qr_code_url(
        "https://accounts.feishu.cn/q?code=abc",
        "multica",
        "Bot - Multica",
    )
    .expect("合法 URL");
    assert!(url.contains("from=sdk"), "{url}");
    assert!(url.contains("tp=sdk"), "{url}");
    assert!(url.contains("source=go-sdk%2Fmultica"), "{url}");
    assert!(url.contains("name=Bot+-+Multica"), "{url}");
    assert!(
        url.starts_with("https://accounts.feishu.cn/q?code=abc&"),
        "{url}"
    );
}

#[test]
fn qr_url_rejects_a_non_url() {
    assert!(decorate_qr_code_url("not a url", "multica", "").is_err());
}

#[test]
fn bot_name_preset_degrades_without_a_dangling_dash() {
    assert_eq!(bot_name_preset("Ops"), "Ops - Multica");
    assert_eq!(bot_name_preset("   "), "Multica");
}

#[tokio::test]
async fn begin_parses_the_envelope_and_defaults_the_absent_fields() {
    // 只给 device_code + QR：interval / expires_in 走默认（5s / 600s）。
    let poster = FakePoster::with_begin(
        r#"{"device_code":"dc_1","verification_uri_complete":"https://accounts.lark.test/q?c=1"}"#,
    );
    let client = RegistrationClient::new(
        RegistrationConfig {
            domain: "https://accounts.feishu.test".to_string(),
            lark_domain: "https://accounts.lark.test".to_string(),
            source: "multica".to_string(),
        },
        Arc::new(poster),
    );
    let begin = client
        .begin("Ops - Multica", Region::Lark)
        .await
        .expect("begin");
    assert_eq!(begin.device_code, "dc_1");
    assert_eq!(begin.domain, "https://accounts.lark.test");
    assert_eq!(begin.interval, Duration::from_secs(DEFAULT_POLL_SECONDS));
    assert_eq!(
        begin.expires_in,
        Duration::from_secs(DEFAULT_EXPIRE_SECONDS)
    );
}

#[tokio::test]
async fn begin_surfaces_platform_errors() {
    let poster = FakePoster::with_begin(r#"{"error":"invalid_app","error_description":"nope"}"#);
    let client = RegistrationClient::new(RegistrationConfig::default(), Arc::new(poster));
    let error = client
        .begin("", Region::Feishu)
        .await
        .expect_err("必须失败");
    assert_eq!(error.code, "invalid_app");
    assert!(!error.to_string().contains("secret"), "{error}");
}

#[tokio::test]
async fn begin_rejects_a_response_without_a_qr_target() {
    let poster = FakePoster::with_begin(r#"{"device_code":"dc_1"}"#);
    let client = RegistrationClient::new(RegistrationConfig::default(), Arc::new(poster));
    assert_eq!(
        client
            .begin("", Region::Feishu)
            .await
            .expect_err("必须失败")
            .code,
        "invalid_response"
    );
}

#[tokio::test]
async fn poll_branches_on_every_documented_signal() {
    let poster = FakePoster::with_polls(vec![
        r#"{"error":"authorization_pending"}"#,
        r#"{"error":"slow_down"}"#,
        r#"{"error":"access_denied","error_description":"user said no"}"#,
        r#"{"error":"expired_token"}"#,
        r#"{"client_id":"cli_new","client_secret":"S","user_info":{"open_id":"ou_1"}}"#,
        r"{}",
    ]);
    let client = RegistrationClient::new(RegistrationConfig::default(), Arc::new(poster));

    let pending = client.poll("", "dc").await.expect("poll");
    assert_eq!(pending.status, "authorization_pending");
    assert!(!pending.is_success() && !pending.is_switch());

    let slow = client.poll("", "dc").await.expect("poll");
    assert_eq!(slow.status, "slow_down");

    let denied = client.poll("", "dc").await.expect("poll");
    assert_eq!(denied.error.as_ref().expect("错误").code, "access_denied");

    let expired = client.poll("", "dc").await.expect("poll");
    assert_eq!(expired.error.as_ref().expect("错误").code, "expired_token");

    let ok = client.poll("", "dc").await.expect("poll");
    assert!(ok.is_success());
    assert_eq!(ok.client_secret, "S");
    assert_eq!(ok.open_id.as_ref().expect("open_id").as_str(), "ou_1");

    // 空 error + 空凭据 = 继续轮询（上游对 authorize 重定向窗口的宽容处理）。
    let blank = client.poll("", "dc").await.expect("poll");
    assert_eq!(blank.status, "authorization_pending");
}

#[tokio::test]
async fn poll_rejects_a_half_populated_success() {
    let poster = FakePoster::with_polls(vec![r#"{"client_id":"cli_new","client_secret":"S"}"#]);
    let client = RegistrationClient::new(RegistrationConfig::default(), Arc::new(poster));
    let error = client.poll("", "dc").await.expect_err("必须失败");
    assert_eq!(error.code, "invalid_response");
}

#[tokio::test]
async fn poll_switches_cloud_in_both_directions_and_only_once() {
    let poster = FakePoster::with_polls(vec![
        // 在飞书主机上扫到了国际账号 ⇒ 改道 larksuite。
        r#"{"user_info":{"tenant_brand":"lark"}}"#,
        // 在已改道的 larksuite 主机上再看到同一档 ⇒ **不**再改道（否则会打转）。
        r#"{"user_info":{"tenant_brand":"lark"}}"#,
    ]);
    let client = RegistrationClient::new(RegistrationConfig::default(), Arc::new(poster));

    let switched = client
        .poll(DEFAULT_FEISHU_ACCOUNTS_DOMAIN, "dc")
        .await
        .expect("poll");
    assert_eq!(switched.switched_domain, DEFAULT_LARK_ACCOUNTS_DOMAIN);
    assert_eq!(switched.switched_region, Some(Region::Lark));

    let quiet = client
        .poll(DEFAULT_LARK_ACCOUNTS_DOMAIN, "dc")
        .await
        .expect("poll");
    assert!(!quiet.is_switch(), "同一档不该再次改道");

    // 反向：在 larksuite 上扫到了大陆账号。
    let reverse_poster = FakePoster::with_polls(vec![r#"{"user_info":{"tenant_brand":"feishu"}}"#]);
    let reverse = RegistrationClient::new(RegistrationConfig::default(), Arc::new(reverse_poster));
    let switched = reverse
        .poll(DEFAULT_LARK_ACCOUNTS_DOMAIN, "dc")
        .await
        .expect("poll");
    assert_eq!(switched.switched_domain, DEFAULT_FEISHU_ACCOUNTS_DOMAIN);
    assert_eq!(switched.switched_region, Some(Region::Feishu));
}

#[tokio::test]
async fn poll_refuses_an_empty_device_code_before_any_network_call() {
    let poster = Arc::new(FakePoster::default());
    let client = RegistrationClient::new(RegistrationConfig::default(), poster.clone());
    assert_eq!(
        client.poll("", "").await.expect_err("必须失败").code,
        "invalid_argument"
    );
    assert_eq!(poster.calls.load(Ordering::SeqCst), 0);
}

// =====================================================================
// 会话表
// =====================================================================

#[tokio::test]
async fn session_store_is_workspace_scoped_and_drops_expired_entries() {
    let clock = Arc::new(AtomicI64::new(0));
    let session_now = {
        let clock = clock.clone();
        Arc::new(move || {
            Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间")
                + chrono::Duration::seconds(clock.load(Ordering::SeqCst))
        }) as Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>
    };
    let store = MemoryInstallSessionStore::with_clock(session_now);
    let state = InstallSessionState {
        id: "sess_1".to_string(),
        workspace_id: id(2),
        initiator_id: id(4),
        status: SessionStatus::Pending,
        installation_id: None,
        error_reason: String::new(),
        error_message: String::new(),
        expires_at: Utc.timestamp_opt(1_700_003_600, 0).single().expect("时间"),
    };
    store
        .create(state.clone(), Duration::from_secs(60))
        .await
        .expect("create");

    assert!(store.get(id(2), "sess_1").await.is_ok());
    // 别的 workspace 猜 id ⇒ 与不存在同一个错误（不可区分）。
    assert_eq!(store.get(id(9), "sess_1").await, Err(SessionNotFound));
    assert_eq!(store.get(id(2), "nope").await, Err(SessionNotFound));

    // 走完保活窗口 ⇒ 记录出队（不是"陈旧"）。
    clock.store(61, Ordering::SeqCst);
    assert_eq!(store.get(id(2), "sess_1").await, Err(SessionNotFound));
}

#[tokio::test]
async fn mark_terminal_is_first_writer_wins_and_moves_retention() {
    let clock = Arc::new(AtomicI64::new(0));
    let now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync> = {
        let clock = clock.clone();
        Arc::new(move || {
            Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间")
                + chrono::Duration::seconds(clock.load(Ordering::SeqCst))
        })
    };
    let store = MemoryInstallSessionStore::with_clock(now);
    store
        .create(
            InstallSessionState {
                id: "sess_1".to_string(),
                workspace_id: id(2),
                initiator_id: id(4),
                status: SessionStatus::Pending,
                installation_id: None,
                error_reason: String::new(),
                error_message: String::new(),
                expires_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
            },
            Duration::from_secs(10),
        )
        .await
        .expect("create");

    store
        .mark_terminal(
            "sess_1",
            InstallSessionOutcome {
                status: SessionStatus::Success,
                installation_id: Some(id(7)),
                error_reason: String::new(),
                error_message: String::new(),
            },
            Duration::from_secs(600),
        )
        .await
        .expect("mark");

    // 第二次终态写**不动**已经记录的结局，但保活窗口照移（与 Redis 脚本的无条件 EXPIRE 对齐）。
    store
        .mark_terminal(
            "sess_1",
            InstallSessionOutcome {
                status: SessionStatus::Error,
                installation_id: None,
                error_reason: reason::EXPIRED.to_string(),
                error_message: "too late".to_string(),
            },
            Duration::from_secs(600),
        )
        .await
        .expect("mark");

    let state = store.get(id(2), "sess_1").await.expect("get");
    assert_eq!(state.status, SessionStatus::Success);
    assert_eq!(state.installation_id, Some(id(7)));

    clock.store(20, Ordering::SeqCst);
    assert!(
        store.get(id(2), "sess_1").await.is_ok(),
        "retention 必须已经移到终态窗口上"
    );
    assert_eq!(
        store
            .mark_terminal(
                "gone",
                InstallSessionOutcome {
                    status: SessionStatus::Error,
                    installation_id: None,
                    error_reason: String::new(),
                    error_message: String::new(),
                },
                Duration::from_secs(1),
            )
            .await,
        Err(SessionNotFound)
    );
}

// =====================================================================
// 会话状态机：三态
// =====================================================================

#[tokio::test]
async fn begin_install_registers_a_pending_session_and_returns_the_qr() {
    let h = harness(
        FakePoster::with_polls(vec![r#"{"error":"authorization_pending"}"#]),
        FakeApi::serving("ou_bot", ""),
        FakeRegistrationStore::with_agent("Ops"),
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");
    assert!(!begin.session_id.is_empty());
    assert!(begin.qr_code_url.contains("from=sdk"));
    assert_eq!(begin.poll_interval_seconds, 1);
    assert_eq!(begin.expires_in_seconds, 3600);

    let state = h
        .service
        .get_session(id(2), &begin.session_id)
        .await
        .expect("get");
    assert_eq!(state.status, SessionStatus::Pending);
    assert_eq!(state.workspace_id, id(2));
    assert_eq!(state.initiator_id, id(4));
}

#[tokio::test]
async fn begin_install_refuses_an_agent_outside_the_workspace() {
    let h = harness(
        FakePoster::with_polls(vec![]),
        FakeApi::serving("ou_bot", ""),
        FakeRegistrationStore::default(),
    );
    let error = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect_err("必须失败");
    assert_eq!(error.code, "invalid_argument");
    // 一个会话都没登记 ⇒ 浏览器不会拿到一张永远读不到的 QR。
    assert!(h.sessions.get(id(2), "anything").await.is_err());
}

#[tokio::test]
async fn terminal_state_success_commits_the_installation_and_binds_the_installer() {
    let h = harness(
        FakePoster::with_polls(vec![&poll_success_body("ou_installer")]),
        FakeApi::serving("ou_bot", "on_bot"),
        FakeRegistrationStore::with_agent("Ops"),
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");

    let state = wait_for_terminal(&h, &begin.session_id).await;
    assert_eq!(state.status, SessionStatus::Success);
    assert_eq!(state.installation_id, Some(id(7)));

    // `begin` 与 `poll` 都落到了配置的那台主机上（换云判定要靠这个基）。
    let urls = h.poster.seen_urls.lock().expect("锁").clone();
    assert!(
        urls.iter().all(|url| url.ends_with(REGISTRATION_ENDPOINT)),
        "{urls:?}"
    );
    assert!(
        urls[0].starts_with(DEFAULT_FEISHU_ACCOUNTS_DOMAIN),
        "默认应当在飞书主机上 begin: {urls:?}"
    );

    let commits = h.store.commits.lock().expect("锁").clone();
    assert_eq!(commits.len(), 1);
    let commit = &commits[0];
    assert_eq!(commit.app_id, "cli_new");
    assert_eq!(commit.workspace_id, id(2));
    assert_eq!(commit.agent_id, id(3));
    assert_eq!(commit.initiator_id, id(4));
    // 装出来的 Bot 与发起人是**两个**身份，两个都进了判决入参。
    assert_eq!(commit.bot_open_id.as_str(), "ou_bot");
    assert_eq!(commit.installer_open_id.as_str(), "ou_installer");
    assert_eq!(commit.bot_union_id, "on_bot");
    // 令牌缓存被叫忘掉了（重新注册会同一 app_id 换密钥）。
    assert_eq!(
        h.api.invalidations.lock().expect("锁").clone(),
        vec!["cli_new"]
    );
}

#[tokio::test]
async fn terminal_state_failure_is_access_denied() {
    let h = harness(
        FakePoster::with_polls(vec![r#"{"error":"access_denied"}"#]),
        FakeApi::serving("ou_bot", ""),
        FakeRegistrationStore::with_agent("Ops"),
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");

    let state = wait_for_terminal(&h, &begin.session_id).await;
    assert_eq!(state.status, SessionStatus::Error);
    assert_eq!(state.error_reason, reason::ACCESS_DENIED);
    assert!(
        h.store.commits.lock().expect("锁").is_empty(),
        "被拒的会话不该落库"
    );
}

#[tokio::test]
async fn terminal_state_expired_fires_before_any_poll_when_the_window_closed() {
    // 时钟推过 `expires_in` ⇒ 循环的第一件事就是"窗口已经关了"，
    // **不**发起任何 poll（上游 `context.WithDeadline` 的等价形态）。
    let clock = Arc::new(AtomicI64::new(0));
    let now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync> = {
        let clock = clock.clone();
        Arc::new(move || {
            Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间")
                + chrono::Duration::seconds(clock.load(Ordering::SeqCst))
        })
    };
    let config = RegistrationServiceConfig {
        now: now.clone(),
        ..RegistrationServiceConfig::default()
    };
    let h = harness_with(
        FakePoster::with_polls(vec![&poll_success_body("ou_installer")]),
        FakeApi::serving("ou_bot", ""),
        FakeRegistrationStore::with_agent("Ops"),
        config,
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");
    // 会话记下来了（expires_at = base + 3600），然后把时钟推过去。
    clock.store(3_700, Ordering::SeqCst);

    let state = wait_for_terminal(&h, &begin.session_id).await;
    assert_eq!(state.status, SessionStatus::Error);
    assert_eq!(state.error_reason, reason::EXPIRED);
    assert!(h.store.commits.lock().expect("锁").is_empty());
}

#[tokio::test]
async fn a_commit_conflict_is_recorded_as_installation_conflict() {
    let store = FakeRegistrationStore::with_agent("Ops");
    *store.verdict.lock().expect("锁") = Some(PersistVerdict::Conflict(
        crate::lark::installation::InstallError::OwnedBySameWorkspace,
    ));
    let h = harness(
        FakePoster::with_polls(vec![&poll_success_body("ou_installer")]),
        FakeApi::serving("ou_bot", ""),
        store,
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");
    let state = wait_for_terminal(&h, &begin.session_id).await;
    assert_eq!(state.error_reason, reason::INSTALLATION_CONFLICT);
}

#[tokio::test]
async fn a_store_failure_is_recorded_as_internal_error() {
    let store = FakeRegistrationStore::with_agent("Ops");
    *store.verdict.lock().expect("锁") =
        Some(PersistVerdict::Failing("sqlstate 08006".to_string()));
    let h = harness(
        FakePoster::with_polls(vec![&poll_success_body("ou_installer")]),
        FakeApi::serving("ou_bot", ""),
        store,
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");
    let state = wait_for_terminal(&h, &begin.session_id).await;
    assert_eq!(state.status, SessionStatus::Error);
    assert_eq!(state.error_reason, reason::INTERNAL_ERROR);
}

#[tokio::test]
async fn bot_info_failure_is_recorded_as_bot_info_failed() {
    let h = harness(
        FakePoster::with_polls(vec![&poll_success_body("ou_installer")]),
        FakeApi::failing(),
        FakeRegistrationStore::with_agent("Ops"),
    );
    let begin = h
        .service
        .begin_install(id(2), id(3), id(4), Region::Feishu)
        .await
        .expect("begin");
    let state = wait_for_terminal(&h, &begin.session_id).await;
    assert_eq!(state.status, SessionStatus::Error);
    assert_eq!(state.error_reason, reason::BOT_INFO_FAILED);
    assert!(
        !state.error_message.contains(CLIENT_SECRET),
        "错误路径回显了 client_secret: {}",
        state.error_message
    );
}

/// 轮询是后台协程 ⇒ 等它到终态（上限 5s；`interval` 是 1s）。
async fn wait_for_terminal(h: &Harness, session_id: &str) -> InstallSessionState {
    for _ in 0..500 {
        let state = h.service.get_session(id(2), session_id).await.expect("get");
        if state.status != SessionStatus::Pending {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("会话在 5s 内没有到终态");
}

// =====================================================================
// 凭据纪律
// =====================================================================

#[test]
fn registration_error_text_never_carries_the_client_secret() {
    let error = RegistrationError::new(
        "invalid_response",
        "success response missing installer open_id",
    );
    let rendered = format!("{error} / {error:?}");
    assert!(!rendered.contains(CLIENT_SECRET), "{rendered}");
    assert!(rendered.contains("invalid_response"), "{rendered}");
}

#[test]
fn poll_result_debug_is_the_protocol_shape() {
    // `PollResult` **可以**派生 Debug（它不是一个凭据类型；凭证本体在
    // `InstallationCredentials` / `AppSecret` 那边才有脱敏实现）——
    // 但状态机从不把它插进日志，这里只钉住"它没有意外变成别的形状"。
    let result = PollResult {
        status: "authorization_pending".to_string(),
        ..PollResult::default()
    };
    assert!(format!("{result:?}").contains("authorization_pending"));
}
