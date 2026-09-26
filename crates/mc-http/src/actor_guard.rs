//! **机器凭据闸**（`RequireHumanActor` 的本仓等价物）—— `M9-0` anchor 新建并实现。
//!
//! 上游：`server/internal/handler/actor_guards.go`（120 行）。
//!
//! # 这个闸解决什么问题（R-M9-2，`docs/62` §8）
//!
//! 上游 `Auth` 中间件把**四种** bearer 形态收敛成同一个「已盖章的 `X-User-ID`」，
//! 所以下游 handler 不必关心调用方用的是哪种 token：
//!
//! | 凭据 | `X-User-ID` | `X-Actor-Source` |
//! | --- | --- | --- |
//! | JWT cookie / `mul_` PAT | 人本人的 id | **空** |
//! | `mat_` 任务令牌 | **拥有者**的 id | `task_token` |
//! | `mcn_` 云节点 PAT | 拥有者的 id | `cloud_pat` |
//!
//! 后两者是**有意**设计成"以拥有者身份行事"的（这样 agent 能发评论、认领 issue、注册
//! runtime）。这对 issue / comment / chat 面是对的 —— 那些面被 workspace 成员关系与
//! task / runtime 绑定限住。
//!
//! 但它对**账户级**面**不对**：
//!
//! - 余额 / 流水 / batch / topup 是**用户级**的 —— 一个有 prompt injection 的 agent
//!   可以读它拥有者的钱包，而拥有者从没批准过任何计费查询；
//! - checkout / portal 会话**能搬钱** —— 被污染的机器凭据能替攻击者的邮箱开 checkout，
//!   或开一个泄露订阅 / 支付方式状态的 Billing Portal 会话。
//!
//! ⇒ 15 条账户级路由必须挂这个闸（`M9-1` 8 条 + `M9-2` 的写面 5 条… 逐条点名在各自 `DoD`）。
//! 🔴 **没有这条闸就不许合并 `M9-1` / `M9-2`**（R-M9-2 原文）。
//!
//! # 三态语义（逐字对齐上游 `isMachineCredentialActor`）
//!
//! | `X-Actor-Source` | 判定 | 理由 |
//! | --- | --- | --- |
//! | 缺省 / 空串 | **人类**（放行） | 上游 `Header.Get` 的返回值是 `""`，落在 `default` 分支 |
//! | `task_token` | **机器**（403） | `mat_` 任务令牌 |
//! | `cloud_pat` | **机器**（403） | `mcn_` 云节点 PAT |
//! | 其它任何值 | **人类**（放行） | 上游注释逐字：「silently passing an unknown actor source is a feature, not a bug」 |
//!
//! ⚠️ 第 4 行是**有意的**：`X-Actor-Source` 是**服务端盖章**的头（上游 `auth.go` 先
//! `r.Header.Del("X-Actor-Source")` 再按分支 `Set`）⇒ 客户端伪造的值会被剥掉。
//! 本仓的等价前提见下一节 —— 这也是为什么它是**放行未知值**而不是"拦未知值"。
//!
//! # 🔴 本仓的两条前提（与上游的**差异**，登记 `docs/32` §9.13）
//!
//! 1. **本仓的 `AuthUser` 目前只读 `X-Multica-User-Id`（M1 dev-mode 契约），
//!    不盖章 `X-Actor-Source`**（`routes/auth_user.rs` 的模块头逐字：「当前实现**不**校验
//!    header 与数据库中 user 的对应关系——只信任 header」）⇒ **今天**这个闸只在
//!    「上游盖章链已接线」之前是**形状正确、效力待接线**的：它挡得住显式带了
//!    `X-Actor-Source: task_token` 的请求，但挡不住"伪造了 `X-Multica-User-Id` 又不带
//!    actor-source"的请求 —— 后者本来就已被 M1 dev-mode 契约放行。
//!    这条**不是**本片能补的（它要动 `middleware/authn.rs` 的整条盖章链，属 W1 面）
//!    ⇒ 登记为**未接线项**，由 `M9-10`（INT）与 W1 面协调。
//! 2. **`mat_` / `mcn_` 在本仓的落地形态**：本仓的 PAT 桥
//!    （`routes/pats.rs` 的 `mul_` 面）与 task 令牌面**不**在 M9 的写集里 ⇒ 本片只实现
//!    「读头 + 三态判定 + 403 文本」，**不**新增 token 种类。
//!
//! # 两条**不许**（上游注释逐字点名）
//!
//! - **不许**用「agent vs member」的归属判定（`resolveActor`）替代本闸：归属判定要
//!   workspace 上下文，而计费路由**没有** workspace；语义也不同（归属是"谁签的名"，
//!   本闸是"这是不是机器凭据"）；
//! - **不许**把本闸挂到全局 Auth 链上：别处的默认契约是"agent 与人类可互换"，
//!   全局挂会打断合法的 agent 流量。**只在真正 human-only 的路由组上挂。**

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use mc_errors::Error;

