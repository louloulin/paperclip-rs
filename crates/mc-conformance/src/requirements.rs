//! 场景前提（`requires`）：一条 fixture 的期望值背后的场景，回放前必须先凑齐。
//!
//! 见 `lib.rs` 的模块文档「`requires`：先问「这一层能不能判定」，再回放」。本模块是那份
//! 文档的**可执行形态**：一张前提登记表（[`REQUIREMENTS`]）、本仓自己的那一份
//! （[`REPO_SIDE_PRECONDITIONS`]），以及两个查询入口（[`missing_requirements`] /
//! [`requirements_detail`]）。
//!
//! 单独成文件而不是塞在 `lib.rs` 里，有两个理由：
//! 1. **它是数据，不是逻辑。** 九条前提各自一句「为什么这一层供不起」，那九句话是本轮
//!    唯一的调研产出；混在回放逻辑里，下一个改 `run_tier` 的人会顺手把它们删掉。
//! 2. `lib.rs` 已在 R7 的 800 行存量白名单里（`scripts/file_size_baseline.tsv` 记 1024 行，
//!    且**只减不增**）。加机制就得同时把文件拆回基线以下，否则门 ⑩ 直接红。

use crate::{Fixture, Tier};

// ---------------------------------------------------------------------------
// 前提（`requires`）
// ---------------------------------------------------------------------------

/// 一条 fixture 的前提由**两个来源**合成，两者在报告里合成一条 `requires`：
///
/// * **上游注入的**（`extraction.requires`，由抽取器写）：上游测试在驱动 handler 之前
///   装配了什么。这不是本仓能猜的，只能由读上游代码的人/工具记下来。
/// * **本仓的**（[`REPO_SIDE_PRECONDITIONS`]）：同一个请求在**本仓**的实现上，第一步就
///   需要什么。`POST /auth/send-code` 在上游是匿名可达的（上游那条 handler 不查库），
///   而本仓的 `send_code` 走 `VerificationCodeRepo` 两次查库 ⇒ 在 stateless 层必然 500。
///   这条事实只属于本仓，所以它住在这里而不是抽取器里。
///
/// 形态上刻意做成**按 (method, path) 的常量表**而不是一张要解析的文件：条目少、可 grep、
/// 类型检查过；`--check` 的逐字节比对加上 [`requirements`] 的「每条至少命中一个 fixture」
/// 断言一起防漂移（表里写错的 path 会在加载期报错，不会静默变成一个永远不被评估的声明）。
pub const REPO_SIDE_PRECONDITIONS: &[(&str, &str, &[&str])] = &[
    // `routes/auth.rs::send_code` —— `VerificationCodeRepo::new(state.db)` +
    // `recent_for()` 两次查库。stateless 层的连接池指向不可达端口（`harness::STATELESS_URL`），
    // 不拨号也不建库 ⇒ 查询失败 ⇒ `Error::Database` ⇒ 500，而期望是 200。
    // 这就是 §201.2 的子根因 C：**tier 标错，不是实现写错**。
    ("POST", "/auth/send-code", &["database"]),
];

