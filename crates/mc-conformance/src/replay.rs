//! 回放：打请求、跑层、合并两层观察、拼成报告行。
//!
//! 从 `lib.rs` 拆出（门 ⑩ 第 8 批）。对外符号由 crate 根 `pub use` 重导出，路径不变。

use axum::body::to_bytes;
use axum::Router;
use tower::ServiceExt;

use crate::bindings::Bindings;
use crate::report::Row;
use crate::request_plan::plan;
use crate::requirements::{
    actor_credential_detail, credential_satisfied_by, missing_requirements,
    request_target_is_encodable, requirements_detail, unplannable_request_detail,
};
use crate::verdict::{judge, FixtureOutcome, Observed, Outcome};
use crate::Fixture;

/// 打一次请求并判定。
pub async fn replay_one(app: &Router, fx: &Fixture, bindings: &Bindings) -> FixtureOutcome {
    match plan(fx, bindings) {
        Err(reason) => FixtureOutcome {
            outcome: Outcome::Unevaluable,
            tier: "none".into(),
            detail: reason,
            status_observed: None,
            offline: None,
            database: None,
        },
        Ok(p) => {
            let notes = p.notes.join("; ");
            let request = match p.to_http() {
                Ok(r) => r,
                Err(e) => {
                    return FixtureOutcome {
                        outcome: Outcome::Unevaluable,
                        tier: "none".into(),
                        detail: format!("cannot build request: {e}"),
                        status_observed: None,
                        offline: None,
                        database: None,
                    }
                }
            };
            let response = match app.clone().oneshot(request).await {
                Ok(r) => r,
                Err(e) => {
                    return FixtureOutcome {
                        outcome: Outcome::Unevaluable,
                        tier: "none".into(),
                        detail: format!("router refused the request: {e}"),
                        status_observed: None,
                        offline: None,
                        database: None,
                    }
                }
            };
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = to_bytes(response.into_body(), 1 << 20)
                .await
                .map(|b| b.to_vec())
                .unwrap_or_default();
            let observed = Observed {
                status,
                body,
                content_type,
            };
            let (outcome, mut detail) = judge(fx, &observed);
            if !notes.is_empty() {
                detail = format!("{detail}; {notes}");
            }
            FixtureOutcome {
                outcome,
                tier: "stateless".into(),
                detail,
                status_observed: Some(status),
                offline: Some(outcome),
                database: None,
            }
        }
    }
}
// 回放驱动
// ---------------------------------------------------------------------------

/// 回放层次。
///
/// 层与 fixture 的匹配规则是**显式**的，且分两问：**身份**与**前提**。
/// 1. 身份：stateless 层只判定 [`ActorKind::Anonymous`] fixture（它的全部结论都能在没有
///    数据库时得出）；其余 fixture 在这一层不可判定。
/// 2. 前提：即使身份是匿名，场景本身需要的装配凑不齐时同样不判定（见 [`requirements`]）——
///    `POST /auth/send-code` 就是这一类：匿名，但它一上来就查库。
///
/// 任何一问答「否」都落 `unevaluable` 并写明原因，**不用「没库导致的 500」冒充 `mismatch`**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 无数据库连接（懒连接池指向不可达端口），只判定匿名断言。
    Stateless,
    /// 真库 + 迁移 + 种子身份。
    Database,
}

impl Tier {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stateless => "stateless",
            Self::Database => "database",
        }
    }

    /// 这一层能不能**判定**这条 fixture：身份可判定 ∧ 前提全齐。
    ///
    /// `Result` 只在前提表不同名时是 `Err`（加载期已拦过一次，这里是第二道）。
    pub fn supports(self, fx: &Fixture) -> Result<bool, String> {
        // 三问答都必须来自**声明表**（凭据表 / 前提表 / 请求面闸门），而不是 `plan` 的 `Err`
        // 兜底 —— 否则「这一层判不判」与「请求发不发得出来」又各说一套（§216）。
        let identity_ok = credential_satisfied_by(fx.actor.kind, self)?;
        Ok(identity_ok
            && missing_requirements(fx, self)?.is_empty()
            && request_target_is_encodable(fx))
    }
}

/// 一层里可能不止一个 router：**部署形态不同，判定能力就不同**。
///
/// `base` 是默认装配；`cloud_configured` 是第二个 router，只给声明了
/// `cloud_runtime_configured` 的 fixture 用。上游有三个互不相同的部署场景
/// （`fakeCloudRuntimeProxy{enabled:false}` ⇒ 403、`:true` + 本地短路 ⇒ 401/429、
/// `:true` + 转发 ⇒ 上游响应），而 stateless 层只能选一个作为默认；所以第二个形态
/// 显式存在，而不是把默认改成「已配置」—— 那会让断言「未配置 ⇒ 403」的那条失真。
#[derive(Clone)]
pub struct TierRouters {
    base: Router,
    cloud_configured: Option<Router>,
}