use crate::error::ApiError;

/// 服务端盖章的凭据来源头（**逐字**，含潜在的大小写差异 —— HTTP 头名不区分大小写）。
pub const ACTOR_SOURCE_HEADER_NAME: &str = "X-Actor-Source";

/// 同上，作为 `HeaderName`（避免每处重新解析字面量）。
pub const ACTOR_SOURCE_HEADER: HeaderName = HeaderName::from_static("x-actor-source");

/// 机器凭据的两条 `X-Actor-Source` 取值（上游 `isMachineCredentialActor` 的 denylist）。
pub const MACHINE_ACTOR_SOURCES: [&str; 2] = ["task_token", "cloud_pat"];

/// 403 的**唯一**文本（上游逐字，客户端与用例如依赖它）。
pub const HUMAN_ACTOR_REQUIRED_MESSAGE: &str = "this endpoint is only available to human actors";

/// 判定的三态（`X-Actor-Source` 的四行映射收敛成三个值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorSource {
    /// 人类（含"未知值"——上游有意放行，见模块头）。
    Human,
    /// `mat_` 任务令牌。
    TaskToken,
    /// `mcn_` 云节点 PAT。
    CloudPat,
}

impl ActorSource {
    /// 头的取值（人类是 `None` —— 上游的人类分支**保留**该头为空）。
    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Human => None,
            Self::TaskToken => Some("task_token"),
            Self::CloudPat => Some("cloud_pat"),
        }
    }

    /// 是否机器凭据（⇒ 403）。
    #[must_use]
    pub const fn is_machine(self) -> bool {
        matches!(self, Self::TaskToken | Self::CloudPat)
    }

    /// 从头的原始值分类（**唯一**的分类点；未知值 ⇒ [`ActorSource::Human`]）。
    #[must_use]
    pub fn from_header_value(raw: Option<&str>) -> Self {
        match raw {
            Some("task_token") => Self::TaskToken,
            Some("cloud_pat") => Self::CloudPat,
            _ => Self::Human,
        }
    }
}

/// 从一组头分类（缺省 / 空串 / 非 ASCII ⇒ [`ActorSource::Human`]）。
#[must_use]
pub fn classify_actor_source(headers: &HeaderMap) -> ActorSource {
    let raw = headers
        .get(&ACTOR_SOURCE_HEADER)
        .and_then(|value| value.to_str().ok());
    ActorSource::from_header_value(raw)
}

/// 便捷谓词：这组头是不是机器凭据（上游 `isMachineCredentialActor`）。
///
/// ⚠️ 消费者**应当优先**用 [`HumanActor`] 提取器或 [`require_human_actor`] 中间件 ——
/// 本函数是给"fail-closed backstop"（上游逐字：敏感的 handler 可以在路由中间件之外
/// 再自查一次）与用例用的。
#[must_use]
pub fn is_machine_credential_actor(headers: &HeaderMap) -> bool {
    classify_actor_source(headers).is_machine()
}

/// 403 错误（文本逐字对齐上游）。
#[must_use]
pub fn human_actor_required_error() -> Error {
    Error::Forbidden {
        message: HUMAN_ACTOR_REQUIRED_MESSAGE.to_string(),
    }
}

/// **人类凭据提取器**（`RequireHumanActor` 的 handler 级形态）。
///
/// handler 签名里加一个 `actor: HumanActor` 即可 —— 提取失败（机器凭据）时 axum 直接
/// 返回 403，handler 体不执行。与上游「router middleware + handler 内 backstop」
/// 的双保险同款：既有 [`require_human_actor`] 中间件（路由组级），也有本提取器（handler 级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanActor;

impl HumanActor {
    /// 分类结果（诊断用）。
    #[must_use]
    pub fn source(headers: &HeaderMap) -> ActorSource {
        classify_actor_source(headers)
    }
}

#[async_trait::async_trait]
impl<S> axum::extract::FromRequestParts<S> for HumanActor
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        if is_machine_credential_actor(&parts.headers) {
            return Err(ApiError(human_actor_required_error()));
        }
        Ok(Self)
    }
}

