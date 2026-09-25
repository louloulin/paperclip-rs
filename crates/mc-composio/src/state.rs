//! callback state 的 HMAC 签发与校验 —— 上游 `integrations/composio/state.go`（92 行）。
//!
//! **M8-0 anchor 建桩（`LUM-1797`）、M8-6 落地（`LUM-1803`）**：本文件的形状与契约面由
//! anchor 钉死，实现（签发 / 校验 / 四个反例）归本片。
//!
//! # 为什么 state 必须签（`docs/61` §1.5 第 4 行）
//!
//! `GET /api/integrations/composio/callback` 是**公开块**路由（不挂会话 middleware）——
//! 它**明确从会话之外取身份**（`state` 决定 `user_id` / workspace）。secret 取
//! `COMPOSIO_STATE_SECRET`，缺则退回 `JWT_SECRET` 派生。
//!
//! # 四个反例（`docs/61` §6.5 的 M8-6 专属 `DoD`）
//!
//! 篡改 / 过期 / 重放 / 错密钥 —— 全部必须被拒。重放靠 nonce（进程内台账，无 Redis ⇒
//! **单副本部署契约**，与 R-M7-1 同源，登记 `docs/32` §9.12）。
//!
//! # 本地 wire 形态（`<payload>.<sig>`，与上游同形）
//!
//! ```text
//! payload = base64url_nopad(json({"u":…,"t":…,"a":…,"e":…,"n":…}))
//! sig     = base64url_nopad(HMAC-SHA256(secret, payload))
//! token   = payload + "." + sig
//! ```
//!
//! - 字段名是**单字母**（上游 `stateClaims` 逐字：`u`=`user_id` / `t`=`toolkit_slug` /
//!   `a`=`auth_config_id` / `e`=`exp`），本片**多一个 `n`=`nonce`**（上游靠短 `exp` 限重放，
//!   本片按 anchor 的契约另加进程内台账）；
//! - `auth_config_id` 在 `BeginConnect` 解析时就被签进 state（上游注释逐字：signing it into
//!   the state lets `CompleteCallback` verify the returned account was created under **THIS**
//!   toolkit's auth config without re-resolving (which could fail-open)）⇒ 校验侧**不回查**目录；
//! - 签的是**编码后的 payload 串**（不是原始结构），校验侧因此重算出逐字节同一段字节
//!   （上游 `signState` 的注释逐字：so verification re-derives the exact bytes that were signed）。
//!
//! # 校验顺序（先证伪、后信任，最后才消费 nonce）
//!
//! 形态（`Malformed`）→ 签名（`Tampered`，常量时间）→ 过期（`Expired`）→ 重放（`Replayed`）。
//! 顺序是契约的一部分：先比较签名再解 payload，**不解析未经验证的输入**；过期在重放之前，
//! 所以「过期 + 已用过」报到的是 `Expired`（交接给 M8-7 的登记项）。
//!
//! # 单次使用（nonce 台账）
//!
//! 校验**成功即消费** nonce：同一个 state 第二次到达（无论中间那一次的业务是成功还是失败）
//! 一律 `Replayed`。台账按 `exp` 修剪 ⇒ 有界增长。
//!
//! ⚠️ **单副本部署契约**：台账在进程内存里（本仓无 Redis），多副本部署时同一 state 可能在
//! 另一个副本上被判为首次 ⇒ 部署必须是单副本（与 M7 的 R-M7-1 同源，登记 `docs/32` §9.12）。

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// state 的默认寿命：上游 `defaultStateTTL` 逐字（5 分钟）。
///
/// 上游注释逐字：「Five minutes is generous for a hosted OAuth flow while keeping the replay
/// window small.」
pub const DEFAULT_STATE_TTL_SECS: i64 = 300;

/// state 校验失败的原因（**逐条可测**的四类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    #[error("composio state: malformed")]
    Malformed,
    #[error("composio state: signature mismatch")]
    Tampered,
    #[error("composio state: expired")]
    Expired,
    #[error("composio state: replayed")]
    Replayed,
}

