//! `mk_pat_` 的**签发 + 登记**：回放器自己造四档真 PAT，给 `ActorKind::Token` 用。
//!
//! # 这份装配是为了把一条「死变体」变成活的
//!
//! §233.4 之前，`POST /api/tokens/current/renew` 的 8 条 fixture（5 条期望 200、3 条期望
//! 401）**全部**拿到 400，而且 8 条同一个数字、与期望值无关 ⇒ 请求根本没进业务逻辑：
//! 路由第一件事是 `bearer_token(&headers)`（`pats.rs:286-291`），而回放按
//! `actor.kind = member` 注入了 `X-Multica-Session`，**一个 `Authorization` 都没有**。
//!
//! 根因在抽取面：上游那 8 条测试确实把 PAT 放上了线
//! （`newRenewRequest`：`req.Header.Set("Authorization", "Bearer "+rawToken)`），但明文来自
//! `auth.GeneratePATToken()` 的随机值 —— 走查解不出 ⇒ 整个 header 被丢掉，契约里连
//! 一个字段都没有留下。抽取器修好之后（[`crate::bindings`] 侧看 `$testPAT*` 符号），
//! 缺的就只剩「回放器去把那一档 PAT 真的签出来」。
//!
//! # 四档状态，不是一枚令牌
//!
//! 断言 200 还是 401 的是**那一行 PAT 的状态**，不是令牌本身：上游 8 条测试分别铸了
//! 「窗口内 / 窗口外 / 已过期 / 已撤销 / 外键属于另一个用户」几种行。所以本模块按
//! **状态**各签一枚，并让符号（`$testPATValid` …）指到对应那一枚：
//!
//! | 符号 | 库里的行 | 本仓 `renew_current_pat` 的判读 |
//! | --- | --- | --- |
//! | `$testPATValid` | 该用户、未撤销、未过期 | 200（窗口内 `renewed:true`，窗口外 `false`）|
//! | `$testPATExpired` | 该用户、`expires_at` 已过去 | 401（`get_by_token` 的 `expires_at > now()` 过滤）|
//! | `$testPATRevoked` | 该用户、`revoked_at` 非空 | 401（同一句 SQL 的 `revoked_at IS NULL` 过滤）|
//! | `$testPATForeignUser` | **另一个用户**、未撤销、未过期 | ⚠️ 见下 |
//!
//! # 🔴 `$testPATForeignUser` 在**不碰路由**的前提下拿不到 401
//!
//! 上游 `RenewCurrentPersonalAccessToken` 先 `requireUserID(w, r)`（X-User-ID 盖的章），
//! 再比 `pat.UserID` —— 「不许替另一个用户续期」是**盖章链**上的防线。本仓这条路
//! （`pats.rs:280-282` 逐字写明）**只**从 `Authorization` 取身份、从不读 `X-User-ID`，
//! 而 `middleware/authn.rs` 的盖章链本身还挂在 `M9-10`/`W1` 的未接线清单上
//! ⇒ 那枚「属于别人的 PAT」在本仓**就是一枚合法 PAT**，回放只能拿到 200。
//! 本模块仍然如实铸出那一行（另一枚用户行 + 它的 PAT），把差异留成一条**具名**的
//! `mismatch`（`200 != 401`），而不是把它糊成 401：糊过去等于替上游断言一个本仓
//! 从未实现的理由。
//!
//! # 明文只活在内存里
//!
//! 与 [`crate::daemon_token`] 同一条纪律：库里的 `token_hash` 是 sha256 hex，明文
//! （`mk_pat_` + 64 hex）**只在进程内存里**，`Debug` 手写脱敏、报告不登记 —— 报告
//! `report.json` 是提交物，明文令牌进版本库就是一个长期泄密面。
//!
//! 与 [`crate::bindings`] 的分工：本模块**签**，`Bindings` **持有**（`$testPAT*` 是
//! 符号表里的身份两类之外唯一一个「明文凭据」符号），`crate::plan` 把它装配成
//! `Authorization: Bearer …`。

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use mc_core::Id;
use mc_repos::pat::{NewPat, PatRepo};
use mc_repos::user::{NewUser, UserRepo};
use uuid::Uuid;

/// 明文前缀。与 `mc_http::routes::pats::PAT_PREFIX` 是**同一个**字面量：路由用
/// `strip_prefix` 决定「这是不是一枚 PAT」，两边分叉的表现是每一条 token fixture 都
/// 悄悄落在 400 上（与 §233.4 的原始症状逐字相同），所以这里引用而不是抄一遍。
pub const PAT_PREFIX: &str = mc_http::routes::pats::PAT_PREFIX;

/// 明文里 hash 覆盖的那一段的长度：32 字节 → 64 个十六进制字符。
const SUFFIX_HEX_LEN: usize = 64;