/// 前提 id → (哪些层供得起它, 缺它时报告里写什么)。
///
/// `satisfied_by` 为空 ⇒ **没有任何一层**能供得起 ⇒ 该 fixture 恒 `unevaluable`。
/// 恒不可判定不是缺陷，是**诚实的记账**：这些场景需要**测试替身**
/// （`cloud_runtime_stub` / `db_fault_injection` / 拒绝一切的限流器）——
/// 而替身是上游单测的内部结构，不是本仓 router 的输入。硬造一个替身来把它们「判成通过」
/// 就是在测自己造的假货，正是 §201.2 警告的形态。
///
/// 🔴 但「恒不可判定」与「**缺装配**」是两回事（本表唯一一条 `daemon_token` 曾是后者）：
/// 凡是本仓**已经有**签发面 / 解析面、只是回放器没去调它的前提，都不属这一类 ——
/// `daemon_token` 已由 [`crate::daemon_token`] 补上（§205）。判别式：**问「这条判据在
/// 什么实现下会 FAIL」**；答不上来的多半就是一张本不该留着的空条。
pub const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        id: "daemon_token",
        satisfied_by: &[Tier::Database],
        detail: "upstream put a daemon identity in the request context (middleware.WithDaemonContext), \
                 not in a header: this repo resolves it from the daemon_token table instead, and the \
                 database tier mints and registers an mdt_ token per replay (crate::daemon_token) so \
                 the request carries a credential this repo can actually look up; the stateless tier \
                 has no pool to register into and cannot decide it",
    },
    Requirement {
        id: "browser_session_cookie",
        satisfied_by: &[],
        detail: "upstream authenticated this request with a signed session cookie / JWT through \
                 middleware.Auth, which is not a header the extractor can replay; this repo's session \
                 middleware takes X-Multica-Session, and forging a cookie would assert nothing about \
                 this repo",
    },
    Requirement {
        id: "db_fault_injection",
        satisfied_by: &[],
        detail: "upstream's expected status is produced by an injected mock query layer (mockDB); \
                 a real pool cannot be told to fail on demand, and an unreachable pool would answer 500 \
                 for the wrong reason",
    },
    Requirement {
        id: "bare_handler_no_wiring",
        satisfied_by: &[],
        detail: "upstream drove a bare &Handler{} and the expectation *is* the missing wiring (a 5xx from \
                 a collaborator that was never constructed); the replay always drives the fully \
                 assembled router, where that state does not exist",
    },
    Requirement {
        id: "cloud_runtime_configured",
        satisfied_by: &[Tier::Stateless],
        detail: "upstream ran this with a cloud runtime configured; the stateless tier's default \
                 deployment has none (that is the third scenario, and \
                 TestStripeWebhookDisabledReturnsForbidden asserts it), so this fixture is replayed \
                 against a second, cloud-configured router",
    },
    Requirement {
        id: "cloud_runtime_stub",
        satisfied_by: &[],
        detail: "upstream's expectation is the traffic a fake cloud proxy recorded (proxy.req / proxy.resp), \
                 so it asserts the stub, not this repo's forwarding; a real (or unreachable) cloud \
                 transport would answer for a different reason",
    },
    Requirement {
        id: "webhook_rate_limiter_denying",
        satisfied_by: &[],
        detail: "upstream installed a denyingWebhookIPRateLimiter double, so the 429 is the double's \
                 verdict; this repo's limiter is a real process-wide singleton that allows the first N \
                 requests, and the replay carries no peer address (so the gate is skipped outright, \
                 exactly as upstream does when the client IP is unknown)",
    },
    Requirement {
        id: "external_oauth",
        satisfied_by: &[],
        detail: "upstream's expectation depends on a stubbed Google OAuth round-trip: client id/secret \
                 env, an injected HTTP client, and a user row — none of which is a router input",
    },
    Requirement {
        id: "database",
        satisfied_by: &[Tier::Database],
        detail: "this repo's handler queries the database before it can decide anything, and the \
                 stateless pool is a lazy pool on an unreachable port (it never dials and never creates \
                 a schema), so the query fails and the request answers 500 for an infrastructure reason",
    },
];

/// 一条前提的登记项。见 [`REQUIREMENTS`]。
#[derive(Debug, Clone, Copy)]
pub struct Requirement {
    /// 写进 `extraction.requires` / 报告行的稳定 id。
    pub id: &'static str,
    /// 哪些层能供得起它；空 = 没有任何层能。
    pub satisfied_by: &'static [Tier],
    /// 供不起时报告里写的那句话（要能独立读懂，不依赖上下文）。
    pub detail: &'static str,
}

pub fn requirement(id: &str) -> Option<&'static Requirement> {
    REQUIREMENTS.iter().find(|r| r.id == id)
}

/// 一条 fixture 在某一层**凑不齐**的前提；全齐则 `None`。
///
/// 未知 id 在这里是**加载期错误**而不是「当作没有」—— 抽取器与回放器各写一半的 id
/// 表若不同名，那是一条拼写错误，不是一条可以默默跳过的声明。
pub fn missing_requirements(fx: &Fixture, tier: Tier) -> Result<Vec<&'static str>, String> {
    let mut out = Vec::new();
    for id in fx.requirement_ids() {
        match requirement(id) {
            None => return Err(format!("{}: unknown requirement id {id:?}", fx.id)),
            Some(r) if !r.satisfied_by.contains(&tier) => out.push(r.id),
            Some(_) => {}
        }
    }
    Ok(out)
}

