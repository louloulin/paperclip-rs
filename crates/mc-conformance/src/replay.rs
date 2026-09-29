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
    /// 只有一个 router 的层（当前两层都有第二个形态；保留它是为了让「这一层只有一种
    /// 装配」仍是一种**能写出来**的状态，而不是靠 `Option` 的 `None` 隐式表达）。
    #[must_use]
    pub fn single(base: Router) -> Self {
        Self {
            base,
            cloud_configured: None,
        }
    }

    /// 一层里的两个部署形态（默认 + 已配置 cloud）。
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
            // 平局（`s.outcome == d.outcome`，最常见的是两层都 `Unevaluable`）必须由
            // **真打过真实基础设施的那层**说了算：database 层跑过了，结论和 `tier`
            // 归属就都是它的。
            //
            // 旧写法是 `d.outcome < s.outcome` —— 只有严格小于才判给 database，于是平局
            // 全部落进 `else` 取 stateless 侧。后果不是措辞瑕疵：stateless 侧那两档
            // `member`/`daemon` 身份给出的理由是「请用 `--db-url` 重跑」，而读者手上
            // 这份报告**正是连着库跑出来的那一份** ⇒ 指路牌指向一件已经做过的事，
            // 排障者会绕死循环；database 层自己给出的真话理由（缺的是哪一条前提，
            // 还是 fixture 本身发不出去）被整段丢弃，而 `tier` 字段还把结论错记成
            // `"stateless"`。
            let tie = s.outcome == d.outcome;
            let (winner, loser, tier) = if d.outcome <= s.outcome {
                (d.clone(), s.clone(), "database")
            } else {
                (s.clone(), d.clone(), "stateless")
            };
            // 只有**平局**才需要把落败那层的理由带上：非平局时胜者已经分出强弱，
            // 落败那层的理由不构成对结论的补充（逐字节保持不变，别动已提交的快照）。
            // 平局时两层都没判出来 ⇒ 两句都得留，否则读者会以为只跑了一层。
            // 唯一的例外是那句「请重跑 `--db-url`」：database 层既然已经出过结论，
            // 它指的那条路已经走过，留着反而是假线索（`actor_credential_detail()`
            // 自己也把这种自指列为必须避开的坑）。
            let detail =
                if !tie || loser.detail == winner.detail || is_database_tier_pointer(&loser.detail)
                {
                    winner.detail
                } else {
                    format!(
                        "[{}] {} || [{}] {}",
                        winner.tier, winner.detail, loser.tier, loser.detail
                    )
                };
            FixtureOutcome {
                outcome: winner.outcome,
                tier: tier.into(),
                detail,
                status_observed: winner.status_observed,
                offline: s.offline.or(Some(s.outcome)),
                database: d.database.or(Some(d.outcome)),
            }
        }
    }
}

