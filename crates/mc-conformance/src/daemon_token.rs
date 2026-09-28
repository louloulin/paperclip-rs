//! `daemon_token` 的**签发 + 登记**：回放器自己造一个 `mdt_` 身份。
//!
//! # 这份装配是为了把一句话变成假的
//!
//! §201.2 子根因 A 之前，20 条 daemon 场景的 `requires` 挂着一条**恒不可满足**的前提，
//! 理由逐字写在 `REQUIREMENTS::daemon_token` 的 `detail` 里：「harness 不会凭空造一个
//! `mdt_` 令牌并在库里登记它」。恒不可判定是诚实的记账，但**只要缺的是装配而不是能力**，
//! 它就是一条迟早要还的债 —— 本仓的解析链（`mc_http::routes::daemon::scope::authenticate`）
//! 和写入面（`DaemonRepo::insert_daemon_token`）本来就都齐了，缺的只是回放器去调它们。
//!
//! # 三件事，且不新增任何领域概念
//!
//! 1. **签发**：[`mint`] 造一个 `mdt_` + 随机后缀；哈希走
//!    [`mc_repos::daemon::hash_daemon_token`]（`hex(sha256(明文))`），与解析面同一算法。
//! 2. **登记**：[`register`] 往 `daemon_token` 插一行，`workspace_id` = 种子 workspace，
//!    `daemon_id` 用一个**固定可读**值，TTL 见 [`TOKEN_TTL_SECS`]。
//! 3. **回放时带上身份**：`plan()` 在 `actor == daemon` 的 fixture 上加
//!    `Authorization: Bearer <明文>`（见 `crate::Bindings::daemon_token`）。
//!
//! # 🔴 明文只活在内存里
//!
//! [`RegisteredToken`] 的 `Debug` 是**手写**的：打出来的永远是 `mdt_<redacted>`。
//! 报告 `report.json` 是提交物，明文令牌进版本库就是一个长期泄密面 —— 所以
//! [`crate::Report::from_rows`] 只登记 `user_id` / `workspace_id` 两个绑定，
//! 令牌连字段都不给它。报告里能看到的最多是「这条 fixture 用的是本次回放现场造的那个身份」，
//! 由 `daemon_seed_id()` 这类 id 级信息承载，不是凭据本身。

use anyhow::{Context, Result};
use uuid::Uuid;

use mc_core::Id;
use mc_repos::daemon::{hash_daemon_token, DaemonRepo};

/// 凭据前缀。解析面 `DaemonAuth::authenticate` **按前缀分流**（`mdt_` 查
/// `daemon_token` / `mul_` 查 PAT / 其余 fail-closed），所以这不是命名口味：
/// 前缀错了，令牌会落到 PAT 分支上去，然后以「查无此行」的形式 401。
pub const DAEMON_TOKEN_PREFIX: &str = "mdt_";

/// 登记时写进 `daemon_token.daemon_id` 的固定值。
///
/// 刻意用**可读常量**而不是随机值：daemon 面的多条 handler 会把 `daemon_id` 回显进
/// 响应体（上游 `GetTask` 的 daemon 归属断言就是比它），随机值会让「同一身份」的断言
/// 只能靠回显自证，而这个常量让「就是同一个身份」一眼可核。
pub const DAEMON_ID: &str = "conformance-daemon";

/// 令牌有效期（秒）。
///
/// 取 1 小时而不是「永久」：过期判定是这条装配**唯一有鉴别力的分支**
/// （`scope::authenticate` 里 `row.expires_at <= now()` ⇒ 401），所以必须让过期这件事
/// 在一次回放里是**可达**的 —— 一个永不过期的令牌会让那条分支永远不会被执行。
pub const TOKEN_TTL_SECS: i64 = 3_600;

/// 随机后缀的十六进制长度（两个 v4 UUID = 256 bit 熵）。
const SUFFIX_HEX_LEN: usize = 64;