impl TierRouters {
    /// 只有一个 router 的层（database 层当前如此）。
    #[must_use]
    pub fn single(base: Router) -> Self {
        Self {
            base,
            cloud_configured: None,
        }
    }

    /// stateless 层的两个部署形态。
    #[must_use]
    pub fn with_cloud_configured(base: Router, cloud_configured: Router) -> Self {
        Self {
            base,
            cloud_configured: Some(cloud_configured),
        }
    }

    /// 这一条声明了「必须有 cloud 配置」的前提吗？
    fn wants_cloud_configured(fx: &Fixture) -> bool {
        fx.requirement_ids().contains(&"cloud_runtime_configured")
    }

    /// 这条 fixture 该打哪个 router。
    ///
    /// 声明了 `cloud_runtime_configured` 却没有第二个 router 时**回落到 base**，
    /// 不是一个静默的错误：那一层根本不判定这类 fixture（`Tier::supports` 先答了否），
    /// 所以回落到哪都不影响结论。
    pub fn for_fixture(&self, fx: &Fixture) -> &Router {
        match &self.cloud_configured {
            Some(cloud) if Self::wants_cloud_configured(fx) => cloud,
            _ => &self.base,
        }
    }
}

/// 跑一层：`routers` 是这一层可用的 router（按 fixture 选）。
pub async fn run_tier(
    routers: &TierRouters,
    fixtures: &[Fixture],
    bindings: &Bindings,
    tier: Tier,
) -> Vec<FixtureOutcome> {
    let mut out = Vec::with_capacity(fixtures.len());
    for fx in fixtures {
        let mut got = match tier.supports(fx) {
            Err(e) => FixtureOutcome {
                outcome: Outcome::Unevaluable,
                tier: tier.as_str().into(),
                detail: e,
                status_observed: None,
                offline: None,
                database: None,
            },
            Ok(false) => {
                // 理由必须点名**真正没过的那一关**（身份 → 前提 → 请求面）。
                let identity_ok = credential_satisfied_by(fx.actor.kind, tier).unwrap_or(true);
                let detail = if !identity_ok {
                    // 凭据面：「别的层有签发面」指路；一个都没有 ⇒ 说清「没有签发面」。
                    match actor_credential_detail(fx.actor.kind, tier) {
                        Ok(s) | Err(s) => s,
                    }
                } else if !missing_requirements(fx, tier)
                    .unwrap_or_default()
                    .is_empty()
                {
                    requirements_detail(fx, tier)
                } else {
                    // 请求面：路径拼不出合法 URI / 还有未填充的占位符。
                    unplannable_request_detail(fx)
                };
                FixtureOutcome {
                    outcome: Outcome::Unevaluable,
                    tier: tier.as_str().into(),
                    detail,
                    status_observed: None,
                    offline: None,
                    database: None,
                }
            }
            Ok(true) => replay_one(routers.for_fixture(fx), fx, bindings).await,
        };
        got.tier = tier.as_str().to_string();
        match tier {
            Tier::Stateless => got.offline = Some(got.outcome),
            Tier::Database => got.database = Some(got.outcome),
        }
        out.push(got);
    }
    out
}

/// 合并两层观察：取更强的一层，并记录结论来自哪层。
#[must_use]
pub fn merge(
    stateless: Option<FixtureOutcome>,
    database: Option<FixtureOutcome>,
) -> FixtureOutcome {
    match (stateless, database) {
        (None, None) => FixtureOutcome {
            outcome: Outcome::Unevaluable,
            tier: "none".into(),
            detail: "no tier ran this fixture".into(),
            status_observed: None,
            offline: None,
            database: None,
        },
        (Some(s), None) => s,
        (None, Some(d)) => d,
        (Some(s), Some(d)) => {
            let (winner, tier) = if d.outcome < s.outcome {
                (d.clone(), "database")
            } else {
                (s.clone(), "stateless")
            };
            FixtureOutcome {
                outcome: winner.outcome,
                tier: tier.into(),
                detail: winner.detail,
                status_observed: winner.status_observed,
                offline: s.offline.or(Some(s.outcome)),
                database: d.database.or(Some(d.outcome)),
            }
        }
    }
}

/// 把 fixture + 两层观察拼成报告行。
#[must_use]
pub fn to_row(fx: &Fixture, merged: &FixtureOutcome) -> Row {
    Row {
        id: fx.id.clone(),
        domain: fx.domain(),
        method: fx.method.clone(),
        path: fx.path.clone(),
        actor: fx.actor.kind.as_str().to_string(),
        via: if fx.source.via.is_empty() {
            "unknown".into()
        } else {
            fx.source.via.clone()
        },
        status_expected: fx.expect.status,
        source: format!("{}:{}", fx.source.file, fx.source.line),
        requires: fx
            .requirement_ids()
            .into_iter()
            .map(str::to_string)
            .collect(),
        outcome: merged.outcome,
        tier: merged.tier.clone(),
        status_observed: merged.status_observed,
        offline: merged.offline,
        database: merged.database,
        detail: merged.detail.clone(),
    }
}
