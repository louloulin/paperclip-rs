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

use crate::{ActorKind, Fixture, Tier};

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

/// 那些**按设计就不承载上游全量语料**的 golden 根目录（`<路径>` , `<为什么>`）。
///
/// 🔴 这张表存在的原因是 [`REPO_SIDE_PRECONDITIONS`] 的作用域是**全仓**，而 CLI 的
/// `--golden` 可以指向任意一个根。`mc-conformance` 的「每条前提至少命中一个 fixture」
/// 断言（`main.rs`）回答的问题是「**有没有一条前提是死的**」—— 这个问题只对**主语料**
/// （`contracts/golden`，上游 365 条）有意义。对一个**刻意只放本仓自造场景**的根
/// （`contracts/golden-local/**`）再问同一句话，得到的不是「前提写错了」，而是
/// 「这个根本来就不含那条路由」—— 两者形态完全一样，但处置相反。
///
/// **实测踩中的就是这一条**：`contracts/golden-local/{default,token}` 里没有任何
/// `/auth/send-code` fixture（那条在 `contracts/golden/auth/002-TestSendCode-L2154.json`），
/// 于是 `mc_golden_local_check.sh` 的**两个根都在加载期 exit 2**，`T1-12` 判红，
/// 而 T1-12 要问的那件事（自造面 `mismatch == 0 ∧ unmounted == 0`）**一条都没被跑到**。
/// 判红的原因与被测的性质完全无关 —— 这正是 `docs/64` §9.8 反复警告的「没法判定被记成红」。
///
/// 纪律（防止这张表退化成一张万能豁免单）：
///   * **主语料 `contracts/golden` 永不可豁免** —— 它就是这条断言唯一的判据现场；
///   * 每行必须写清理由，且行数是**人工裁定**的：🔴 **严禁从实测值反推生成**
///     （那等于每轮都在判「实测 == 实测」，判据归零 —— 与 `T1-1b` 的占位白名单同一条纪律）；
///   * 豁免只作用于**逐条前提断言**。根里 fixture 一条都没被判定（`totals.fixtures` 对不上）
///     仍然照旧 bail —— 那是 `main.rs` 里另一条、更强的断言。
pub const PARTIAL_GOLDEN_ROOTS: &[(&str, &str)] = &[
    (
        "contracts/golden-local/default",
        "本仓自造面「未配置 cloud」形态：只放 /api/config 键集、feature_flags、/health/realtime \
         三组自造场景，按设计不含上游 auth 路由",
    ),
    (
        "contracts/golden-local/token",
        "本仓自造面「已配置 cloud」形态：同上，且额外要求 X-Multica-Session 场景；同样不含上游 auth 路由",
    ),
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

// ---------------------------------------------------------------------------
// actor 凭据表（`Tier::supports` 与 `plan` 的唯一共同依据）
// ---------------------------------------------------------------------------

/// 一枚 actor 凭据的**供给登记**：这一档身份在哪些层有签发 / 解析面。
///
/// 🔴 这张表存在的唯一理由，是让「这一层判不判」与「请求发不发得出来」读**同一张表**。
/// 两边各说各的那一次留下了 `database_tier_replays_every_decidable_fixture` 恒红的存量
/// 缺陷：`Tier::supports(Tier::Database, Agent)` 答「供得起」，而 `plan` 的
/// `ActorKind::Agent` 分支答「不伪造」—— 两条断言在任何实现下互斥，该用例不可能全绿。
///
/// `satisfied_by` 为空 ⇒ **没有任何层**有签发面 ⇒ 这一档恒 `unevaluable`，且
/// [`actor_credential_detail`] 与 `crate::plan` 必须给出**同一个** `detail`
/// （不是「`supports` 松口、由 `plan` 的 `Err` 兜底」）。
///
/// 🔴 修法不许是「把 `satisfied_by` 放宽」：给 `ActorKind::Agent` 补上 `Tier::Database`
/// 会把 13 条 `agent` 的 `unevaluable` 变成**假 `mismatch`** —— 这一档没有签发面，
/// 回放只能拿到 401/500，而那与「实现写错了」在报告里长得一样。
///
/// 🔴 同一句话对 [`ActorKind::Token`] **只成立到 `LUM-2552` 为止**：那一档当时是「枚举
/// 里有、注释写明了语义、凭据表也有条目」的**死变体**（366 条 golden 里 0 条用它），
/// 于是看起来也该留在 `&[]`。区别在于它不是「本仓没这个能力」，而是「回放器没去调那
/// 个能力」—— `pats.rs:212-228` 早就有现成的签发代码路径。所以放宽它的前提是**同时**
/// 把签发面接上（`crate::pat_token::register` + `plan` 的 `ActorKind::Token` 分支）：
/// 凭据表松口而签发面不接，就是上面那条假 `mismatch`（`credential_table_is_symmetric_`
/// `with_the_replay_planner` 单测会在 `(true, Err)` 那一支直接红）。
pub const ACTOR_CREDENTIALS: &[ActorCredential] = &[
    ActorCredential {
        kind: ActorKind::Anonymous,
        satisfied_by: &[Tier::Stateless, Tier::Database],
        detail: "",
    },
    ActorCredential {
        kind: ActorKind::Member,
        satisfied_by: &[Tier::Database],
        detail: "member identity is a seeded user row; the stateless tier has no user table, \
                 so there is nothing to resolve X-User-ID against",
    },
    ActorCredential {
        kind: ActorKind::Agent,
        // `LUM-2560`：这一档不再是「没有凭证面」。本仓的 agent 身份面是**两段**：
        // ① `X-User-ID` → 会员会话（AuthUser 只认 `X-Multica-User-Id`，M1 dev-mode 契约）；
        // ② `/api/chat/**` 直接读 `X-Actor-Source` + `X-Task-ID`（`chat/task/history.rs:144`）。
        // 两段都不需要新签什么凭证，只需要 database 层**把实体行种下来**（`X-Task-ID`
        // 指到一行真的 `agent_task_queue`）。`X-Agent-ID` 按上游原样转发：它在本仓
        // 没有解析面（`routes/agents.rs:42`），伪造它等于改掉 `…RejectsForgedAgentIDHeader`
        // 那条断言的前提。
        satisfied_by: &[Tier::Database],
        detail: "agent identity is a seeded user session (`X-User-ID` → `X-Multica-User-Id`) \
                 plus the `X-Actor-Source` / `X-Task-ID` pair `/api/chat/**` reads directly; \
                 the stateless tier has no user or task table to resolve either against",
    },
    ActorCredential {
        kind: ActorKind::Token,
        // `LUM-2552`：这一档不再是「没有签发面」。database 层在 `database_router` 里按
        // **状态**各签一枚 `mk_pat_`（`crate::pat_token::register`），`plan` 把
        // `$testPAT<State>` 装配成 `Authorization: Bearer …`。
        satisfied_by: &[Tier::Database],
        detail:
            "token identity is resolved from the `personal_access_token` row the `Authorization` \
                 header hashes to; the stateless tier has no token table and mints none",
    },
    ActorCredential {
        kind: ActorKind::Daemon,
        satisfied_by: &[Tier::Database],
        detail: "daemon identity is resolved from the daemon_token table; the stateless tier \
                 cannot mint one and has none to look up",
    },
    ActorCredential {
        kind: ActorKind::System,
        satisfied_by: &[],
        detail: "actor kind system is internal-only",
    },
];

/// 一条凭据登记项。见 [`ACTOR_CREDENTIALS`]。
#[derive(Debug, Clone, Copy)]
pub struct ActorCredential {
    /// fixture 里的 `actor.kind`。
    pub kind: ActorKind,
    /// 哪些层有签发 / 解析面；空 = 没有任何层有。
    pub satisfied_by: &'static [Tier],
    /// 供不起时报告里写的那句话（`satisfied_by` 覆盖全部层时可以为空）。
    pub detail: &'static str,
}

/// 按 actor kind 取登记项。查不到 = 凭据表漏了这一档（第二道；加载期由
/// [`credential_table_covers_every_kind`] 的同形断言先拦一次）。
pub fn actor_credential(kind: ActorKind) -> Result<&'static ActorCredential, String> {
    ACTOR_CREDENTIALS
        .iter()
        .find(|c| c.kind == kind)
        .ok_or_else(|| format!("actor kind {} has no credential entry", kind.as_str()))
}