/// 签进 state 的载荷（`user_id` / `toolkit_slug` / `auth_config_id` / `exp` / `nonce`）。
///
/// 手写 `Debug` **不必**：这五个字段里没有密钥（`auth_config_id` 是不透明的配置句柄 `ac_…`，
/// 不是凭据）。它们**可以**出现在日志里，但**不**进任务评论与响应体。
///
/// `user_id` / `toolkit_slug` / `auth_config_id` 在 `toolkit_slug` 上是**规范化之后**的值
/// （小写 + trim，`Service::begin_connect` 负责），因为它是回写 `user_composio_connection`
/// 的那一列的值。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateClaims {
    /// Multica 的用户 id（`util.UUIDToString(userID)` 逐字）。
    ///
    /// 上游的不变量逐字：`composio_user_id` 恒等于 Multica 用户 id 的字符串形态。
    #[serde(rename = "u")]
    pub user_id: String,
    /// 规范化后的 toolkit slug（小写 + trim）。
    #[serde(rename = "t")]
    pub toolkit_slug: String,
    /// `BeginConnect` 当时解析出的 `auth_config_id`（`ac_…`，**不透明句柄**，非凭据）。
    #[serde(rename = "a")]
    pub auth_config_id: String,
    /// 过期时刻（Unix 秒）。上游 `stateClaims.Exp`。
    #[serde(rename = "e")]
    pub exp: i64,
    /// 本片新增的 nonce（重放台账的索引键）。
    #[serde(rename = "n")]
    pub nonce: String,
}

impl StateClaims {
    /// 造一份载荷：`exp = now_unix + ttl_secs`，`nonce` 由 [`fresh_nonce`] 现取。
    pub fn new(
        user_id: impl Into<String>,
        toolkit_slug: impl Into<String>,
        auth_config_id: impl Into<String>,
        now_unix: i64,
        ttl_secs: i64,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            toolkit_slug: toolkit_slug.into(),
            auth_config_id: auth_config_id.into(),
            exp: now_unix.saturating_add(ttl_secs),
            nonce: fresh_nonce(),
        }
    }
}

/// 已消费的 nonce 台账（**进程级**，见 [`ledger`]）。
static CONSUMED_NONCES: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

/// 进程级的重放台账。
///
/// ⚠️ 为什么**不**把台账放进 [`StateSigner`] 的字段：签发器是**按请求**构造的
/// （`ComposioService::new` 每次请求一次，见 `mc-http` 的装配助手）⇒ 台账跟着实例就
/// **形同虚设**（同一份 state 的第二次到达会落到一个新实例上，永远判不出重放）。
/// 台账按 nonce 索引（nonce 由 [`fresh_nonce`] 保证唯一）⇒ 跨实例、跨 secret 都不串。
fn ledger() -> &'static Mutex<HashMap<String, i64>> {
    CONSUMED_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 把 nonce 记进台账；已在台账里 ⇒ [`StateError::Replayed`]。
///
/// 顺手按 `exp` 修剪过期项 ⇒ 台账大小与「最近 `ttl` 秒内签发过的 state 数」同阶。
fn consume(nonce: &str, exp: i64, now_unix: i64) -> Result<(), StateError> {
    let mut ledger: MutexGuard<HashMap<String, i64>> =
        ledger().lock().unwrap_or_else(PoisonError::into_inner);
    ledger.retain(|_, seen_exp| *seen_exp >= now_unix);
    if ledger.insert(nonce.to_string(), exp).is_some() {
        return Err(StateError::Replayed);
    }
    Ok(())
}

/// state 的 HMAC 签发器（**不派生 `Debug`**：secret 绝不进日志）。
pub struct StateSigner {
    /// state secret（`COMPOSIO_STATE_SECRET` 或由 `JWT_SECRET` 派生）。
    secret: String,
    /// 本签发器签出的 state 的寿命（秒）。
    ttl_secs: i64,
}

