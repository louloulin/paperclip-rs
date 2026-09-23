//! 插件令牌：安装令牌（安装方调用宿主）与回调令牌（宿主带着回调进插件）的签发/校验。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`internal/service/plugin_token.go` —— `installTokenPrefix = "mpi_"`(36) /
//!   `callbackTokenPrefix = "mpc_"`(37) / `IssueInstallToken`(47，`base64.RawURLEncoding`) /
//!   `InstallCredentials`(65) / `RotateInstallCredentials`(74) / `RevokeInstallToken`(92) /
//!   `AuthenticateInstallToken`(103) / `hashToken`(121) / `CallbackGrant`(131) /
//!   `CallbackTokens`(166)。
//!
//! ## 两条**不能混**的令牌族
//!
//! | 族 | 前缀 | 落库 | 生命周期 |
//! | --- | --- | --- | --- |
//! | 安装令牌 | `mpi_` | 只有**哈希**进 `plugin_installation.token_hash`（+ `token_rotated_at`） | 可轮换 / 可吊销 |
//! | 回调令牌 | `mpc_` | **不落库**（进程内 `CallbackTokens`，带 sweep） | 短命、单次授权用 |
//!
//! 明文令牌只在**签发那一刻**存在；`token_hash` 是唯一的持久形态。回调令牌**绝不**进
//! iframe（`docs/57` §4.2 M6-7 的安全约束）。
//!
//! - **本仓约定**：`mc-core::plugin::PluginTokenKind` 已经固定了两个前缀常量
//!   （`Install` → `mpi_`、`Callback` → `mpc_`），**以它为准**，本文件不要再写一遍字面量；
//!   哈希用 `sha2`（纯 hex，不带前缀）。
//! - **不做什么**：不发 JWT（上游就是随机令牌 + 哈希比对）；不做 token 的跨进程共享
//!   （回调令牌是进程内的，多实例部署下由 M6-7 的 sticky/回源策略处理，本波不扩面）。
//!
//! **状态：M6-1 已落地。**
//!
//! ## 为什么回调令牌**不是**单次使用（上游实测出来的）
//!
//! 单次使用看起来更严，实际更糟：参考 handler「读 issue → 判断 → 发评论」的第二次调用会
//! 撞上已耗尽的令牌。**任何**做事的 handler 至少要两次调用。真正要防的不是「一次还是多
//! 次」，而是「handler 完不成活就改用那枚永不过期、不绑调用的安装令牌」—— 把作者推向更强
//! 凭据的控制比略松的控制更差。故：`Resolve` 在 TTL 内**可重复**成功（本文件的用例
//! `callback_token_survives_a_second_call` 就是这条的回归）；边界靠 TTL、本安装的 scope、
//! 派发时定的 actor、以及调用所针对的 issue。

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mc_core::id::Id;
use mc_core::plugin::{PluginInvocationTrigger, PluginTokenKind};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// 令牌随机部分的字节数（上游 `callbackTokenEntropy = 32`；两族同值）。
pub const TOKEN_ENTROPY_BYTES: usize = 32;

/// 回调令牌的生存期（上游 `callbackTokenTTL = 5 * time.Minute`）。
pub const CALLBACK_TOKEN_TTL: Duration = Duration::from_secs(300);

/// 令牌相关的失败（消息与上游逐字对齐）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// 前缀不对（`mpi_`）。
    #[error("invalid plugin token")]
    InvalidInstallToken,
    /// 前缀不对（`mpc_`）。
    #[error("invalid callback token")]
    InvalidCallbackToken,
    /// 哈希查不到，或已过期 —— 上游故意**不区分**这两种（都给同一个消息）。
    #[error("callback token is expired or unknown")]
    CallbackTokenUnavailable,
    /// 取随机数失败。
    #[error("generate {kind} token")]
    Generate {
        /// `"install"` / `"callback"`。
        kind: &'static str,
    },
}

impl TokenError {
    /// 稳定错误码（route 层映射 JSON 错误体时用）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Generate { .. } => "plugin_token_unavailable",
            Self::InvalidInstallToken | Self::InvalidCallbackToken | Self::CallbackTokenUnavailable => {
                "plugin_token_invalid"
            }
        }
    }
}

/// `sha256(token)` 的纯 hex（**无密钥**、**无前缀**）。
///
/// 安装令牌是「插件产生、宿主只验证」的方向 ⇒ 单向哈希就够，库里存不出可用凭据；
/// 这与 hook 签名密钥（宿主必须**产生**签名）方向相反，见 [`crate::credentials`]。
#[must_use]
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