/// `missing_requirements` 的人类可读形式，写进报告的 `detail`。
pub fn requirements_detail(fx: &Fixture, tier: Tier) -> String {
    use std::fmt::Write as _;
    let ids = fx.requirement_ids().join(", ");
    let missing = match missing_requirements(fx, tier) {
        Ok(m) => m,
        Err(e) => return e,
    };
    if missing.is_empty() {
        return format!("this tier supplies every precondition the scenario needs ({ids})");
    }
    let mut out = format!(
        "this scenario needs {} the {} tier does not supply",
        if missing.len() == 1 {
            "precondition".to_string()
        } else {
            "preconditions".to_string()
        },
        tier.as_str()
    );
    for id in &missing {
        // `write!` 而不是 `push_str(&format!(..))`（clippy::format_push_string）。
        let _ = write!(
            out,
            "\n  - {id}: {}",
            requirement(id).map_or("unregistered requirement", |r| r.detail)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实的 golden 目录（而不是现编一个 fixture）：本片要判的是「那 20 条」，
    /// 现编一条只会证明现编的那条。
    fn golden() -> Vec<Fixture> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden");
        crate::load_dir(&dir).expect("contracts/golden loads")
    }

    fn daemon_token_fixtures() -> Vec<Fixture> {
        let all: Vec<Fixture> = golden()
            .into_iter()
            .filter(|fx| fx.requirement_ids().contains(&"daemon_token"))
            .collect();
        assert!(
            !all.is_empty(),
            "no fixture declares daemon_token — §205 的前提表与 golden 面脱节了"
        );
        all
    }

    #[test]
    fn daemon_token_is_supplied_by_the_database_tier_only() {
        let r = requirement("daemon_token").expect("daemon_token is registered");
        // 🔴 判据 2：必须是 `&[Tier::Database]` 单层。写进 Stateless 会把一条
        // 「stateless 供不起」的前提说成供得起 —— 那一层的池指向不可达端口，
        // 登记与查表都做不了，写进去等于删掉这条前提的鉴别力。
        assert_eq!(r.satisfied_by, &[Tier::Database]);
        assert!(
            !r.satisfied_by.contains(&Tier::Stateless),
            "stateless tier cannot register or look up a daemon token"
        );
    }

    #[test]
    fn daemon_token_fixtures_are_all_daemon_actor_and_no_longer_blocked_by_it() {
        let mut only_daemon_token = 0usize;
        for fx in daemon_token_fixtures() {
            assert_eq!(
                fx.actor.kind,
                crate::ActorKind::Daemon,
                "{}: 声明了 daemon_token 前提却不是 daemon actor",
                fx.id
            );
            // 判据 1 的口径：`daemon_token` 本身不再出现在 database 层的缺口清单里。
            // 别的前提（`db_fault_injection` —— 那是上游 mockDB 的产物，本仓真池
            // 供不起）仍会让个别 fixture 整体不可判定，那是**另一条**判据的事。
            let missing_db =
                missing_requirements(&fx, Tier::Database).expect("known requirement id");
            assert!(
                !missing_db.contains(&"daemon_token"),
                "{}: database 层仍缺 daemon_token",
                fx.id
            );
            if missing_db.is_empty() {
                only_daemon_token += 1;
            }
            // stateless 层两道关都答否 ⇒ 仍不可判定（`satisfied_by` 刻意不含 Stateless）。
            let missing_st =
                missing_requirements(&fx, Tier::Stateless).expect("known requirement id");
            assert!(!missing_st.is_empty(), "{}: stateless 层不应判定它", fx.id);
        }
        // 防止「缺口只是从 daemon_token 换成了别的 id」这种平移被当成进展。
        assert_eq!(
            only_daemon_token,
            daemon_token_fixtures().len() - 2,
            "预期只有那 2 条与 db_fault_injection 并存的仍不可判定"
        );
    }

    #[test]
    fn every_requirement_detail_explains_itself() {
        // 前提表是「恒不可判定」的登记处；空 detail 会让不可判定的理由变成空白
        // （`main.rs` 有一条 `unevaluable with no reason recorded` 的兜底断言）。
        for r in REQUIREMENTS {
            assert!(r.detail.len() > 40, "{}: detail 太短", r.id);
        }
    }
}
