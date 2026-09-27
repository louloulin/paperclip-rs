//! `GET /api/config`（M10-4 / `LUM-2106`）的字段级用例。
//!
//! 上游判据来源：`server/internal/handler/config_test.go`（pin `90e0bdf`）的 19 个测试。
//! ⚠️ ⑨ 门里那 17 条 `config/*` fixture 的 `json_subset` **全是空对象**（`docs/64` §2.2 实测）
//! ⇒ 它们只证明"挂上了、200、JSON 对象"。**本文件**才是字段级判据。
//!
//! 两层，与 `probes/ready/tests.rs` 同款分层：
//!
//! | 层 | 驱动 | 覆盖面 |
//! | --- | --- | --- |
//! | **装配层**（1–12） | `AppConfig::from_env_with(注入闭包, …)` | 17 字段取值 + `omitempty` 行为 + URL 归一化 + 6 flag |
//! | **接线层**（13–18） | 真 router（`AppState`，库**不可达**） | 200 / 键集 / 匿名可读 / 4 条能力声明真的以键出现 / 不触库 |
//!
//! 为什么装配层用注入闭包而不是改进程 env：本仓 `rust-version = 1.80` / edition 2021，
//! `std::env::set_var` 在 2024 才是 unsafe，且并行测试改全局 env 本身就是竞态
//! （与 `state/integrations.rs::from_env_with` 同一判例）。
//!
//! 接线层用**不可达**的库 URL 装 `AppState`（`probes/ready/tests.rs` 与
//! `mc-conformance/src/harness.rs` 同款）：本路由一旦有人真去查库就会拿到连接错误，
//! 而不是静默通过 —— 这是"**不触库**"这条硬要求的可执行判据。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use super::AppConfig;
use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

/// 真 URL、**不可达**地址（`probes/ready/tests.rs::UNREACHABLE_DB` 同款形状）。
const UNREACHABLE_DB: &str = "postgres://config-probe:config-probe@127.0.0.1:1/config_probe";