/// 符号 `$testPATValid`：该用户、未撤销、未过期的 PAT（`Authorization` 上那一枚）。
pub const VALID_SYMBOL: &str = "$testPATValid";
/// 符号 `$testPATExpired`：该用户、`expires_at` 已经过去的 PAT。
pub const EXPIRED_SYMBOL: &str = "$testPATExpired";
/// 符号 `$testPATRevoked`：该用户、`revoked_at` 非空的 PAT。
pub const REVOKED_SYMBOL: &str = "$testPATRevoked";
/// 符号 `$testPATForeignUser`：属于**另一个用户**的 PAT（见模块文档的那条红线）。
pub const FOREIGN_SYMBOL: &str = "$testPATForeignUser";

/// 四种状态各自的符号。抽取器（`scripts/extract_requirements.py::PAT_BINDINGS`）是
/// 另一半真相 —— `every_symbol_is_known_to_the_extractor` 这条单测盯着两边。
pub const SYMBOLS: [&str; 4] = [VALID_SYMBOL, EXPIRED_SYMBOL, REVOKED_SYMBOL, FOREIGN_SYMBOL];

/// 「还剩多久过期」：够远 ⇒ 落到「窗口外、`renewed:false`」那支，够近 ⇒ 落到续期那支。
/// 两支都是 200，所以这个选择只影响响应体（`json_subset` 为空，不参与判定），但挑一个
/// **落在真阈值之内**的值仍然必要：`PAT_RENEW_THRESHOLD_SECS` 是 7 天。
const VALID_TTL_DAYS: i64 = 3;

/// 四档 PAT 的**明文**（`mk_pat_` + 64 hex）。🔴 只在内存里；`Debug` 脱敏。
#[derive(Clone, Default)]
pub struct Credentials {
    valid: String,
    expired: String,
    revoked: String,
    foreign_user: String,
}

impl Credentials {
    /// 该符号指向的那一枚明文（`None` = 不是 PAT 符号）。
    ///
    /// 返回的是整枚明文（**含** `mk_pat_` 前缀）：路由自己 `strip_prefix(PAT_PREFIX)`
    /// 剥前缀，所以这里必须整枚给出去 —— 少一个前缀字面量就是 400。
    #[must_use]
    pub fn secret_for(&self, sym: &str) -> Option<&str> {
        match sym {
            VALID_SYMBOL => Some(&self.valid),
            EXPIRED_SYMBOL => Some(&self.expired),
            REVOKED_SYMBOL => Some(&self.revoked),
            FOREIGN_SYMBOL => Some(&self.foreign_user),
            _ => None,
        }
    }