/// 造一个 `mdt_` 明文令牌。**不入库**（入库存的是它的哈希，见 [`register`]）。
#[must_use]
pub fn mint() -> String {
    // 两个 v4 UUID 的随机位来自 `getrandom`，不依赖 `rand` crate（也就不必给本 crate
    // 加依赖）。`simple()` 去连字符，拼接后正好 [`SUFFIX_HEX_LEN`] 个十六进制字符。
    let mut suffix = Uuid::new_v4().simple().to_string();
    suffix.push_str(&Uuid::new_v4().simple().to_string());
    debug_assert_eq!(suffix.len(), SUFFIX_HEX_LEN);
    format!("{DAEMON_TOKEN_PREFIX}{}", &suffix[..SUFFIX_HEX_LEN])
}

/// 一次登记的结果。
///
/// `raw` 是**明文凭据**，只该流向两处：`daemon_token` 表的哈希（经 [`register`]）
/// 与回放请求的 `Authorization` 头（经 `crate::Bindings`）。任何其他去处都是泄密面，
/// 所以 `Debug` 手写脱敏，而不是 `#[derive(Debug)]`。
pub struct RegisteredToken {
    /// 明文 `mdt_…`。🔴 不写进 `report.json`。
    pub raw: String,
    /// 登记到的 `daemon_token.id`（报告里可以出现这个 —— 它是 id 级信息，不是凭据）。
    pub id: Id,
}

impl std::fmt::Debug for RegisteredToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredToken")
            .field("raw", &format!("{DAEMON_TOKEN_PREFIX}<redacted>"))
            .field("id", &self.id)
            .finish()
    }
}

/// 签发一枚令牌并在 `daemon_token` 表登记它，返回明文。
///
/// `workspace_id` 必须是一条**真实存在**的 workspace —— `daemon_token.workspace_id`
/// 有 `REFERENCES workspace(id)` 外键，而这条外键正是断言「daemon 身份被限制在某个
/// workspace 内」的地基（上游 `TestGetIssueGCCheck_WithDaemonToken_CrossWorkspace`
/// 依赖它）。所以本函数不接受「现编一个 UUID」这种用法。
pub async fn register(repo: &DaemonRepo, workspace_id: Id) -> Result<RegisteredToken> {
    let raw = mint();
    let id = repo
        .insert_daemon_token(
            &hash_daemon_token(&raw),
            workspace_id,
            DAEMON_ID,
            TOKEN_TTL_SECS,
        )
        .await
        .context("insert daemon_token")?;
    Ok(RegisteredToken { raw, id })
}

/// 报告里可以安全出现的身份标识（**不是**凭据）。
#[must_use]
pub fn seed_id() -> String {
    format!("mdt/{DAEMON_ID}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_prefixed_unique_and_not_the_seed_id() {
        let a = mint();
        let b = mint();
        assert!(a.starts_with(DAEMON_TOKEN_PREFIX), "{a}");
        assert_eq!(a.len(), DAEMON_TOKEN_PREFIX.len() + SUFFIX_HEX_LEN);
        assert!(a[DAEMON_TOKEN_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "两次签发必须给出不同令牌");
    }

    #[test]
    fn registered_token_debug_redacts_the_credential() {
        // 承重：报告 / 日志一旦 `{:?}` 打出来，令牌就进了 CI 输出。
        let t = RegisteredToken {
            raw: mint(),
            id: Id(Uuid::from_u128(7)),
        };
        let shown = format!("{t:?}");
        assert!(!shown.contains(&t.raw), "明文出现在 Debug 输出里：{shown}");
        assert!(shown.contains(DAEMON_TOKEN_PREFIX), "{shown}");
        // 报告侧能引用的只有 id 级信息。
        assert_eq!(seed_id(), "mdt/conformance-daemon");
        assert!(!seed_id().contains(&mint()[DAEMON_TOKEN_PREFIX.len()..]));
    }
}