/// 令牌明文是否属于某一族（`TrimSpace` 后再判）。
#[must_use]
pub fn token_has_prefix(token: &str, kind: PluginTokenKind) -> bool {
    token.trim_start().starts_with(kind.prefix())
}

/// 生成一枚安装令牌（`mpi_` + 32 字节随机物的 base64url **无填充**）。
///
/// # Errors
///
/// [`TokenError::Generate`]：熵源失败（fail-closed，绝不用可预测的兜底）。
pub fn issue_install_token(rng: &mut impl RngCore) -> Result<String, TokenError> {
    random_token(rng, PluginTokenKind::Install, "install")
}

/// 生成一枚回调令牌（`mpc_`，同上）。
///
/// # Errors
///
/// [`TokenError::Generate`]。
pub fn issue_callback_token(rng: &mut impl RngCore) -> Result<String, TokenError> {
    random_token(rng, PluginTokenKind::Callback, "callback")
}

fn random_token(
    rng: &mut impl RngCore,
    kind: PluginTokenKind,
    label: &'static str,
) -> Result<String, TokenError> {
    use base64::Engine as _;
    let mut raw = [0u8; TOKEN_ENTROPY_BYTES];
    rng.try_fill_bytes(&mut raw)
        .map_err(|_| TokenError::Generate { kind: label })?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    Ok(format!("{}{}", kind.prefix(), encoded))
}

/// 一次轮换返回的全部凭据（上游 `InstallCredentials`）。
///
/// `signing_secret` 为空 = 本部署没配 `MULTICA_PLUGIN_SECRET_KEY`（hook 签名关闭），
/// 安装令牌仍可用于 Public API —— 这两个凭据的生命周期互不牵连。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallCredentials {
    /// `mpi_…` 明文，**只在此刻出现一次**。
    pub token: String,
    /// `whsec_…`，见 [`crate::credentials::hook_signing_secret`]。
    pub signing_secret: Option<String>,
}

/// hook 调用的主体类型（上游 `HookActor.Type` 的闭集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    /// `member`：有人在界面按下按钮 —— 写归属到那个人。
    Member,
    /// `plugin`：服务端手发或 cron 计划 —— 没有「人」可借，归属到安装。
    Plugin,
    /// `agent`：agent 工具调用。
    Agent,
}

impl ActorKind {
    /// wire 字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Plugin => "plugin",
            Self::Agent => "agent",
        }
    }

    /// 解析 wire 字面量。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Member, Self::Plugin, Self::Agent]
            .into_iter()
            .find(|kind| kind.as_str() == raw)
    }
}

/// 派发时**就定好**的写归属（上游 `HookActor`）——handler 不能自己选写谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HookActor {
    /// 主体类型。
    pub kind: ActorKind,
    /// 主体 id（`plugin` 时是安装 id）。
    pub id: Id,
}

/// 签发一枚回调令牌所需的输入（上游 `HookInvocation` 的投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackRequest<'a> {
    /// 所属安装。
    pub installation_id: Id,
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 哪个 hook。
    pub hook_key: &'a str,
    /// 本次调用的触发器。
    pub trigger: PluginInvocationTrigger,
    /// 写归属。
    pub actor: HookActor,
    /// 有 issue 上下文时收窄到该 issue；否则 `None`。
    pub issue_id: Option<Id>,
}

/// 一枚回调令牌证明了什么（上游 `CallbackGrant`）。
///
/// scope 是安装的，**绝不更宽**：回调存在的意义是让 handler 把活干完，不是让一个越权
/// 请求能比 surface 做得更多。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackGrant {
    /// 所属安装。
    pub installation_id: Id,
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 哪个 hook。
    pub hook_key: String,
    /// 本次调用的触发器。
    pub trigger: PluginInvocationTrigger,
    /// 写归属（派发时定）。
    pub actor: HookActor,
    /// 收窄到的 issue。
    pub issue_id: Option<Id>,
    /// 过期时刻（unix 秒）。
    pub expires_at: u64,
}

/// 进程内的回调令牌表（上游 `CallbackTokens`）。
///
/// 用 `Mutex<BTreeMap<hash, grant>>`：键是**哈希**（与安装令牌同一形状，进程内存转储也
/// 拿不到明文）；临界区只有几次 map 操作，不需要异步锁。
///
/// 跨实例 / 重启后令牌**提前**失效而不是延后 —— 失败模式是 handler 看到 403（可见、可重
/// 试），而不是授权活过窗口。
#[derive(Debug, Default)]
pub struct CallbackTokens {
    issued: Mutex<BTreeMap<String, CallbackGrant>>,
}