/// stateless 层那句「请用 `--db-url` 重跑」是不是指路牌？
///
/// 它由 [`crate::requirements::actor_credential_detail`] 生成，语义是「这档身份
/// 换到 database 层才判得了」。**database 层已经产出结论时这句话是自指的假线索**
/// —— 读者看的就是连着库跑出来的那一份报告。形状跟着那一条 format 字面量走
/// （凭据面理由只有这一处拼 `rerun with --db-url`）；真要改措辞，本函数下面那条
/// 单测会跟着一起红。
fn is_database_tier_pointer(detail: &str) -> bool {
    detail.contains("rerun with --db-url")
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

#[cfg(test)]
mod tests {
    use super::{is_database_tier_pointer, merge};
    use crate::verdict::{FixtureOutcome, Outcome};

    fn at(outcome: Outcome, tier: &str, detail: &str) -> FixtureOutcome {
        FixtureOutcome {
            outcome,
            tier: tier.into(),
            detail: detail.into(),
            status_observed: None,
            offline: None,
            database: None,
        }
    }

    /// 现场那 13 条的形状：两层都没判出来，stateless 侧说「请用 `--db-url` 重跑」，
    /// 而 database 层其实跑过了、并且给出了自己的理由。
    const POINTER: &str = "member actor needs the database tier (rerun with --db-url); \
                           stateless tier cannot decide it";
    const DB_REASON: &str = "this scenario needs precondition the database tier does not supply";

    /// 平局时胜者是 database 侧，`tier` 归属也改过来 —— 否则报告把「结论来自
    /// database 层」这件事记成 `"stateless"`。
    #[test]
    fn tie_goes_to_the_tier_that_actually_ran() {
        let got = merge(
            Some(at(Outcome::Unevaluable, "stateless", POINTER)),
            Some(at(Outcome::Unevaluable, "database", DB_REASON)),
        );
        assert_eq!(got.outcome, Outcome::Unevaluable);
        assert_eq!(got.tier, "database");
        assert_eq!(got.detail, DB_REASON);
    }

    /// database 层已经出过结论 ⇒ 那句「请重跑 `--db-url`」是自指的假线索，
    /// 赢的那句不能把它带出来。
    #[test]
    fn winner_never_repeats_a_resolved_database_pointer() {
        let got = merge(
            Some(at(Outcome::Unevaluable, "stateless", POINTER)),
            Some(at(Outcome::Unevaluable, "database", DB_REASON)),
        );
        assert!(
            !is_database_tier_pointer(&got.detail),
            "database 层已经跑过，胜者理由却还在叫人 rerun --db-url：{}",
            got.detail
        );
    }

    /// 平局且两层理由都不是指路牌 ⇒ 两句都要留，否则读者以为只跑了一层。
    #[test]
    fn tie_keeps_both_reasons_when_neither_is_a_pointer() {
        let got = merge(
            Some(at(
                Outcome::Unevaluable,
                "stateless",
                "this scenario needs precondition the stateless tier does not supply",
            )),
            Some(at(Outcome::Unevaluable, "database", DB_REASON)),
        );
        assert_eq!(got.tier, "database");
        assert!(got.detail.contains("[stateless]"), "{}", got.detail);
        assert!(got.detail.contains("[database]"), "{}", got.detail);
    }

    /// database 层不存在（`None`）时逐字不变：门 ⑩ 那份 stateless 快照走的就是
    /// 这一支，它必须原封不动。
    #[test]
    fn without_a_database_layer_nothing_changes() {
        let s = at(Outcome::Pass, "stateless", "status matched");
        let got = merge(Some(s.clone()), None);
        assert_eq!(got.outcome, s.outcome);
        assert_eq!(got.tier, s.tier);
        assert_eq!(got.detail, s.detail);
    }

    /// database 侧**严格更强**时行为逐字不变（不因为改了平局分支就连累非平局）。
    #[test]
    fn strictly_stronger_database_side_still_wins_verbatim() {
        let got = merge(
            Some(at(Outcome::Mismatch, "stateless", "status mismatch")),
            Some(at(Outcome::Pass, "database", "status matched")),
        );
        assert_eq!(got.outcome, Outcome::Pass);
        assert_eq!(got.tier, "database");
        assert_eq!(got.detail, "status matched");
    }

    /// 非平局时**不**拼两层理由（这一支的输出已进已提交快照，逐字节不能动）。
    #[test]
    fn non_tie_detail_is_not_rewritten() {
        let got = merge(
            Some(at(Outcome::Pass, "stateless", "status matched")),
            Some(at(
                Outcome::Mismatch,
                "database",
                "json_subset mismatch at $.x",
            )),
        );
        assert_eq!(got.detail, "status matched");
        assert!(!got.detail.contains("||"), "{}", got.detail);
    }

    /// 指路牌判据跟着 `actor_credential_detail()` 的字面量走：真去改那句措辞，
    /// 这条会先红，而不是等到某条 fixture 的理由悄悄变假。
    #[test]
    fn pointer_predicate_matches_the_credential_detail_shape() {
        assert!(is_database_tier_pointer(POINTER));
        assert!(is_database_tier_pointer(
            "daemon actor needs the database tier (rerun with --db-url); stateless tier cannot decide it"
        ));
        assert!(!is_database_tier_pointer(DB_REASON));
    }
}