/// 这一层供得起这一档身份吗 —— [`crate::Tier::supports`] 的**唯一**依据。
pub fn credential_satisfied_by(kind: ActorKind, tier: Tier) -> Result<bool, String> {
    Ok(actor_credential(kind)?.satisfied_by.contains(&tier))
}

/// 这一层判不了这一档身份时的**报告理由** —— `crate::plan` 拒绝「无签发面」的档时
/// 给的是同一句（`satisfied_by.is_empty()` 那半支）。
///
/// 两种情形必须分开说，否则读者会顺着一条假线索去 rerun `--db-url`：
/// * 别的层有签发面 ⇒ 指路（换到那层去）；
/// * **没有任何层**有 ⇒ 说清「没有签发面」，因为换层也判不了。
pub fn actor_credential_detail(kind: ActorKind, tier: Tier) -> Result<String, String> {
    let c = actor_credential(kind)?;
    if c.satisfied_by.is_empty() {
        return Ok(c.detail.to_string());
    }
    let elsewhere: Vec<&str> = c.satisfied_by.iter().copied().map(Tier::as_str).collect();
    Ok(format!(
        "{} actor needs the {} tier (rerun with --db-url); {} tier cannot decide it",
        kind.as_str(),
        elsewhere.join(" or "),
        tier.as_str()
    ))
}

