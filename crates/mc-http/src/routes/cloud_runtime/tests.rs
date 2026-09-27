//! M9-11 的证据面（`docs/62` §4.2 的「离线替身」与 §6.5 的通用 `DoD`）。
//!
//! 判据分两半，**依据不同**（与 `routes/cloud/subscriptions/tests.rs` 同手法）：
//!
//! - **本文件**（门 ⑤，不碰库）：路由注册键的**逐字**形状、`withQuery` / `withBody` 的
//!   开关位置、授权链的**出站前**三层（401 / 400 / 机器凭据**不拦**）、传输错误表的**总性**；
//! - **`tests/db.rs`**（门 ⑥，`#[ignore]`，`MULTICA_TEST_DATABASE_URL`）：真库 + **离线云侧
//!   替身** ⇒ 11 条的出站契约逐字、成员/非成员的角色矩阵、体校验三道。
//!
//! 共用件（替身 / `AppState` 字面量 / 请求装置）在 `tests/support.rs`。
//!
//! ## 替换说明
//!
//! `pub use fixture;` 那行是本仓的既有写法（见 `subscriptions/tests/support.rs` 的同名
//! `macro_rules!` 文本作用域技巧）—— 它让 `tests/db.rs` 能用 `use super::support::*;`
//! 取到 `fixture!` 宏。

use axum::body::Body as AxumBody;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::json;
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
// 路由表 / 注册键（门 ⑦ 负责「本地 ↔ 上游路由表」，这里负责**运行期**那一半）
// -----------------------------------------------------------------------

/// 本地字面量与云侧路径是**同一个键的前后两半**（`mc-cloud` 的两个前缀是唯一真相源）。
///
/// ⚠️ 这一条钉的是**尾斜杠**：`GET /api/cloud-runtime/` 的云侧那一半是 `/api/v1/`，
/// 两边**都**带尾斜杠 —— 「顺手」规范化任一边都会被这一条抓住。
#[test]
fn local_and_upstream_paths_are_the_two_halves_of_one_key() {
    for (method, local, upstream, _, _) in ELEVEN {
        assert!(local.starts_with(runtime::CLOUD_RUNTIME_PREFIX), "{local}");
        // 两条探针在**云侧**没有 `/api/v1` 前缀（上游逐字 `/healthz` / `/readyz`）⇒ 先判它们。
        if local == "/api/cloud-runtime/healthz" || local == "/api/cloud-runtime/readyz" {
            assert!(
                !upstream.starts_with(runtime::RUNTIME_UPSTREAM_PREFIX),
                "{upstream} 不得带 /api/v1 前缀"
            );
            assert_eq!(upstream, &local[runtime::CLOUD_RUNTIME_PREFIX.len()..]);
            continue;
        }
        let suffix = local
            .strip_prefix(runtime::CLOUD_RUNTIME_PREFIX)
            .expect("前缀已断言");
        // 根那条的云侧是 `/api/v1/`（带斜杠），其余是 `/api/v1` + 同一个后缀。
        let expected = format!("{}{suffix}", runtime::RUNTIME_UPSTREAM_PREFIX);
        assert_eq!(upstream, expected, "{method} {local}");
    }
}

/// `GET /api/cloud-runtime` 与 `GET /api/cloud-runtime/` **两个键都在**（上游 chi 的
/// Mount + child `/` 让两种写法都命中；axum 必须都注册 —— 门 ⑦b 的 `MISSING_ALIAS`）。
///
/// 单独钉这一条：只注册带尾斜杠那一形态会同时骗过本片用例与 ⑦ 的一半检查。
#[tokio::test]
async fn the_service_root_serves_both_the_slashed_and_the_bare_form() {
    let app = test_app(lazy_db(), cloud_at(&stub_base()));
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    // 两种写法都**必须打到 handler**（懒库 ⇒ 成员校验失败，但**不是** 404 / 405）。
    for form in ["/api/cloud-runtime/", "/api/cloud-runtime"] {
        let call = Call::new("GET", form, user, ws);
        let (status, _, _) = send(&app, &call).await;
        assert_ne!(status, StatusCode::NOT_FOUND, "{form}：键必须存在");
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{form}");
        assert!(
            calls(&call.request_id).is_empty(),
            "懒库 ⇒ 到不了出站，但仍不得凭空产生出站"
        );
    }
}