impl CallbackTokens {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前表内条数（含未清扫的过期项；清扫在 `issue`/`resolve` 时发生）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 表是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// 为一个 hook 调用签发令牌（`now` = 当前 unix 秒）。
    ///
    /// # Errors
    ///
    /// [`TokenError::Generate`]。
    pub fn issue(&self, request: &CallbackRequest<'_>) -> Result<String, TokenError> {
        self.issue_at(request, unix_now())
    }

    /// 与 [`Self::issue`] 相同，但时刻由调用方给（用例用）。
    ///
    /// # Errors
    ///
    /// [`TokenError::Generate`]。
    pub fn issue_at(
        &self,
        request: &CallbackRequest<'_>,
        now_unix: u64,
    ) -> Result<String, TokenError> {
        let token = issue_callback_token(&mut rand::rngs::OsRng)?;
        let grant = CallbackGrant {
            installation_id: request.installation_id,
            workspace_id: request.workspace_id,
            hook_key: request.hook_key.to_owned(),
            trigger: request.trigger,
            actor: request.actor,
            issue_id: request.issue_id,
            expires_at: now_unix + CALLBACK_TOKEN_TTL.as_secs(),
        };
        let mut issued = self.lock();
        sweep(&mut issued, now_unix);
        issued.insert(hash_token(&token), grant);
        Ok(token)
    }

    /// 解析一枚令牌；TTL 内**可重复**成功（见文件头注）。
    ///
    /// # Errors
    ///
    /// [`TokenError::InvalidCallbackToken`]（前缀不对）/
    /// [`TokenError::CallbackTokenUnavailable`]（查不到或已过期）。
    pub fn resolve(&self, token: &str) -> Result<CallbackGrant, TokenError> {
        self.resolve_at(token, unix_now())
    }

    /// 与 [`Self::resolve`] 相同，但时刻由调用方给（用例用）。
    ///
    /// # Errors
    ///
    /// 同 [`Self::resolve`]。
    pub fn resolve_at(&self, token: &str, now_unix: u64) -> Result<CallbackGrant, TokenError> {
        let token = token.trim();
        if !token.starts_with(PluginTokenKind::Callback.prefix()) {
            return Err(TokenError::InvalidCallbackToken);
        }
        let key = hash_token(token);
        let mut issued = self.lock();
        sweep(&mut issued, now_unix);
        issued
            .get(&key)
            .cloned()
            .ok_or(TokenError::CallbackTokenUnavailable)
    }

    /// 在过期前主动丢掉一枚令牌（调用结束时调用，免得它继续挂到窗口结束）。
    pub fn revoke(&self, token: &str) {
        if token.trim().is_empty() {
            return;
        }
        self.lock().remove(&hash_token(token.trim()));
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, CallbackGrant>> {
        self.issued.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 丢掉已过期的条目（长跑进程的表不会无界增长）。调用方持锁。
fn sweep(issued: &mut BTreeMap<String, CallbackGrant>, now_unix: u64) {
    issued.retain(|_, grant| grant.expires_at > now_unix);
}

/// 当前 unix 秒（时钟早于 epoch 时退化为 0，不 panic）。
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 可预测的熵源：把每个字节都填成同一个值。
    struct FixedRng(u8);

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            u32::from(self.0) * 0x0101_0101
        }
        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            dest.fill(self.0);
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    fn request(actor_kind: ActorKind, hook_key: &str) -> CallbackRequest<'_> {
        CallbackRequest {
            installation_id: Id::new(),
            workspace_id: Id::new(),
            hook_key,
            trigger: PluginInvocationTrigger::Ui,
            actor: HookActor {
                kind: actor_kind,
                id: Id::new(),
            },
            issue_id: None,
        }
    }

    #[test]
    fn prefix_vocabulary_comes_from_mc_core() {
        // 本文件不写字面量：前缀的权威是 `mc-core::plugin::PluginTokenKind`。
        assert_eq!(PluginTokenKind::Install.prefix(), "mpi_");
        assert_eq!(PluginTokenKind::Callback.prefix(), "mpc_");
        let install = issue_install_token(&mut FixedRng(0)).expect("entropy");
        let callback = issue_callback_token(&mut FixedRng(0)).expect("entropy");
        assert!(token_has_prefix(&install, PluginTokenKind::Install));
        assert!(token_has_prefix(&callback, PluginTokenKind::Callback));
        assert!(!token_has_prefix(&callback, PluginTokenKind::Install));
    }

