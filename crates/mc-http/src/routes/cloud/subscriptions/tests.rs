//! M9-2 的证据面（`docs/62` §4.2 的「离线替身」与 §6.5 的通用 `DoD`）。
//!
//! 分两半，**判据不同**（与 `routes/channels/lark/tests.rs` 同手法）：
//!
//! - **不碰库的那一半**（门 ⑤，本文件）：授权链的**出站前**三层 —— 无会话 401 / 机器凭据 403 /
//!   flag 关 403 / workspace 解析失败 400，以及传输错误表与两条纯函数的逐格判定；
//! - **真库的那一半**（门 ⑥，`#[ignore]`，`MULTICA_TEST_DATABASE_URL`）：真库 + **离线云侧替身**
//!   ⇒ 7 条的出站契约、`workspace_id` 注入/覆盖、两档幂等键与转发规则、座位购买三件套、角色矩阵。
//!   未设置变量 ⇒ 打印跳过并 `return`；**已设置但连不上 / 没建表 ⇒ panic**（不许静默假装绿）。
//!
//! 共用件（替身 / `AppState` 字面量 / 请求装置）在 `subscriptions/tests/support.rs`；真库那一半在
//! `subscriptions/tests/db.rs` —— 拆出来是门 ⑩（单文件 800 行）的要求，先例 = `docs/32` §30 的 **D10**。

use std::sync::{Mutex, OnceLock};

use axum::body::Body as AxumBody;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_core::cloud::{
    CLOUD_SUBSCRIPTIONS_PREFIX, MAX_IDEMPOTENCY_KEY_LENGTH,
    MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH, SUBSCRIPTIONS_UPSTREAM_PREFIX,
};
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt as _;
use uuid::Uuid;

use super::*;
use crate::actor_guard::HUMAN_ACTOR_REQUIRED_MESSAGE;
use crate::routes::auth_user::USER_ID_HEADER;
use crate::state::cloud::{CloudConfig, EntitlementConfig};
use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use crate::state::{
    AdapterRegistry, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
};

mod db;
mod support;

use support::*;

// -----------------------------------------------------------------------
// 不碰库的那一半（门 ⑤）
// -----------------------------------------------------------------------

/// 本地字面量与云侧路径是**同一个键的前后两半**（`mc-core` 的两个前缀是唯一真相源）。
///
/// 五条写的云侧路径多一个 workspace 段（它是本片注入的那个值）⇒ 比较时先剥掉它。
#[test]
fn local_and_upstream_paths_are_the_two_halves_of_one_key() {
    for (method, local, upstream) in SEVEN {
        assert!(local.starts_with(CLOUD_SUBSCRIPTIONS_PREFIX), "{local}");
        let upstream_without_workspace = upstream.replace("/{ws}", "");
        assert!(
            upstream_without_workspace.starts_with(SUBSCRIPTIONS_UPSTREAM_PREFIX),
            "{upstream}"
        );
        assert_eq!(
            upstream_without_workspace
                .strip_prefix(SUBSCRIPTIONS_UPSTREAM_PREFIX)
                .expect("前缀已断言"),
            local
                .strip_prefix(CLOUD_SUBSCRIPTIONS_PREFIX)
                .expect("前缀已断言"),
            "{method} {local}"
        );
    }
    // 只有 checkout 一条的云侧路径**不带** workspace 段（它走注入体）。
    let without_workspace: Vec<&str> = SEVEN
        .iter()
        .filter(|(_, _, upstream)| !upstream.contains("{ws}"))
        .map(|(_, local, _)| *local)
        .collect();
    assert_eq!(without_workspace, vec![CHECKOUT]);
}