/// 注入式 env：`(名字, 值)` 列表 ⇒ 「名字 → 值」查询函数（缺项 = 未设置）。
fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name: &str| {
        owned
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

/// 装配出的 `AppConfig` → JSON 对象（验的是**序列化后的键集**，`omitempty` 才判得到）。
fn as_json(pairs: &[(&str, &str)]) -> Value {
    serde_json::to_value(AppConfig::from_env_with(env(pairs), false)).expect("serialize")
}

// --------------------------------------------------------------------------- #
// 装配层
// --------------------------------------------------------------------------- #

/// 上游 `TestGetConfigIncludesRuntimeAuthConfig`（`config_test.go:54`）的字段级等价。
#[test]
fn includes_runtime_auth_and_analytics_config() {
    let cfg = as_json(&[
        ("ALLOW_SIGNUP", "false"),
        ("GOOGLE_CLIENT_ID", "google-client-id"),
        ("POSTHOG_API_KEY", "phc_test"),
        ("POSTHOG_HOST", "https://eu.i.posthog.com"),
        ("MULTICA_CDN_DOMAIN", "cdn.example.com"),
        ("MULTICA_DAEMON_SERVER_URL", ""),
        ("MULTICA_PUBLIC_URL", "https://api.example.com/"),
        ("MULTICA_APP_URL", "https://app.example.com/"),
    ]);
    assert_eq!(cfg["cdn_domain"], "cdn.example.com");
    assert_eq!(cfg["allow_signup"], false);
    assert_eq!(cfg["google_client_id"], "google-client-id");
    assert_eq!(cfg["posthog_key"], "phc_test");
    assert_eq!(cfg["posthog_host"], "https://eu.i.posthog.com");
    assert_eq!(cfg["analytics_environment"], "dev");
    // 未设 `DISABLE_WORKSPACE_CREATION` ⇒ false ⇒ omitempty ⇒ 键不出现。
    assert!(cfg.get("workspace_creation_disabled").is_none());
}

/// 上游 `TestGetConfigUsesDaemonServerURLOverride`（`config_test.go:109`）：三级回落链的
/// 第一级胜出，且 `normalizePublicURL` 去尾斜杠 + 去首尾空白。
#[test]
fn daemon_server_url_env_override_wins_and_is_normalized() {
    let cfg = as_json(&[
        (
            "MULTICA_DAEMON_SERVER_URL",
            " https://api.internal.example/// ",
        ),
        ("MULTICA_PUBLIC_URL", "https://hooks.example.com/"),
        ("MULTICA_APP_URL", "https://app.example.com/"),
    ]);
    assert_eq!(cfg["daemon_server_url"], "https://api.internal.example");
    assert_eq!(cfg["daemon_app_url"], "https://app.example.com");
}

/// 上游 `TestGetConfigUsesAppURLForSameOriginDaemonSetup`（`:159`）：server URL 缺省时
/// **回落成 `app_url`**（同源部署）。
#[test]
fn daemon_server_url_falls_back_to_app_url() {
    let cfg = as_json(&[
        ("MULTICA_DAEMON_SERVER_URL", ""),
        ("MULTICA_PUBLIC_URL", ""),
        ("MULTICA_APP_URL", "https://multica.internal.example/"),
    ]);
    assert_eq!(cfg["daemon_server_url"], "https://multica.internal.example");
    assert_eq!(cfg["daemon_app_url"], "https://multica.internal.example");
}

/// 上游 `TestGetConfigUsesFrontendOriginForSameOriginDaemonSetup`（`:184`）：
/// `MULTICA_APP_URL` 缺省时回落 `FRONTEND_ORIGIN`。
#[test]
fn daemon_app_url_falls_back_to_frontend_origin() {
    let cfg = as_json(&[
        ("MULTICA_DAEMON_SERVER_URL", ""),
        ("MULTICA_PUBLIC_URL", ""),
        ("MULTICA_APP_URL", ""),
        ("FRONTEND_ORIGIN", "https://multica.internal.example/"),
    ]);
    assert_eq!(cfg["daemon_server_url"], "https://multica.internal.example");
    assert_eq!(cfg["daemon_app_url"], "https://multica.internal.example");
}

/// 上游 `TestGetConfigOmitsCloudDaemonSetupWithoutPublicURL`（`:244`）：官方云只按**前端
/// 主机**判定；没配 `MULTICA_PUBLIC_URL` 也必须抑制（否则 UI 会拼出
/// `setup self-host --server-url https://multica.ai`，把后端指到前端）。
#[test]
fn official_cloud_suppresses_daemon_urls_without_public_url() {
    for pairs in [
        // FRONTEND_ORIGIN 形态
        vec![
            ("MULTICA_PUBLIC_URL", ""),
            ("MULTICA_APP_URL", ""),
            ("FRONTEND_ORIGIN", "https://multica.ai"),
        ],
        // MULTICA_APP_URL 形态（上游 config_test.go:271）
        vec![
            ("MULTICA_PUBLIC_URL", ""),
            ("MULTICA_APP_URL", "https://multica.ai"),
            ("FRONTEND_ORIGIN", ""),
        ],
        // 已配了 public URL 也要抑制（上游 config_test.go:210）
        vec![
            ("MULTICA_PUBLIC_URL", "https://api.multica.ai"),
            ("MULTICA_APP_URL", ""),
            ("FRONTEND_ORIGIN", "https://multica.ai"),
        ],
    ] {
        let cfg = as_json(&pairs);
        assert!(cfg.get("daemon_server_url").is_none(), "{pairs:?}");
        assert!(cfg.get("daemon_app_url").is_none(), "{pairs:?}");
    }
}

/// 上游 `TestURLHostEqualsCanonicalizesCommonHostForms`（`:296`）的六种形态逐字。
///
/// 判据要**双向**：官方云 ⇒ 两个 URL 键都不出现；非官方云 ⇒ `daemon_app_url` 出现。
/// 只判前者会让“`app_url` 为空 ⇒ 整条回落链短路”把整条用例变成空断言。
#[test]
fn official_cloud_detection_canonicalizes_host_forms() {
    // 官方云的等价写法（裸主机 / 带端口 / 尾点 / 大小写 / 带空白）全部要判成云。
    for raw in [
        "https://multica.ai",
        "multica.ai",
        "https://multica.ai:443",
        "https://multica.ai.",
        "  HTTPS://MULTICA.AI  ",
    ] {
        let cfg = as_json(&[("MULTICA_APP_URL", raw)]);
        assert!(
            cfg.get("daemon_app_url").is_none(),
            "{raw:?} must be the official cloud"
        );
        assert!(cfg.get("daemon_server_url").is_none(), "{raw:?}");
    }
    // 相邻主机**不得**被误判：它们必须照常发布 daemon URL。
    for raw in [
        "https://evil.example",
        "https://notmultica.ai",
        "https://multica.ai.evil.example",
        "https://api.multica.ai",
    ] {
        let cfg = as_json(&[("MULTICA_APP_URL", raw)]);
        assert_eq!(
            cfg["daemon_app_url"],
            raw.trim(),
            "{raw:?} is not the official cloud"
        );
    }
    // userinfo / 路径 / 查询串不影响主机判定。
    assert!(
        as_json(&[("MULTICA_APP_URL", "https://user@multica.ai/dash?x=1")])
            .get("daemon_app_url")
            .is_none()
    );
}

/// 上游 `TestGetConfigExposesWorkspaceCreationDisabled`（`:322`）+ **逐字**解析：
/// 只有 `"true"` 打开，`"TRUE"` / `"1"` / `"yes"` 都不算（上游是 `== "true"`，不是宽松解析）。
#[test]
fn workspace_creation_disabled_is_a_verbatim_true_comparison() {
    assert_eq!(
        as_json(&[("DISABLE_WORKSPACE_CREATION", "true")])["workspace_creation_disabled"],
        true
    );
    for raw in ["TRUE", "1", "yes", "false", ""] {
        assert!(
            as_json(&[("DISABLE_WORKSPACE_CREATION", raw)])
                .get("workspace_creation_disabled")
                .is_none(),
            "{raw:?} must not enable the flag"
        );
    }
}

/// 上游 `TestGetConfigExposesServerVersion`（`:350`）+ `…OmitsServerVersionOnOfficialCloud`
/// （`:388`）：`server_version` 只在自建版出现。
#[test]
fn server_version_is_suppressed_on_official_cloud_only() {
    let hosted = as_json(&[("MULTICA_APP_URL", "https://multica.self-hosted.example")]);
    assert_eq!(hosted["server_version"], env!("CARGO_PKG_VERSION"));
    let cloud = as_json(&[("MULTICA_APP_URL", "https://multica.ai")]);
    assert!(cloud.get("server_version").is_none());
}

/// `ANALYTICS_DISABLED` 的短路闸：三个 analytics 字段**一起**清空
/// （`analytics_environment` 此时是空串，**不是** `"dev"` —— 上游的 if 块整段不执行）。
#[test]
fn analytics_disabled_short_circuits_all_three_fields() {
    for raw in ["true", "1"] {
        let cfg = as_json(&[
            ("ANALYTICS_DISABLED", raw),
            ("POSTHOG_API_KEY", "phc_test"),
            ("ANALYTICS_ENVIRONMENT", "production"),
        ]);
        // 三个键**都出现**（无 omitempty），值都是空串。
        assert_eq!(cfg["posthog_key"], "", "ANALYTICS_DISABLED={raw:?}");
        assert_eq!(cfg["posthog_host"], "", "ANALYTICS_DISABLED={raw:?}");
        assert_eq!(
            cfg["analytics_environment"], "",
            "ANALYTICS_DISABLED={raw:?}"
        );
    }
    // 非短路值（如 `"0"` / `"false"` / 未设）照常装配。
    let on = as_json(&[
        ("ANALYTICS_DISABLED", "0"),
        ("POSTHOG_API_KEY", "phc_test"),
        ("ANALYTICS_ENVIRONMENT", "prod"),
    ]);
    assert_eq!(on["posthog_key"], "phc_test");
    assert_eq!(on["analytics_environment"], "production");
}

/// `posthog_host` 的缺省回填 + `analytics_environment` 的两级回落与归一化。
#[test]
fn posthog_host_defaults_and_analytics_environment_falls_back() {
    // 空 host + 非空 key ⇒ 回填 us.i.posthog.com（上游逐字）。
    let cfg = as_json(&[("POSTHOG_API_KEY", "phc_test")]);
    assert_eq!(cfg["posthog_host"], "https://us.i.posthog.com");
    // key 也空 ⇒ **不**回填（回填条件是 key 非空）。
    assert_eq!(as_json(&[])["posthog_host"], "");
    // `APP_ENV` 兜底 + 别名归一化。
    assert_eq!(
        as_json(&[("APP_ENV", "staging")])["analytics_environment"],
        "staging"
    );
    assert_eq!(
        as_json(&[("APP_ENV", "local")])["analytics_environment"],
        "dev"
    );
    // 无法识别的值 ⇒ 继续回落 `"dev"`（上游 `normalizeEnvironment` 返回 `""`）。
    assert_eq!(
        as_json(&[("ANALYTICS_ENVIRONMENT", "staging-ish")])["analytics_environment"],
        "dev"
    );
    // `ANALYTICS_ENVIRONMENT` 优先于 `APP_ENV`。
    let both = as_json(&[
        ("ANALYTICS_ENVIRONMENT", "production"),
        ("APP_ENV", "staging"),
    ]);
    assert_eq!(both["analytics_environment"], "production");
}

/// 6 个公开 flag 的发布规则（上游 `EvaluateFrontendPublicFlags` 逐字）。
#[test]
fn publishes_exactly_six_flags_with_the_upstream_defaults() {
    let cfg = as_json(&[]);
    let flags = cfg["feature_flags"]
        .as_object()
        .expect("feature_flags object");
    assert_eq!(flags.len(), 6, "got {:?}", flags.keys().collect::<Vec<_>>());
    for key in [
        "billing_workspace_subscriptions",
        "composio_mcp_apps",
        "plugins_v1",
    ] {
        assert_eq!(flags.get(key), Some(&Value::Bool(false)), "{key}");
    }
    for key in [
        "agents_agent_builder",
        "agents_skill_toggles",
        "settings_resource_labels",
    ] {
        assert_eq!(flags.get(key), Some(&Value::Bool(true)), "{key}");
    }
    // 必须**不**发布：两个退役键 + 一个不在 `frontendPublicFlags` 里的上游 flag。
    for absent in [
        "desktop_hang_stack_capture",
        "custom_issue_statuses",
        "triage_v1",
    ] {
        assert!(
            flags.get(absent).is_none(),
            "{absent} must not be published"
        );
    }
}

/// 上游 `TestGetConfigExposesEnabledPluginsV1Flag`（`:484`）：`FF_PLUGINS_V1` 打开门控键。
#[test]
fn gated_flags_follow_ff_env_override() {
    let cfg = as_json(&[("FF_PLUGINS_V1", "true"), ("FF_COMPOSIO_MCP_APPS", "on")]);
    assert_eq!(cfg["feature_flags"]["plugins_v1"], true);
    assert_eq!(cfg["feature_flags"]["composio_mcp_apps"], true);
    assert_eq!(
        cfg["feature_flags"]["billing_workspace_subscriptions"],
        false
    );
}

// --------------------------------------------------------------------------- #
// 接线层
// --------------------------------------------------------------------------- #

/// 装一个 `AppState`：库**不可达**，其余按 `probes/ready/tests.rs` 同款。
fn state() -> Arc<AppState> {
    let db = mc_db::Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-2106"));
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

/// 本切片的 router（生产装配路径）。
fn config_router() -> Router {
    let state = state();
    super::router(state.clone()).with_state(state)
}

/// 全量 router（与 `apps/mc-server/src/main.rs` 同款装配）—— 判据是"全局挂上了且匿名可读"。
fn full_router() -> Router {
    let state = state();
    crate::routes::router(state.clone()).with_state(state)
}

/// `GET <uri>` ⇒ `(status, body-as-json)`。
async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
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
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// 上游 `TestConfigRouteIsPublic`（`integration_test.go:263`，⑨ 的 `001`）：匿名 200 + JSON 对象。
/// 🔴 库 URL **不可达** ⇒ 这一条同时是"**不触库**"的可执行判据。
#[tokio::test]
async fn config_route_is_public_and_offline() {
    for router in [config_router(), full_router()] {
        let (status, body) = get_json(&router, "/api/config").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_object(), "body must be a JSON object, got {body}");
    }
}

/// 四条能力声明必须以**真键真值**出现（上游三个 `…Supported` 测试逐字都额外断言了
/// "键在 JSON body 里"——因为客户端把"缺键"读作"不支持"，`false` 与缺键是两件事）。
#[tokio::test]
async fn capability_declarations_are_real_keys() {
    let (_status, body) = get_json(&full_router(), "/api/config").await;
    for key in [
        "local_worktree_supported",
        "agent_conversation_starters_supported",
        "comment_delete_keep_replies_supported",
        "issue_create_properties_supported",
    ] {
        assert!(
            body.get(key).is_some(),
            "{key} must be a real key, not omitted"
        );
    }
    // 本 build 的三条能力为 true（证据见 `docs/32` §45 的逐条实测表）。
    assert_eq!(body["local_worktree_supported"], true);
    assert_eq!(body["agent_conversation_starters_supported"], true);
    assert_eq!(body["comment_delete_keep_replies_supported"], true);
    // 唯一一条取 `false`：`CreateIssueRequest` 没有 `properties` 字段 ⇒ 宣告 true 就是撒谎。
    assert_eq!(body["issue_create_properties_supported"], false);
}

/// 缺省部署下的键集：**17 个字段**里 11 个必现、6 个被 `omitempty` 省掉。
///
/// ⚠️ 判据里 `server_version` 归在**必现**那一侧：测试进程没有 `MULTICA_APP_URL` /
/// `FRONTEND_ORIGIN` ⇒ 不是官方云 ⇒ 自建版 ⇒ 版本号照发（上游逐字）。想看它缺席，
/// 装配层那条 `server_version_is_suppressed_on_official_cloud_only` 已经钉住。
#[tokio::test]
async fn default_body_key_set_matches_the_upstream_shape() {
    let (_status, body) = get_json(&full_router(), "/api/config").await;
    let obj = body.as_object().expect("object");
    // 无 omitempty ⇒ 缺省也必现的 11 个键。
    for key in [
        "cdn_domain",
        "allow_signup",
        "posthog_key",
        "posthog_host",
        "analytics_environment",
        "feature_flags",
        "local_worktree_supported",
        "agent_conversation_starters_supported",
        "issue_create_properties_supported",
        "comment_delete_keep_replies_supported",
        "server_version",
    ] {
        assert!(obj.contains_key(key), "{key} must always be present");
    }
    // omitempty ⇒ 缺省不出现的 6 个键。
    for key in [
        "cdn_signed",
        "google_client_id",
        "workspace_creation_disabled",
        "daemon_server_url",
        "daemon_app_url",
        "vcs_integration_available",
    ] {
        assert!(!obj.contains_key(key), "{key} must be omitted when falsy");
    }
    // 11 + 6 == 17 个字段（`AppConfig` 的字段数）⇒ 多一个键就是形状漂了
    //（`#[serde(flatten)]` / 给白名单加字段都会在这里现形）。
    assert_eq!(obj.len(), 11, "unexpected extra keys: {obj:?}");
    // 未设 env ⇒ 空串的总出现键（是"空串"而不是"键缺席"—— 这两件事客户端读起来不同）。
    assert_eq!(body["cdn_domain"], "");
    assert_eq!(body["posthog_key"], "");
    assert_eq!(body["posthog_host"], "");
    // `analytics_environment` 的缺省是 `"dev"`（不是空串）：两个 env 都没配时的第三级回落。
    assert_eq!(body["analytics_environment"], "dev");
    assert_eq!(body["allow_signup"], true);
}

/// 形态：只注册**无尾斜杠**那一形态（上游 plain `r.Get`）⇒ 补斜杠必须 404，
/// 否则 ⑦ 的 `EXTRA_ALIAS` 硬失败。
#[tokio::test]
async fn only_the_plain_form_is_registered() {
    let router = full_router();
    let (ok_status, _) = get_json(&router, "/api/config").await;
    assert_eq!(ok_status, StatusCode::OK);
    let (slash_status, _) = get_json(&router, "/api/config/").await;
    assert_eq!(
        slash_status,
        StatusCode::NOT_FOUND,
        "must not register a trailing-slash form"
    );
}

/// `vcs_integration_available` **只读复用** M8 的 `AppState::vcs_keys`：默认未配 ⇒ 键不出现。
#[tokio::test]
async fn vcs_flag_comes_from_the_m8_state_seam() {
    let (_status, body) = get_json(&full_router(), "/api/config").await;
    // 测试进程的 env 没有 `MULTICA_VCS_INTEGRATION_ENABLED` ⇒ `VcsKeys::from_env` 读到 false。
    assert_eq!(body.get("vcs_integration_available"), None);
    // 判据是"读的是 `vcs_keys`"而不是"又解析了一遍 env"：显式打开时键出现且为 true。
    let state = state_with_vcs(true);
    let (_s, on) = get_json(
        &super::router(state.clone()).with_state(state),
        "/api/config",
    )
    .await;
    assert_eq!(on["vcs_integration_available"], true);
}

/// `feature_flags` 的键序 = 排序键序（`BTreeMap`），与上游 Go `map[string]bool` 经
/// `encoding/json` 序列化后的**排序**输出逐字对齐。
#[test]
fn feature_flags_keys_are_sorted() {
    let flags: BTreeMap<String, bool> =
        mc_feature_flags::frontend::evaluate_frontend_public_flags(env(&[]));
    let keys: Vec<&str> = flags.keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    assert_eq!(
        keys,
        vec![
            "agents_agent_builder",
            "agents_skill_toggles",
            "billing_workspace_subscriptions",
            "composio_mcp_apps",
            "plugins_v1",
            "settings_resource_labels",
        ]
    );
}

/// 装一个显式打开 VCS 开关的 `AppState`。
///
/// `VcsKeys` 的字段私有且**没有** `with_enabled` 构造器 ⇒ 走它自己的公开注入口
/// `from_env_with`（与 `state/integrations.rs` 里其它面的同款判例），不新增任何
/// 生产可见的 API。
fn state_with_vcs(enabled: bool) -> Arc<AppState> {
    let db = mc_db::Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-2106"));
    let vcs_keys = crate::state::integrations::VcsKeys::from_env_with(|name| {
        (name == crate::state::integrations::VcsKeys::ENABLED_ENV && enabled)
            .then(|| "true".to_owned())
    });
    let mut state = AppState::new(
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
    );
    state.vcs_keys = vcs_keys;
    Arc::new(state)
}