/// **请求面**的判定闸门（与凭据面、前提面并列的第三条）：这一条 fixture 的请求本身
/// 发不发得出来。
///
/// `plan()` 只对 `path_params` / `query` 的**取值**做 percent-encode（`query` 的 key 也编），
/// 路径里的字面量原样进 URI ⇒ 字面量带空格这类字节会被 axum 在 `RequestPlan::to_http`
/// 拒掉（`cannot build request: invalid uri character`）。
///
/// 这一问答否 ⇒ [`crate::Tier::supports`] 必须也答否。否则「这一层供得起」与「请求发得
/// 出来」又各说一套 —— 与凭据面那个缺陷同族，只是闸门不在凭据面而在请求面。
/// 实证：`agents/TestUpdateAgent_KeepsMcpConfigForMemberActor@…:1423#23` 就是一条
/// （上游那条测试 PUT 了一个字面带空格的路径）；它**不是本仓实现写错**，
/// 所以只能被声明为「两层都判不了」，不能反过来变成假 `mismatch`。
#[must_use]
pub fn request_target_is_encodable(fx: &Fixture) -> bool {
    // 与 `plan` 同一条拼法：先按 `path_params` 把占位符换掉（取值由 `plan` percent-encode，
    // 这里只需把占位符本身去掉），剩下的字面量必须自己就是合法 URI 字节。
    let mut literal = fx.path.clone();
    for name in fx.path_params.keys() {
        literal = literal.replace(&format!("{{{name}}}"), "");
    }
    // 未填充的占位符同样是「发不出来」（`plan` 会以 `still has an unfilled placeholder` 拒）。
    if literal.contains('{') || literal.contains('}') {
        return false;
    }
    axum::http::Uri::try_from(literal.as_str()).is_ok()
}