/// 授权链的**出站前**三层 × 7 条：无会话 401 / 机器凭据 403 / workspace 解析 400
/// —— 三者都**不产生出站请求**。
#[tokio::test]
async fn the_pre_database_layers_hold_for_all_seven_routes() {
    let app = test_app(lazy_db(), cloud_at(&stub_base()), true);
    let workspace = Uuid::new_v4();
    let user = Uuid::new_v4();

    for (method, local, _) in SEVEN {
        // ① 无会话 ⇒ 401（`AuthUser` 提取器）。
        let call = Call::new(method, local, user, workspace).without_session();
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {local}");
        assert_eq!(error_of(&bytes)["code"], "unauthorized", "{local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );

        // ② 机器凭据（两种来源）⇒ 403 + 上游逐字文本（闸在链上先于会话提取，见 `docs/32` §46 D-1）。
        for actor in ["task_token", "cloud_pat"] {
            let call = Call::new(method, local, user, workspace).machine(actor);
            let (status, _, bytes) = send(&app, &call).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: {method} {local}");
            let message = error_of(&bytes)["message"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(message.contains(HUMAN_ACTOR_REQUIRED_MESSAGE), "{message}");
            assert!(
                calls(&call.request_id).is_empty(),
                "{actor}: {local} 不得产生出站请求"
            );
        }

        // ③ workspace 四个来源都缺 ⇒ 400（本仓既有 `resolve_workspace` 的口径）。
        let call = Call::new(method, local, user, workspace).without_workspace();
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {local}");
        assert_eq!(error_of(&bytes)["code"], "validation_error", "{local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
    }
}

/// rollout flag 只 gate **写 5 条**（本片 `DoD` 第 1 条；与上游的偏离登记在 `docs/32` §47）。
#[tokio::test]
async fn the_rollout_flag_gates_the_five_writes_and_not_the_two_reads() {
    let workspace = Uuid::new_v4();
    let user = Uuid::new_v4();
    let app = test_app(lazy_db(), cloud_at(&stub_base()), false);

    for (method, local, _) in FIVE_WRITES {
        let call = Call::new(method, local, user, workspace);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {local}");
        assert_eq!(
            error_of(&bytes)["code"],
            CODE_SUBSCRIPTIONS_DISABLED,
            "{local}"
        );
        assert_eq!(
            error_of(&bytes)["message"],
            MSG_SUBSCRIPTIONS_DISABLED,
            "{local}"
        );
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
    }

    // 读 2 条**不**受 flag 影响：它们走到成员校验（懒库 ⇒ 非 403），但**不是** flag 那条 403。
    for local in [SUMMARY, PRICES] {
        let call = Call::new("GET", local, user, workspace);
        let (status, _, bytes) = send(&app, &call).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "{local} 读面不受 flag 影响");
        assert_ne!(
            error_of(&bytes)["code"],
            CODE_SUBSCRIPTIONS_DISABLED,
            "{local}"
        );
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
    }
}

/// 幂等键的两个 helper 在**头那一路**同值（portal 的注释引用了这条）。
#[test]
fn the_two_key_helpers_agree_on_the_header_path() {
    for raw in [None, Some(""), Some("  "), Some("k"), Some("  k  ")] {
        let resolved =
            subscriptions::resolve_idempotency_key(None, raw, MAX_IDEMPOTENCY_KEY_LENGTH);
        let forwarded = subscriptions::forwarded_idempotency_key(raw);
        assert_eq!(
            resolved.ok(),
            forwarded,
            "{raw:?}：校验用的键与转发的头必须同值"
        );
    }
}

/// 座位购买的那一串校验（上游逐字的五个合取项 + `isASCIICurrency`）。
#[test]
fn seat_purchase_validation_matches_upstream() {
    let base = CloudSubscriptionSeatPurchaseRequest {
        additional_seats: 1,
        expected_current_seats: 5,
        expected_purchase_version: 1,
        accepted_proration_amount: 0,
        currency: "usd".into(),
        idempotency_key: None,
    };
    assert!(is_valid_seat_purchase(&base));
    // `accepted_proration_amount` 是**唯一**允许为 0 的那一项（上游 `>= 0`）。
    let mut zero_seats = base.clone();
    zero_seats.additional_seats = 0;
    assert!(!is_valid_seat_purchase(&zero_seats));
    let mut zero_current = base.clone();
    zero_current.expected_current_seats = 0;
    assert!(!is_valid_seat_purchase(&zero_current));
    let mut zero_version = base.clone();
    zero_version.expected_purchase_version = 0;
    assert!(!is_valid_seat_purchase(&zero_version));
    let mut negative = base.clone();
    negative.accepted_proration_amount = -1;
    assert!(!is_valid_seat_purchase(&negative));
    for currency in ["usd", "USD", "eUr"] {
        assert!(is_ascii_currency(currency), "{currency}");
    }
    for currency in ["", "us", "usdd", "u$d", "us1", "美元"] {
        assert!(!is_ascii_currency(currency), "{currency:?}");
    }
    // 两档上限**不是**同一个数（上游刻意更短）。
    assert_eq!(MAX_IDEMPOTENCY_KEY_LENGTH, 255);
    assert_eq!(MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH, 200);
}

/// `docs/62` §2.6 的四行映射逐行（含离线替身端到端**打不到**的两支）。
#[tokio::test]
async fn the_transport_error_table_is_total() {
    for (error, expected, code) in [
        (
            CloudError::Disabled,
            StatusCode::FORBIDDEN,
            CODE_NOT_CONFIGURED,
        ),
        (CloudError::Transport, StatusCode::BAD_GATEWAY, CODE_FAILED),
        (
            CloudError::InvalidPath,
            StatusCode::BAD_GATEWAY,
            CODE_FAILED,
        ),
        (
            CloudError::InvalidJson,
            StatusCode::BAD_GATEWAY,
            CODE_FAILED,
        ),
        (
            CloudError::InvalidBaseUrl,
            StatusCode::INTERNAL_SERVER_ERROR,
            CODE_MISCONFIGURED,
        ),
        (
            CloudError::Timeout,
            StatusCode::GATEWAY_TIMEOUT,
            CODE_TIMEOUT,
        ),
        (
            CloudError::ResponseTooLarge { limit: 4 },
            StatusCode::BAD_GATEWAY,
            CODE_FAILED,
        ),
    ] {
        let response = transport_error(&error);
        assert_eq!(response.status(), expected, "{error}");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(error_of(&bytes)["code"], code, "{error}");
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("subscriptions/"), "错误体不得回显云侧路径");
    }
}