    #[test]
    fn token_shape_is_prefix_plus_base64url_of_32_bytes() {
        // 32 个 0x00 ⇒ 43 个 'A'（RawURLEncoding 不带填充）；32 个 0xff ⇒ 43 个 '_'。
        assert_eq!(
            issue_install_token(&mut FixedRng(0x00)).unwrap(),
            format!("mpi_{}", "A".repeat(43))
        );
        assert_eq!(
            issue_install_token(&mut FixedRng(0xff)).unwrap(),
            format!("mpi_{}8", "_".repeat(42))
        );
        assert_eq!(
            issue_callback_token(&mut FixedRng(0x00)).unwrap(),
            format!("mpc_{}", "A".repeat(43))
        );
    }

    #[test]
    fn hash_is_plain_sha256_hex() {
        // 交叉实现向量：`sha256("abc")`。
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_token("").len(), 64);
        // 与 `plugin_installation.token_hash` 的列口径一致：纯 hex，不带 `sha256:` 前缀。
        assert!(hash_token("mpc_x").chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn callback_token_survives_a_second_call() {
        let tokens = CallbackTokens::new();
        let token = tokens
            .issue_at(&request(ActorKind::Member, "on_issue"), 1_700_000_000)
            .expect("issue");
        let first = tokens.resolve_at(&token, 1_700_000_060).expect("first call");
        let second = tokens
            .resolve_at(&token, 1_700_000_120)
            .expect("second call must not be rejected");
        assert_eq!(first, second);
        assert_eq!(first.hook_key, "on_issue");
        assert_eq!(first.trigger, PluginInvocationTrigger::Ui);
        assert_eq!(first.actor.kind, ActorKind::Member);
        assert_eq!(first.expires_at, 1_700_000_000 + 300);
        // 明文不进表：键是哈希。
        assert_eq!(tokens.len(), 1);
        assert!(!format!("{:?}", tokens.lock().keys().collect::<Vec<_>>()).contains("mpc_"));
    }

    #[test]
    fn callback_token_accepts_surrounding_whitespace_like_go() {
        let tokens = CallbackTokens::new();
        let token = tokens
            .issue_at(&request(ActorKind::Plugin, "on_tick"), 1_000)
            .expect("issue");
        assert!(tokens.resolve_at(&format!("  {token}\n"), 1_000).is_ok());
    }

    #[test]
    fn callback_token_rejects_wrong_prefix_and_unknown_and_expired() {
        let tokens = CallbackTokens::new();
        assert_eq!(
            tokens.resolve_at("mpi_whatever", 1_000),
            Err(TokenError::InvalidCallbackToken)
        );
        assert_eq!(
            tokens.resolve_at("mpc_wellformedbutunknown", 1_000),
            Err(TokenError::CallbackTokenUnavailable)
        );
        let token = tokens
            .issue_at(&request(ActorKind::Agent, "on_call"), 1_000)
            .expect("issue");
        // TTL 边界：到点即失效（`expires_at > now`）。
        assert!(tokens.resolve_at(&token, 1_300).is_err());
        assert!(tokens.is_empty(), "清扫发生在解析时");
    }

    #[test]
    fn revoke_drops_the_grant_before_its_window_ends() {
        let tokens = CallbackTokens::new();
        let token = tokens
            .issue_at(&request(ActorKind::Member, "on_issue"), 5_000)
            .expect("issue");
        assert!(tokens.resolve_at(&token, 5_001).is_ok());
        tokens.revoke(&token);
        assert_eq!(
            tokens.resolve_at(&token, 5_002),
            Err(TokenError::CallbackTokenUnavailable)
        );
        // 空串是 no-op（上游 `if token == "" { return }`）。
        tokens.revoke("   ");
    }

    #[test]
    fn error_codes_and_messages_match_upstream() {
        assert_eq!(TokenError::InvalidInstallToken.to_string(), "invalid plugin token");
        assert_eq!(TokenError::InvalidCallbackToken.to_string(), "invalid callback token");
        assert_eq!(
            TokenError::CallbackTokenUnavailable.to_string(),
            "callback token is expired or unknown"
        );
        assert_eq!(
            TokenError::Generate { kind: "install" }.to_string(),
            "generate install token"
        );
        assert_eq!(TokenError::InvalidInstallToken.code(), "plugin_token_invalid");
        assert_eq!(
            TokenError::Generate { kind: "callback" }.code(),
            "plugin_token_unavailable"
        );
    }

    #[test]
    fn actor_kinds_are_the_upstream_closed_set() {
        assert_eq!(
            [ActorKind::Member, ActorKind::Plugin, ActorKind::Agent].map(ActorKind::as_str),
            ["member", "plugin", "agent"]
        );
        assert_eq!(ActorKind::parse("plugin"), Some(ActorKind::Plugin));
        assert_eq!(ActorKind::parse("system"), None);
    }
}