/// 请求面没过时的报告理由。与 [`request_target_is_encodable`] 成对。
///
/// 措辞要与「缺身份 / 缺前提」分开：这条红是**fixture 自己**的（抽取器把一个字面带
/// 空格的路径原样写进了 `path`），换层、换凭据都没用。
#[must_use]
pub fn unplannable_request_detail(fx: &Fixture) -> String {
    format!(
        "this fixture's request cannot be built on any tier: `{}` carries bytes axum refuses \
         in a URI (plan fails with `invalid uri character`), or still has an unfilled \
         placeholder — the gap is in the fixture, not in a precondition or a credential",
        fx.path
    )
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

    /// 豁免单不能变成万能豁免：主语料一旦被列进去，`main.rs` 的逐条前提断言就被永久关闭，
    /// 而它**没有第二个判据现场**。这是这张表唯一的不安全方向（路径拼错的方向是安全的：
    /// 匹配不上 ⇒ 断言照旧生效），所以只钉这一条。
    #[test]
    fn the_primary_corpus_is_never_exempt() {
        assert!(
            !PARTIAL_GOLDEN_ROOTS
                .iter()
                .any(|(root, _)| *root == "contracts/golden"),
            "PARTIAL_GOLDEN_ROOTS 列了主语料 contracts/golden"
        );
    }

    /// 豁免单的每一行都必须指向一个**真实存在**的 golden 根，且写清了理由 ——
    /// 否则它会退化成「凭一条注释关掉一道判据」。
    #[test]
    fn every_exempted_root_exists_and_states_why() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (root, why) in PARTIAL_GOLDEN_ROOTS {
            assert!(
                base.join(root).is_dir(),
                "PARTIAL_GOLDEN_ROOTS 列了不存在的根 {root}"
            );
            assert!(!why.trim().is_empty(), "{root}: 豁免必须写理由");
        }
    }

    /// 豁免的**收益**必须是真的：被豁免的根按设计就不含任何 `REPO_SIDE_PRECONDITIONS`
    /// 条目的路由。若哪天有人给这些根补上了那条 fixture，这行豁免就该被划掉
    /// （否则就是「有判据现场却不判」——§199 那个形态从另一个方向回来了）。
    #[test]
    fn exempted_roots_really_carry_none_of_the_preconditions() {
        for (root, _) in PARTIAL_GOLDEN_ROOTS {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(root);
            let fixtures = crate::load_dir(&dir).expect("partial root loads");
            for (method, path, _ids) in REPO_SIDE_PRECONDITIONS {
                let hits = fixtures
                    .iter()
                    .filter(|fx| fx.method.eq_ignore_ascii_case(method) && fx.path == *path)
                    .count();
                assert_eq!(
                    hits, 0,
                    "{root} 现在含有 {method} {path} 的 fixture ⇒ \
                     PARTIAL_GOLDEN_ROOTS 里这一行应当划掉（豁免的前提已不成立）"
                );
            }
        }
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

    // -----------------------------------------------------------------------
    // 凭据表（§216）
    // -----------------------------------------------------------------------

    /// `ActorKind` 的全部档。枚举一变（增档）这里就红 —— 这是故意的：
    /// 凭据表漏一档 = 那个档在两层都不被任何判据管，且报告里没有理由。
    const ALL_KINDS: [ActorKind; 6] = [
        ActorKind::Anonymous,
        ActorKind::Member,
        ActorKind::Agent,
        ActorKind::Token,
        ActorKind::Daemon,
        ActorKind::System,
    ];

    /// 一档身份的探针 fixture：不引任何种子符号，只为问 `plan` 一句「发得出来吗」。
    ///
    /// `Token` 档是唯一一个非空 `upstream_identity` 的探针：那一档的凭据**就是**
    /// `Authorization` 上那枚 PAT（`$testPAT<State>` 符号 → [`crate::pat_token::Credentials`]），
    /// 而 `plan` 的 token 分支只有看到这个 header 才装得出来。给一条空身份的探针，等于
    /// 拿一个本仓根本不会发的请求去问「表与 `plan` 对不对称」—— 那会在 `(true, Err)` 那支
    /// 报一条与真实不对称毫无关系的红。
    fn probe(kind: ActorKind) -> Fixture {
        use std::collections::BTreeMap;
        let identity = match kind {
            ActorKind::Token => BTreeMap::from([(
                "Authorization".to_string(),
                crate::pat_token::VALID_SYMBOL.to_string(),
            )]),
            _ => BTreeMap::new(),
        };
        Fixture {
            schema_version: crate::SCHEMA_VERSION,
            id: format!("probe/{}", kind.as_str()),
            method: "GET".into(),
            path: "/probe".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            actor: crate::Actor {
                kind,
                upstream_identity: identity,
                identity_source: None,
            },
            body: None,
            expect: crate::Expect {
                status: 200,
                json_subset: serde_json::Value::Null,
                headers: BTreeMap::new(),
            },
            source: crate::Source {
                file: "requirements.rs".into(),
                line: 1,
                test: "probe::symmetry".into(),
                site: "probe".into(),
                via: String::new(),
                commit: String::new(),
            },
            extraction: crate::Extraction::default(),
        }
    }

    #[test]
    fn credential_table_covers_every_kind() {
        assert_eq!(
            ACTOR_CREDENTIALS.len(),
            ALL_KINDS.len(),
            "凭据表与 ActorKind 的档数不一致（重复项或漏项）"
        );
        for kind in ALL_KINDS {
            let c = actor_credential(kind).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert_eq!(c.kind, kind, "凭据表里 {kind:?} 的条目对不上自己");
        }
    }

    #[test]
    fn credential_detail_explains_every_gap() {
        for c in ACTOR_CREDENTIALS {
            for tier in [Tier::Stateless, Tier::Database] {
                if c.detail.is_empty() {
                    // 空理由 ⇔ **每一层**都供得起（空理由不可能出现在报告里）。
                    assert!(
                        credential_satisfied_by(c.kind, tier).expect("登记项在手"),
                        "{:?}/{tier:?}: 供不起却没有理由",
                        c.kind
                    );
                } else {
                    assert!(
                        c.detail.trim().len() > 25,
                        "{:?}: 有供不起的层，却写不出能独立读懂的理由",
                        c.kind
                    );
                }
            }
        }
    }

    /// 🔴 本片的核心判据（**双向**）：`Tier::supports`（判不判）与 `crate::plan`
    /// （请求发不发得出来）必须读**同一张**凭据表 —— 任何一边自己另说一套，这条就红。
    ///
    /// 反向演示：给表里任意一档写上它 `plan` 分支并不支持的层（例如给 `Agent` 补上
    /// `Tier::Database`，或新增一个只有表项、`plan` 里只会拒绝的档）⇒ 第 ① 条直接红。
    #[test]
    fn credential_table_is_symmetric_with_the_replay_planner() {
        let stateless = crate::Bindings::stateless();
        let database = crate::Bindings::with_daemon_token(
            stateless.user_id,
            stateless.workspace_id,
            "mdt_probe".into(),
        )
        // `Token` 档的凭据（`$testPAT<State>`）也要挂上：`with_daemon_token` 只给 `mdt_`，
        // 少了这一件，探针就变成「缺凭据的那一条」，问出来的红与真实的不对称无关。
        .with_pat_tokens(crate::pat_token::Credentials::probe());
        for c in ACTOR_CREDENTIALS {
            let fx = probe(c.kind);
            for (tier, bindings) in [(Tier::Stateless, &stateless), (Tier::Database, &database)] {
                let declared = credential_satisfied_by(c.kind, tier).expect("登记项在手");
                match (declared, crate::plan(&fx, bindings)) {
                    // ① 表说供得起 ⇒ `plan` 必须发得出来。不对称的那一次就是在这里红的。
                    (true, Ok(_)) => {}
                    (true, Err(e)) => panic!(
                        "{:?}/{tier:?}: 表说供得起，plan 却拒绝（判据不对称）：{e}",
                        c.kind
                    ),
                    // ② 表说供不起、`plan` 却发得出请求 ⇒ 只有当**别的层**有签发面时才成立
                    //    （层闸门由 `Tier::supports` 把守）；一个签发面都没有的档这样就是表漏了。
                    (false, Ok(_)) => assert!(
                        !c.satisfied_by.is_empty(),
                        "{:?}/{tier:?}: 表说没有任何层供得起，plan 却发得出请求",
                        c.kind
                    ),
                    // ③ 「一个签发面都没有」的档必须拒绝，且理由与报告理由逐字相同。
                    (false, Err(e)) => {
                        if c.satisfied_by.is_empty() {
                            assert_eq!(
                                e, c.detail,
                                "{:?}: plan 的拒绝理由不是凭据表那一句",
                                c.kind
                            );
                            assert_eq!(
                                actor_credential_detail(c.kind, tier).expect("登记项在手"),
                                c.detail,
                                "{:?}/{tier:?}: 报告理由与凭据表那一句不一致",
                                c.kind
                            );
                        }
                    }
                }
            }
        }
    }

    /// 有签发面的档：报告理由必须**指路**（换到有签发面的那层），而不是说「没有签发面」。
    #[test]
    fn credential_detail_points_at_the_tier_that_can_supply_it() {
        let d = actor_credential_detail(ActorKind::Member, Tier::Stateless).expect("登记项在手");
        assert_eq!(
            d,
            "member actor needs the database tier (rerun with --db-url); stateless tier cannot \
             decide it",
            "stateless 层对 member 的理由变了 —— 这条会顺带给快照报告换字"
        );
    }

    /// §216 同族第二条（**请求面**）：请求拼不出合法 URI 的 fixture，两层都必须答否 ——
    /// 不许「`supports` 说供得起、`plan` 说拼不出 URI」。用**真 golden 面**判，不现编一条。
    #[test]
    fn an_unencodable_request_is_declared_undecidable() {
        let spacey: Vec<Fixture> = golden()
            .into_iter()
            .filter(|fx| !request_target_is_encodable(fx))
            .collect();
        assert!(
            !spacey.is_empty(),
            "golden 面里找不到请求拼不出 URI 的那条（路径字面带空格）—— 本判据会变成空的"
        );
        for fx in &spacey {
            assert!(
                !unplannable_request_detail(fx).trim().is_empty(),
                "{}: 请求面没过却写不出理由",
                fx.id
            );
            for tier in [Tier::Stateless, Tier::Database] {
                assert!(
                    !tier.supports(fx).expect("前提 id 都在登记表里"),
                    "{}: {tier:?} 声明供得起，但请求拼不出合法 URI（判据不对称）",
                    fx.id
                );
            }
        }
    }
}