/// 路由组级中间件（`route_layer(from_fn(require_human_actor))`）。
///
/// 上游用法逐字：「Apply via `r.Use(handler.RequireHumanActor)` on a chi route group」。
pub async fn require_human_actor(req: Request, next: Next) -> Response {
    if is_machine_credential_actor(req.headers()) {
        return ApiError(human_actor_required_error()).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt as _;
    use tower::util::ServiceExt as _;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                value.parse().expect("header value"),
            );
        }
        map
    }

    /// 三态用例 ①：**无 `X-Actor-Source` ⇒ 通过**（这是"人类"的唯一判据）。
    #[test]
    fn missing_header_is_treated_as_a_human() {
        assert_eq!(classify_actor_source(&HeaderMap::new()), ActorSource::Human);
        assert!(!is_machine_credential_actor(&HeaderMap::new()));
        // 空串同样是人类（上游 `Header.Get` 返回 `""`）。
        assert_eq!(
            classify_actor_source(&headers(&[("x-actor-source", "")])),
            ActorSource::Human
        );
    }

    /// 三态用例 ②③：`task_token` / `cloud_pat` ⇒ 机器。
    #[test]
    fn the_two_machine_sources_are_denied() {
        assert_eq!(
            classify_actor_source(&headers(&[("x-actor-source", "task_token")])),
            ActorSource::TaskToken
        );
        assert_eq!(
            classify_actor_source(&headers(&[("x-actor-source", "cloud_pat")])),
            ActorSource::CloudPat
        );
        assert!(is_machine_credential_actor(&headers(&[(
            "x-actor-source",
            "task_token"
        )])));
        assert!(is_machine_credential_actor(&headers(&[(
            "x-actor-source",
            "cloud_pat"
        )])));
        assert_eq!(ActorSource::TaskToken.as_str(), Some("task_token"));
        assert_eq!(ActorSource::CloudPat.as_str(), Some("cloud_pat"));
        assert_eq!(ActorSource::Human.as_str(), None);
        assert_eq!(MACHINE_ACTOR_SOURCES, ["task_token", "cloud_pat"]);
        assert_eq!(
            human_actor_required_error().to_string(),
            "forbidden: this endpoint is only available to human actors"
        );
    }

    /// 未知值**有意**放行（上游有专门一条 `TestRequireHumanActor_IgnoresUnknownActorSource`）。
    #[test]
    fn unknown_actor_sources_stay_human_equivalent() {
        for raw in [
            "service_account",
            "Task_Token",
            "TASK_TOKEN",
            "cloud-pat",
            " task_token",
            "task_token ",
            "anonymous",
        ] {
            assert_eq!(
                classify_actor_source(&headers(&[("x-actor-source", raw)])),
                ActorSource::Human,
                "{raw:?} 必须按人类处理（上游是一条特性，不是缺陷）"
            );
        }
    }

    /// 头名大小写不敏感（HTTP 语义），且**只看**这一个头。
    #[test]
    fn header_name_is_case_insensitive_and_self_contained() {
        for name in ["x-actor-source", "X-Actor-Source", "X-ACTOR-SOURCE"] {
            assert!(is_machine_credential_actor(&headers(&[(
                name,
                "task_token"
            )])));
        }
        // 别的头（例如一个看起来像的 `X-Actor-Sources`）不参与判定。
        assert!(!is_machine_credential_actor(&headers(&[(
            "x-actor-sources",
            "task_token"
        )])));
        assert!(!is_machine_credential_actor(&headers(&[(
            "x-multica-user-id",
            "task_token"
        )])));
    }

    #[test]
    fn non_ascii_header_values_fall_back_to_human_without_panicking() {
        let mut map = HeaderMap::new();
        map.insert(
            ACTOR_SOURCE_HEADER,
            axum::http::HeaderValue::from_bytes(b"\xff\xfe").expect("opaque bytes are allowed"),
        );
        assert_eq!(classify_actor_source(&map), ActorSource::Human);
    }

    /// 端到端：提取器形态 —— 机器凭据被 403 挡在 handler 之外（handler **不**执行）。
    #[tokio::test]
    async fn extractor_rejects_machine_credentials_before_the_handler_runs() {
        let app: Router = Router::new().route(
            "/billing",
            get(|_actor: HumanActor| async { "handler-ran" }),
        );

        // 人类（无头）⇒ handler 跑了。
        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/billing")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(&body[..], b"handler-ran");

        // 机器凭据 ⇒ 403 + 上游逐字文本，且 handler **没有**跑。
        for raw in ["task_token", "cloud_pat"] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri("/billing")
                        .header(ACTOR_SOURCE_HEADER_NAME, raw)
                        .body(Body::empty())
                        .expect("req"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{raw}");
            let body = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            let text = String::from_utf8_lossy(&body);
            assert!(text.contains(HUMAN_ACTOR_REQUIRED_MESSAGE), "{text}");
            assert!(!text.contains("handler-ran"), "{text}");
        }
    }

    /// 端到端：中间件形态 —— 同一个闸挂在整个路由组上。
    #[tokio::test]
    async fn middleware_guards_a_whole_route_group() {
        let app: Router = Router::new()
            .route("/api/cloud-billing/balance", get(|| async { "ok" }))
            .route("/api/cloud-billing/topups", get(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn(require_human_actor));

        for uri in ["/api/cloud-billing/balance", "/api/cloud-billing/topups"] {
            let human = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("req"),
                )
                .await
                .expect("response");
            assert_eq!(human.status(), StatusCode::OK, "{uri}");

            let machine = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(uri)
                        .header(ACTOR_SOURCE_HEADER_NAME, "task_token")
                        .body(Body::empty())
                        .expect("req"),
                )
                .await
                .expect("response");
            assert_eq!(machine.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }
}