    /// 探针用：四枚**不落库**的可辨识假明文。
    ///
    /// 存在的理由是 `requirements.rs::credential_table_is_symmetric_with_the_replay_planner`
    /// —— 它要问 `plan` 一句「database 层发得出来吗」，而那条探针不该碰数据库。
    #[must_use]
    pub fn probe() -> Self {
        Self {
            valid: format!("{PAT_PREFIX}probe_valid"),
            expired: format!("{PAT_PREFIX}probe_expired"),
            revoked: format!("{PAT_PREFIX}probe_revoked"),
            foreign_user: format!("{PAT_PREFIX}probe_foreign"),
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 四枚明文一个都不落地：只报哪几档签出来了。
        let redact = |s: &str| {
            if s.is_empty() {
                None
            } else {
                Some(format!("{PAT_PREFIX}<redacted>"))
            }
        };
        f.debug_struct("Credentials")
            .field("valid", &redact(&self.valid))
            .field("expired", &redact(&self.expired))
            .field("revoked", &redact(&self.revoked))
            .field("foreign_user", &redact(&self.foreign_user))
            .finish()
    }
}

/// 造一枚 `mk_pat_` 明文。**不入库**（入库存的是它的哈希，见 [`register`]）。
///
/// 与 [`crate::daemon_token::mint`] 同一手法：两个 v4 UUID 的随机位来自 `getrandom`，
/// 不依赖 `rand`（也就不必给本 crate 加依赖）；`simple()` 去连字符，拼接后正好
/// [`SUFFIX_HEX_LEN`] 个十六进制字符 —— 与 `pats.rs` 的 `generate_pat_secret` 同口径。
#[must_use]
pub fn mint() -> String {
    let mut suffix = Uuid::new_v4().simple().to_string();
    suffix.push_str(&Uuid::new_v4().simple().to_string());
    debug_assert_eq!(suffix.len(), SUFFIX_HEX_LEN);
    format!("{PAT_PREFIX}{}", &suffix[..SUFFIX_HEX_LEN])
}

/// 按**状态**签四枚 PAT 并在 `personal_access_token` 里登记，返回明文。
///
/// `user_id` 必须是**真实存在**的行：`personal_access_token.user_id` 有指向 `"user"(id)`
/// 的外键，而这条外键正是「令牌被限定在某个用户上」的地基 —— 现编一个 UUID 的表现是
/// `create` 直接失败（好过一条永远 401 的假令牌）。
///
/// `$testPATForeignUser` 额外需要**第二个**用户行：`X-User-ID` 仍然是种子用户，而令牌
/// 属于别人 —— 这正是上游那条测试的形状（模块文档说明了本仓为什么只能得到 200）。
pub async fn register(db: &mc_db::pool::Db, user_id: Id) -> Result<Credentials> {
    let repo = PatRepo::new(db.clone());

    let valid = create(
        &repo,
        user_id,
        "conformance-pat-valid",
        Utc::now() + Duration::days(VALID_TTL_DAYS),
    )
    .await?;
    let expired = create(
        &repo,
        user_id,
        "conformance-pat-expired",
        Utc::now() - Duration::hours(1),
    )
    .await?;

    let revoked = mint();
    let row = insert(
        &repo,
        user_id,
        "conformance-pat-revoked",
        &revoked,
        Utc::now() + Duration::days(VALID_TTL_DAYS),
    )
    .await?;
    repo.revoke(row)
        .await
        .context("revoke the conformance PAT (the `$testPATRevoked` credential)")?;

    let foreign_user = UserRepo::new(db.clone())
        .upsert_by_email(NewUser {
            name: "Conformance Other User".into(),
            email: format!("conformance-other-{}@example.com", short_suffix()),
            avatar_url: None,
        })
        .await
        .context("seed the second user the `$testPATForeignUser` credential belongs to")?;
    let foreign = create(
        &repo,
        foreign_user.id,
        "conformance-pat-foreign",
        Utc::now() + Duration::days(VALID_TTL_DAYS),
    )
    .await?;

    Ok(Credentials {
        valid,
        expired,
        revoked,
        foreign_user: foreign,
    })
}

/// 签一枚、登记一枚，返回明文。
async fn create(
    repo: &PatRepo,
    user_id: Id,
    name: &str,
    expires_at: DateTime<Utc>,
) -> Result<String> {
    let raw = mint();
    insert(repo, user_id, name, &raw, expires_at).await?;
    Ok(raw)
}

/// 登记：`token_hash` / `token_last4` 都走 `PatRepo` 的 canonical helper。
///
/// 🔴 口径必须与 `pats.rs:212-228`（签发面）和 `get_by_token`（查询面）**逐字**一致：
/// hash 覆盖的是**去掉前缀**的那一段。错一位的表现是「签发成功、回放 401」，
/// 与「令牌过期」长得一模一样。
async fn insert(
    repo: &PatRepo,
    user_id: Id,
    name: &str,
    raw: &str,
    expires_at: DateTime<Utc>,
) -> Result<Id> {
    let secret = raw.strip_prefix(PAT_PREFIX).unwrap_or(raw);
    let row = repo
        .create(NewPat {
            user_id,
            name: name.to_string(),
            token_hash: PatRepo::hash_token(secret),
            token_last4: PatRepo::last4(secret),
            expires_at,
            scopes: Vec::new(),
        })
        .await
        .with_context(|| format!("register the conformance PAT {name}"))?;
    Ok(row.id)
}

/// 12 个十六进制字符，只用来给第二个用户一个**每次运行都不同**的邮箱。
fn short_suffix() -> String {
    let s = Uuid::new_v4().simple().to_string();
    s[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 符号表的两半（抽取器那半在 Python 里）必须指同一批名字。
    ///
    /// 这条断言替代不了类型系统，但能替代「靠回忆」：改名只改一边的表现是那 8 条
    /// fixture 全部 `unbound symbol` ⇒ `unevaluable`，而 `unevaluable` 与「判过了」
    /// 在总数里长得一样（§205.5）。
    #[test]
    fn every_symbol_is_known_to_the_extractor() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/extract_requirements.py");
        let text = std::fs::read_to_string(&path).expect("the extractor is in the tree");
        for sym in SYMBOLS {
            assert!(
                text.contains(&format!("\"{sym}\":")),
                "{sym} 不在 scripts/extract_requirements.py 的 PAT_BINDINGS 里"
            );
        }
        assert!(
            Credentials::probe().secret_for("$testPATUnknown").is_none(),
            "未知符号必须答 None，而不是猜一枚"
        );
    }

    #[test]
    fn minted_secrets_are_mk_pat_shaped_and_indistinguishable_in_debug() {
        let raw = mint();
        assert!(raw.starts_with(PAT_PREFIX));
        assert_eq!(raw.len(), PAT_PREFIX.len() + SUFFIX_HEX_LEN);
        let debug = format!("{:?}", Credentials::probe());
        assert!(!debug.contains("probe_valid"), "明文不许进 Debug: {debug}");
        assert!(debug.contains("<redacted>"));
    }
}