impl StateSigner {
    /// 从 secret 构造（空 secret 也允许构造 —— 但签发/校验必然失败，见 [`StateSigner::sign`]）。
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
            ttl_secs: DEFAULT_STATE_TTL_SECS,
        }
    }

    /// 覆盖 state 寿命（测试注入；生产走 [`DEFAULT_STATE_TTL_SECS`]）。
    #[must_use]
    pub fn with_ttl(mut self, ttl_secs: i64) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    /// 本签发器当前使用的寿命（秒）。
    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    /// 签发一个绑定载荷的 state —— 上游 `signState`。
    ///
    /// # Errors
    ///
    /// - secret 为空 ⇒ [`StateError::Malformed`]（上游 `signPayload` 在空 secret 上会签出一个
    ///   可被任何人伪造的 MAC ⇒ 本片直接判失败，**不**签）；
    /// - 载荷 JSON 编码失败 ⇒ [`StateError::Malformed`]（本结构全是字符串/整数，实际不可达）。
    pub fn sign(&self, claims: &StateClaims) -> Result<String, StateError> {
        if self.secret.is_empty() {
            return Err(StateError::Malformed);
        }
        let raw = serde_json::to_vec(claims).map_err(|_| StateError::Malformed)?;
        let payload = URL_SAFE_NO_PAD.encode(raw);
        let signature = mac_base64(&self.secret, &payload)?;
        Ok(format!("{payload}.{signature}"))
    }

    /// 校验 state 并解出载荷 —— 上游 `verifyState`（多一条重放判定）。
    ///
    /// # Errors
    ///
    /// 篡改 / 过期 / 重放 / 格式非法四类，逐条对应 [`StateError`]；错密钥与改签名同判
    /// [`StateError::Tampered`]（上游注释逐字：the state was tampered with **or signed by a
    /// different secret**）。
    pub fn verify(&self, state: &str, now_unix: i64) -> Result<StateClaims, StateError> {
        let Some((payload, signature)) = state.split_once('.') else {
            return Err(StateError::Malformed);
        };
        if payload.is_empty() || signature.is_empty() {
            return Err(StateError::Malformed);
        }
        // ① 先验签名（常量时间），**不**解析未经验证的 payload。
        let mut verifier = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .map_err(|_| StateError::Malformed)?;
        verifier.update(payload.as_bytes());
        let decoded = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| StateError::Malformed)?;
        verifier
            .verify_slice(&decoded)
            .map_err(|_| StateError::Tampered)?;

        // ② 再解载荷。
        let raw = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| StateError::Malformed)?;
        let claims: StateClaims =
            serde_json::from_slice(&raw).map_err(|_| StateError::Malformed)?;
        if claims.user_id.is_empty() || claims.nonce.is_empty() {
            return Err(StateError::Malformed);
        }

        // ③ 过期（上游 `if now.Unix() > claims.Exp`，逐字 `>` —— 恰好等于 exp 仍是有效）。
        if now_unix > claims.exp {
            return Err(StateError::Expired);
        }

        // ④ 重放（**成功即消费**：同一次握手的第二次到达一律拒绝）。
        consume(&claims.nonce, claims.exp, now_unix)?;
        Ok(claims)
    }
}

impl std::fmt::Debug for StateSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateSigner")
            .field("secret", &"<redacted>")
            .field("ttl_secs", &self.ttl_secs)
            .field("consumed_nonces", &consumed_count())
            .finish()
    }
}

/// 台账里的条目数（诊断用：不暴露 nonce 本身）。
fn consumed_count() -> usize {
    ledger().lock().map_or(0, |ledger| ledger.len())
}