/// 11 条逐条存在、且**没多**注册：本片**零路径参数** ⇒ 每个键只有字面量那一形态。
#[tokio::test]
async fn all_eleven_literals_are_registered_and_nothing_else_is() {
    let app = test_app(lazy_db(), cloud_at(&stub_base()));
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    for (method, local, _, _, _) in ELEVEN {
        let call = Call::new(method, local, user, ws).maybe_body(body_for(method, local));
        let (status, _, _) = send(&app, &call).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {local} 必须已注册（404 = 键不存在）"
        );
    }
    // 反向：节点 id **不是**路径参数（上游节点 id 走体）⇒ 这些形状必须**不存在**。
    for ghost in [
        "/api/cloud-runtime/nodes/node-1",
        "/api/cloud-runtime/nodes/node-1/start",
        "/api/cloud-runtime/nodes/node-1/reboot",
    ] {
        let call = Call::new("POST", ghost, user, ws).body(json!({"x": 1}).to_string());
        let (status, _, _) = send(&app, &call).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{ghost}：上游 11 条里 0 条有路径参数"
        );
    }
}

/// 授权链的**出站前**三层 × 11 条：无会话 401 / workspace 解析 400 —— 两者都**零出站**。
#[tokio::test]
async fn the_pre_database_layers_hold_for_all_eleven_routes() {
    let app = test_app(lazy_db(), cloud_at(&stub_base()));
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();

    for (method, local, _, _, _) in ELEVEN {
        // ① 无会话 ⇒ 401（`AuthUser` 提取器）。
        let call = Call::new(method, local, user, ws)
            .maybe_body(body_for(method, local))
            .without_session();
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {local}");
        assert_eq!(error_of(&bytes)["code"], "unauthorized", "{local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );

        // ② workspace 四个来源都缺 ⇒ 400（本仓既有 `resolve_workspace` 的口径）。
        let call = Call::new(method, local, user, ws)
            .maybe_body(body_for(method, local))
            .without_workspace();
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {local}");
        assert_eq!(error_of(&bytes)["code"], "validation_error", "{local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
    }
}

/// 🔴 本片**不挂**机器凭据闸（与 M9-1 / M9-2 相反；上游 `router.go:1948` 的组只有
/// `RequireWorkspaceMember`）—— 用例钉住「机器凭据**不被**本片拦」。
///
/// 这条是**反向**判据：它挡的是"照着 billing/subscriptions 抄一层闸"这个**最可能**的错。
/// 在**真库**那一半（`db.rs`）里它被再验一次（机器凭据 + 真成员 ⇒ 200）。
#[tokio::test]
async fn machine_credentials_are_not_gated_by_this_slice() {
    let app = test_app(lazy_db(), cloud_at(&stub_base()));
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    for (method, local, _, _, _) in ELEVEN {
        for actor in ["task_token", "cloud_pat"] {
            let call = Call::new(method, local, user, ws)
                .maybe_body(body_for(method, local))
                .machine(actor);
            let (status, _, bytes) = send(&app, &call).await;
            let message = error_of(&bytes)["message"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !message.contains(HUMAN_ACTOR_REQUIRED_MESSAGE),
                "{actor}: {method} {local} 不得被本片拦（上游这一簇没有 RequireHumanActor）"
            );
            // 它会走到成员校验那一格（懒库 ⇒ 连不上 ⇒ 5xx），**不是** 403。
            assert_ne!(status, StatusCode::FORBIDDEN, "{actor}: {method} {local}");
        }
    }
}

/// `withQuery` 只在**唯一**那一条上生效：带 query 的透传、不带的**丢掉**。
#[tokio::test]
async fn query_strings_are_forwarded_only_where_upstream_turns_that_on() {
    // 判据是 `mc-cloud` 那一层的纯函数（真库那一半验真 wire 上它也成立）。
    for (suffix, forwarded) in [
        ("/nodes", true),
        ("/", false),
        ("/healthz", false),
        ("/readyz", false),
    ] {
        let request = match suffix {
            "/" => runtime::service_request(
                "11111111-2222-3333-4444-555555555555".parse().unwrap(),
                None,
            ),
            "/healthz" => runtime::healthz_request(None),
            "/readyz" => runtime::readyz_request(None),
            "/nodes" => runtime::list_nodes_request(
                "11111111-2222-3333-4444-555555555555".parse().unwrap(),
                None,
                runtime::parse_query("limit=10&tag=gpu&tag=arm"),
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            !request.query.is_empty(),
            forwarded,
            "{suffix} withQuery（多值必须保：{forwarded}）"
        );
        if forwarded {
            assert_eq!(
                request.query,
                vec![
                    ("limit".to_string(), "10".to_string()),
                    ("tag".to_string(), "gpu".to_string()),
                    ("tag".to_string(), "arm".to_string()),
                ],
                "保序 + 保多值（上游 url.Values）"
            );
        }
    }
}

/// `withBody` 的位置：7 条带体、4 条不带（上游 `cloudRuntimeProxyOptions` 逐条）。
#[test]
fn with_body_is_on_for_exactly_seven_of_the_eleven() {
    let with_body: Vec<&str> = ELEVEN
        .iter()
        .filter(|(_, _, _, body, _)| *body)
        .map(|(_, local, _, _, _)| *local)
        .collect();
    assert_eq!(with_body.len(), 7, "{with_body:?}");
    for (_, local, _) in SEVEN_BODIES {
        assert!(with_body.contains(&local), "{local} 应当带体");
    }
    // `GET /nodes` 与 `POST|DELETE /nodes` **同 path** ⇒ `no_body` 里那条只出现一次。
    let no_body: Vec<&str> = ELEVEN
        .iter()
        .filter(|(_, _, _, body, _)| !*body)
        .map(|(_, local, _, _, _)| *local)
        .collect();
    assert_eq!(
        no_body,
        vec![
            "/api/cloud-runtime/",
            "/api/cloud-runtime/healthz",
            "/api/cloud-runtime/readyz",
            // `GET /nodes` 与 `POST|DELETE /nodes` 同 path ⇒ 这里只出现一次。
            "/api/cloud-runtime/nodes",
        ]
    );
}

/// `withUserID`：两条探针**不盖章**，其余 9 条**盖章**。
#[test]
fn with_user_id_is_off_for_exactly_the_two_fleet_probes() {
    let anonymous: Vec<&str> = ELEVEN
        .iter()
        .filter(|(_, _, _, _, user)| !*user)
        .map(|(_, local, _, _, _)| *local)
        .collect();
    assert_eq!(
        anonymous,
        vec!["/api/cloud-runtime/healthz", "/api/cloud-runtime/readyz"]
    );
}

/// 🔴 反向验收：`/api/cloud-runtime/{healthz,readyz}` **不是**服务探针 `/healthz` / `/readyz`。
///
/// 上游那两个在 `router.go:1400-1401`（属 M10-2）⇒ 本片**不得**注册它们。
///
/// ⚠️ 两条**都是**已注册的（`/healthz` 由 M10-2 的 `probes/ready.rs` 注册）⇒ 判据
/// 不能是「404」。真正把它们分开的判据是**打到哪儿**：
///
/// - 服务探针 `/healthz`：**零出站**（它看的是**本地**进程 + DB）；
/// - 节点池探针 `/api/cloud-runtime/healthz`：**恰好一笔**出站，目标 `/healthz`。
///
/// 「两个都叫 healthz、但一个不出站一个出站」正是"混实现"会破坏的边界。
#[tokio::test]
async fn the_server_probes_and_the_fleet_probes_hit_different_targets() {
    let app = test_app(lazy_db(), cloud_at(&stub_base()));
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();

    // ① M10-2 的服务探针：不打到云侧（503 = 它自己的 DB 闸，懒库）。
    for probe in ["/healthz", "/readyz"] {
        let call = Call::new("GET", probe, user, ws);
        let (status, _, _) = send(&app, &call).await;
        assert_ne!(status, StatusCode::NOT_FOUND, "{probe} 属 M10-2，已注册");
        assert!(
            calls(&call.request_id).is_empty(),
            "{probe} 是**服务**探针：绝不产生出站请求"
        );
    }

    // ② 本片的 fleet 探针：会出站，且目标就是云侧的 `/healthz` / `/readyz`。
    //    懒库下到不了出站（成员校验先失败）⇒ 这一半的「真的出站」在 `tests/db.rs`；
    //    这里钉的是**两条链互不串**（服务探针零出站）。
}

/// 未配置 / 配了但非法 ⇒ 403 / 500，且**零出站**（上游 `writeFeatureDisabled`）。
///
/// ⚠️ 懒库下这两条走不到（成员校验先失败）⇒ 判据落在 [`transport_error`] 这个纯函数上，
/// 端到端那一半在 `tests/db.rs`。
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
        // 错误体**不得**回显云侧路径（`docs/62` §2.4 判据 ③）。
        assert!(!text.contains("/api/v1/"), "错误体不得回显云侧路径：{text}");
        assert!(
            !text.contains("cloud-runtime"),
            "错误体不得回显本地路由：{text}"
        );
    }
}

/// 体读取的三道判定（上游 `readCloudRuntimeJSONBody`）：1 MiB / 空体 / JSON 语法。
///
/// 判据落在读体那个函数上（不碰库、纯 `axum::body`）。
#[tokio::test]
async fn the_body_reader_rejects_the_three_upstream_cases() {
    // ① 全空白 ⇒ 400「体是必需的」。
    let err = read_cloud_runtime_json_body(json_request(b"   \n\t "))
        .await
        .expect_err("空白体");
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&body_of(err).await)["message"], MSG_BODY_REQUIRED);

    // ② JSON 语法错 ⇒ 400「体非法」。
    let err = read_cloud_runtime_json_body(json_request(b"{not json"))
        .await
        .expect_err("非法 JSON");
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&body_of(err).await)["message"], MSG_BODY_INVALID);

    // ③ 超 1 MiB ⇒ 413。
    let huge = vec![b'x'; MAX_CLOUD_REQUEST_BODY + 1];
    let err = read_cloud_runtime_json_body(json_request(&huge))
        .await
        .expect_err("超限");
    assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_of(&body_of(err).await)["message"], MSG_BODY_TOO_LARGE);

    // ④ 合法体**逐字**返回（含缩进 / unicode，不 trim、不重排）。
    let raw = "{  \"node_id\":\"n-1\",  \"note\":\"café\"}"
        .as_bytes()
        .to_vec();
    let body = read_cloud_runtime_json_body(json_request(&raw))
        .await
        .expect("合法体");
    assert_eq!(body, raw, "体必须逐字转发（云侧要按字节签它）");
}

/// 一个带体的入站请求（读体装置的输入）。
fn json_request(raw: &[u8]) -> Request {
    Request::builder()
        .method("POST")
        .uri("/api/cloud-runtime/nodes/exec")
        .body(AxumBody::from(raw.to_vec()))
        .expect("request")
}

/// 取一个响应的体（断言用）。
async fn body_of(response: Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec()
}