/// `base64url_nopad(HMAC-SHA256(secret, message))`。
///
/// # Errors
///
/// 密钥长度非法 ⇒ [`StateError::Malformed`]（`HMAC` 对任意长度密钥都合法，实际不可达）。
fn mac_base64(secret: &str, message: &str) -> Result<String, StateError> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| StateError::Malformed)?;
    mac.update(message.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// 一个进程内唯一、跨进程不可预测的 nonce（32 个 hex 字符 = 16 字节）。
///
/// 为什么不引 `rand` / `uuid`：`mc-composio` 的 manifest 在 M8-0 anchor 之后**冻结**
/// （`docs/61` §3.1：M8 各代码片都不得改 manifest / `Cargo.lock`），而 `std` 里的
/// [`std::collections::hash_map::RandomState`] 每实例取内核熵 ⇒ 与「进程内单调计数」
/// 拼起来就有「唯一 + 不可预测」两条性质。
///
/// 不可预测**不是**本 nonce 的承重性质（伪造 state 已经由 HMAC 挡掉，重放由台账挡掉）；
/// 唯一性才是 —— 台账按它索引。`seq` 那一半保证同一时刻的两次签发不撞。
fn fresh_nonce() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(nanos);
    hasher.write_u64(sequence);
    format!("{:016x}{:016x}", hasher.finish(), sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "composio-state-secret";
    const NOW: i64 = 1_800_000_000;

    fn signer() -> StateSigner {
        StateSigner::new(SECRET)
    }

    /// 每个用例要在**同一份 state** 上做多次判定 ⇒ 台账是进程级的，用例之间不能互相干扰
    /// （nonce 唯一 ⇒ 天然不串），而**同一用例内**必须复用同一个 token。
    fn fresh_token(ttl_secs: i64) -> (StateSigner, StateClaims, String) {
        let signer = StateSigner::new(SECRET);
        let claims = StateClaims::new(
            "0b2f5a52-0b1a-4f4e-9d2f-7a1c6f0d1e2a",
            "notion",
            "ac_notion_managed",
            NOW,
            ttl_secs,
        );
        let token = signer.sign(&claims).expect("sign");
        (signer, claims, token)
    }

    fn claims() -> StateClaims {
        StateClaims::new(
            "0b2f5a52-0b1a-4f4e-9d2f-7a1c6f0d1e2a",
            "notion",
            "ac_notion_managed",
            NOW,
            DEFAULT_STATE_TTL_SECS,
        )
    }

    #[test]
    fn round_trips_and_binds_every_claim() {
        let claims = claims();
        let token = signer().sign(&claims).expect("sign");
        assert_eq!(token.split('.').count(), 2, "两段：payload + sig");
        let verified = signer().verify(&token, NOW).expect("verify");
        assert_eq!(verified, claims);
        assert_eq!(verified.exp, NOW + DEFAULT_STATE_TTL_SECS);
        assert!(!verified.nonce.is_empty());
    }

    #[test]
    fn payload_is_base64url_json_with_the_single_letter_fields() {
        let claims = claims();
        let token = signer().sign(&claims).expect("sign");
        let (payload, signature) = token.split_once('.').expect("两段");
        let raw = URL_SAFE_NO_PAD.decode(payload).expect("payload base64url");
        let value: serde_json::Value = serde_json::from_slice(&raw).expect("payload json");
        assert_eq!(value["u"], serde_json::json!(claims.user_id));
        assert_eq!(value["t"], serde_json::json!("notion"));
        assert_eq!(value["a"], serde_json::json!("ac_notion_managed"));
        assert_eq!(value["e"], serde_json::json!(claims.exp));
        assert_eq!(value["n"], serde_json::json!(claims.nonce));
        assert!(!payload.contains('='), "nopad");
        assert!(!payload.contains('+') && !payload.contains('/'), "url-safe");
        assert_eq!(URL_SAFE_NO_PAD.decode(signature).expect("sig").len(), 32);
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let token = signer().sign(&claims()).expect("sign");
        let (payload, signature) = token.split_once('.').expect("两段");
        let raw = URL_SAFE_NO_PAD.decode(payload).expect("payload");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).expect("json");

        // 反例一：改 user_id（把账号绑到别人身上）。
        let mut moved = value.clone();
        moved["u"] = serde_json::json!("ffffffff-ffff-ffff-ffff-ffffffffffff");
        let moved = format!(
            "{}.{signature}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&moved).expect("json"))
        );
        assert_eq!(signer().verify(&moved, NOW), Err(StateError::Tampered));

        // 反例二：改 toolkit（把另一家的授权绑到本 toolkit 上）。
        value["t"] = serde_json::json!("github");
        let retooled = format!(
            "{}.{signature}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("json"))
        );
        assert_eq!(signer().verify(&retooled, NOW), Err(StateError::Tampered));
    }

    #[test]
    fn tampered_signature_is_rejected_bit_for_bit() {
        let token = signer().sign(&claims()).expect("sign");
        let (payload, signature) = token.split_once('.').expect("两段");
        let mut flipped = signature.to_string();
        let last = flipped.pop().expect("non-empty");
        flipped.push(if last == 'A' { 'B' } else { 'A' });
        assert_eq!(
            signer().verify(&format!("{payload}.{flipped}"), NOW),
            Err(StateError::Tampered)
        );
        // 签名不是 base64url ⇒ 形态非法（**不是** Tampered：还没进入比较）。
        assert_eq!(
            signer().verify(&format!("{payload}.***"), NOW),
            Err(StateError::Malformed)
        );
    }

    #[test]
    fn expired_state_is_rejected_and_the_boundary_is_inclusive() {
        // 恰好等于 exp ⇒ 仍有效（上游 `now > exp` 逐字）。
        let (signer, claims, token) = fresh_token(DEFAULT_STATE_TTL_SECS);
        assert_eq!(signer.verify(&token, claims.exp), Ok(claims.clone()));

        // 边界之外 ⇒ Expired。
        let (signer, claims, token) = fresh_token(DEFAULT_STATE_TTL_SECS);
        assert_eq!(
            signer.verify(&token, claims.exp + 1),
            Err(StateError::Expired)
        );
        // 过期优先于重放：同一份 state 再过 1 秒重放 ⇒ 仍是 Expired（nonce 从未被消费）。
        assert_eq!(
            signer.verify(&token, claims.exp + 2),
            Err(StateError::Expired)
        );
    }

    #[test]
    fn replayed_state_is_rejected_once() {
        let (signer, _, token) = fresh_token(DEFAULT_STATE_TTL_SECS);
        assert!(signer.verify(&token, NOW).is_ok(), "第一次到达");
        assert_eq!(
            signer.verify(&token, NOW),
            Err(StateError::Replayed),
            "第二次到达 ⇒ 拒绝（无论第一次的业务成败）"
        );
        assert_eq!(signer.verify(&token, NOW + 1), Err(StateError::Replayed));
    }

    #[test]
    fn the_replay_ledger_is_process_wide_not_per_signer() {
        // 反例：签发器按请求构造 ⇒ 台账若跟着实例，重放就完全挡不住。
        let (first, _, token) = fresh_token(DEFAULT_STATE_TTL_SECS);
        assert!(first.verify(&token, NOW).is_ok());
        let second = StateSigner::new(SECRET);
        assert_eq!(second.verify(&token, NOW), Err(StateError::Replayed));
    }

    #[test]
    fn each_signature_carries_a_distinct_nonce() {
        let signer = signer();
        let a = signer.sign(&claims()).expect("sign");
        let b = signer.sign(&claims()).expect("sign");
        assert_ne!(a, b, "同一载荷两次签发必须是两个 state（nonce 不同）");
        assert!(signer.verify(&a, NOW).is_ok());
        assert!(signer.verify(&b, NOW).is_ok());
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let (other, claims, other_token) = {
            let other = StateSigner::new("another-secret");
            let claims = StateClaims::new("u", "notion", "ac_1", NOW, DEFAULT_STATE_TTL_SECS);
            let token = other.sign(&claims).expect("sign");
            (other, claims, token)
        };
        assert_eq!(other.verify(&other_token, NOW), Ok(claims));
        // 别的 secret 签的 token 在**这个**签发器上连签名都过不去。
        assert_eq!(
            StateSigner::new(SECRET).verify(&other_token, NOW),
            Err(StateError::Tampered)
        );
    }

    #[test]
    fn malformed_tokens_are_rejected_before_any_comparison() {
        for bad in [
            "",
            "no-dot",
            ".",
            ".sig",
            "payload.",
            "not base64!!!.AAAA",
            // 合法 base64url、但不是 JSON 对象。
            "aGVsbG8.AAAA",
        ] {
            assert!(
                signer().verify(bad, NOW).is_err(),
                "非法 state 必须被拒：{bad:?}"
            );
        }
        // payload 是合法 JSON，但缺字段 ⇒ 形态非法。
        let stripped = URL_SAFE_NO_PAD.encode(br#"{"u":"x"}"#);
        let signature = mac_base64(SECRET, &stripped).expect("mac");
        assert_eq!(
            signer().verify(&format!("{stripped}.{signature}"), NOW),
            Err(StateError::Malformed)
        );
    }

    #[test]
    fn empty_secret_signs_nothing() {
        let none = StateSigner::new("");
        assert_eq!(none.sign(&claims()), Err(StateError::Malformed));
        let (signer, _, token) = fresh_token(DEFAULT_STATE_TTL_SECS);
        assert!(signer.verify(&token, NOW).is_ok());
        assert_eq!(
            none.verify(&token, NOW),
            Err(StateError::Tampered),
            "空 secret 校验别人的 state ⇒ 签名不匹配"
        );
    }

    #[test]
    fn ttl_defaults_to_five_minutes_and_is_overridable() {
        assert_eq!(signer().ttl_secs(), 300);
        let short = StateSigner::new(SECRET).with_ttl(1);
        assert_eq!(short.ttl_secs(), 1);
        let claims = StateClaims::new("u", "notion", "ac_1", NOW, short.ttl_secs());
        let token = short.sign(&claims).expect("sign");
        assert_eq!(claims.exp, NOW + 1);
        assert!(short.verify(&token, NOW + 1).is_ok());
        let expired = StateClaims::new("u", "notion", "ac_1", NOW, short.ttl_secs());
        let token = short.sign(&expired).expect("sign");
        assert_eq!(short.verify(&token, NOW + 2), Err(StateError::Expired));
    }

    #[test]
    fn debug_never_echoes_the_secret() {
        let rendered = format!("{:?}", StateSigner::new(SECRET));
        assert!(!rendered.contains(SECRET));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("consumed_nonces"));
    }

    #[test]
    fn nonces_are_unique_and_hex_shaped() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let nonce = fresh_nonce();
            assert_eq!(nonce.len(), 32);
            assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(seen.insert(nonce), "nonce 必须唯一");
        }
    }
}
