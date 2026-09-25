//! `DingTalk` 的用户绑定令牌流程：**铸**一枚单次令牌 → 用户在 web app 里**兑换** →
//! `DingTalk` staff id 绑到 Multica 账号（上游 `internal/integrations/dingtalk/binding.go`，178 行）。
//!
//! - **写者**：M7-9（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §23 的 D1）。
//! - **上游定位**（注释逐字）：一个**未绑定**的 `DingTalk` 用户给机器人发消息 ⇒ 出站回复器铸一枚
//!   令牌并回"点这里绑定"；用户点进 `AppURL + /dingtalk/bind?token=…`；
//!   `POST /api/dingtalk/binding/redeem` 把这个 `DingTalk` staff id 绑到**会话身份**
//!   （不是令牌里的那个）。
//! - **走泛化表**：`channel_binding_token` / `channel_user_binding`，`channel_type='dingtalk'`
//!   （上游 `binding.go` 逐字：`mirrors slack.BindingTokenService but runs on the generic
//!   channel_* queries with channel_type='dingtalk'`）。
//!
//! # 四条安全判据（逐条可测）
//!
//! 1. **明文不进库**：只存 `sha256(raw)` 的 hex（上游 `hashBindingToken`）；
//! 2. **同一个不透明错误覆盖三种失败**（不存在 / 已消费 / 已过期）—— 上游逐字：
//!    `One opaque error for all three avoids a replay timing oracle`；
//! 3. **兑换是原子的**：消费令牌、校验成员资格、插绑定行**同一事务**提交 ⇒ 非成员的一次
//!    尝试**不烧掉**令牌（上游逐字：`returning before Commit rolls the consume back`）；
//! 4. **令牌表跨 adapter 共享** ⇒ 别的 adapter 的令牌**不能**从 `DingTalk` 兑换。上游的做法是
//!    先 consume 再 `validateBindingTokenChannel`，失败时靠外层事务回滚；本仓把这条写进
//!    [`BindingStore::redeem_and_bind`] 的 `WHERE` 里（**等价**：认不出 ⇒ 令牌**没被消费**）。
//!
//! # 寿命上限
//!
//! [`BINDING_TOKEN_TTL`] 是 15 分钟，数据库的 `CHECK` 钉着同一条上限（上游逐字：
//! `so a misconfigured caller cannot mint longer`）⇒ 配错也铸不出更长的令牌。
//!
//! # 随机性（登记 `docs/32` §23 的 D6）
//!
//! 上游是 `crypto/rand` 读 32 字节再 base64url。本 crate 的依赖集里**没有** `rand`
//! （`docs/60` §3.1 冻结），故取两个 v4 UUID（各 122 bit 熵，合计 244 bit）拼成 32 字节 ——
//! 熵量级与上游同级（15 分钟单次令牌），**线的长度也相同**，只是这 32 字节不是均匀分布。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use mc_core::id::Id;
use sha2::{Digest, Sha256};

/// 令牌寿命上限（上游 `BindingTokenTTL = 15 * time.Minute`）。
pub const BINDING_TOKEN_TTL: Duration = Duration::minutes(15);

/// 存储口径的渠道名（令牌表是跨 adapter 共享的 ⇒ 每一处查询都要带上它）。
pub const CHANNEL_TYPE: &str = "dingtalk";

// =====================================================================
// 错误（上游的三个哨兵）
// =====================================================================

/// 绑定面的失败（上游 `ErrBindingTokenInvalid` / `ErrBindingAlreadyAssigned` /
/// `ErrBindingNotWorkspaceMember`）。
///
/// `redeem` 的产品判决由 [`RedeemOutcome`] 表达；本枚举给铸令牌与存储故障用。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// 令牌不存在 / 已消费 / 已过期 / **属于别的 adapter**（同一个不透明错误，判据 2 与 4）。
    #[error("dingtalk: binding token invalid or expired")]
    TokenInvalid,
    /// 这个 `DingTalk` 用户 id 已绑到**另一个** Multica 用户（转移必须显式解绑）。
    #[error("dingtalk: user id is already bound to a different user")]
    AlreadyAssigned,
    /// 兑换者不是令牌所属 workspace 的成员（HTTP 边界翻成 403）。
    #[error("dingtalk: redeemer is not a workspace member")]
    NotMember,
    /// 存储层故障（不透明：不回显 SQL / 参数）。
    #[error("dingtalk: binding store failure: {message}")]
    Store { message: String },
}

impl BindingError {
    /// 稳定错误码（诊断 / 日志用；**不回显**任何凭据或明文）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TokenInvalid => "dingtalk_binding_token_invalid",
            Self::AlreadyAssigned => "dingtalk_binding_already_assigned",
            Self::NotMember => "dingtalk_binding_not_member",
            Self::Store { .. } => "dingtalk_binding_store_error",
        }
    }

    /// HTTP 状态码（上游 `handler/dingtalk.go` 的 switch：410 / 409 / 403）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::TokenInvalid => 410,
            Self::AlreadyAssigned => 409,
            Self::NotMember => 403,
            Self::Store { .. } => 500,
        }
    }
}

// =====================================================================
// 值对象
// =====================================================================

/// 一枚刚铸出的令牌（上游 `BindingToken`）：**明文只在返回值里出现一次**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingToken {
    /// 明文（要经 `DingTalk` 送到用户手上；**不落库**）。
    pub raw: String,
    pub expires_at: DateTime<Utc>,
}

/// 兑换成功的结果（上游 `RedeemedBindingToken`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedBinding {
    pub workspace_id: Id,
    pub installation_id: Id,
    /// 令牌里的 `DingTalk` staff id（**不是**兑换者的 —— 那来自会话）。
    pub channel_user_id: String,
}

/// 兑换的判决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// 绑好了。
    Bound(RedeemedBinding),
    /// 不存在 / 已消费 / 已过期 / 属于别的 adapter（四者**同**结果）。
    TokenInvalid,
    /// 这个 `DingTalk` id 已属于另一个 Multica 用户。
    AlreadyAssigned,
    /// 兑换者不是该 workspace 的成员。
    NotMember,
}

/// 落一枚令牌的入参（上游 `CreateChannelBindingTokenParams` 的契约子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBindingToken {
    /// **哈希**（调用方算；明文永不入库）。
    pub token_hash: String,
    pub workspace_id: Id,
    pub installation_id: Id,
    pub channel_user_id: String,
    pub expires_at: DateTime<Utc>,
}

// =====================================================================
// 端口
// =====================================================================

/// 绑定面的读写口（上游 `*db.Queries` + `pgx.Tx` 的那几条语句）。
///
/// [`BindingStore::redeem_and_bind`] **必须**在一个事务里按 consume → membership → insert 的
/// 顺序跑（见模块文档的形态差异），且 consume 那一步必须按 `channel_type = 'dingtalk'` 收窄
/// （判据 4）。
#[async_trait]
pub trait BindingStore: Send + Sync {
    /// 落一枚令牌（`token_hash` 冲突 = 调用方重复用哈希 ⇒ 实现报错）。
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String>;

    /// 原子兑换：消费令牌 + 校验成员资格 + 建绑定行。
    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String>;
}

// =====================================================================
// 服务
// =====================================================================

/// 绑定令牌服务（上游 `BindingTokenService`）。
#[derive(Clone)]
pub struct BindingTokenService {
    store: Arc<dyn BindingStore>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl fmt::Debug for BindingTokenService {
    /// 手写：端口不可打印 ⇒ 只说明端口的**存在性**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BindingTokenService")
            .field("store", &"<dyn BindingStore>")
            .finish()
    }
}

impl BindingTokenService {
    /// 装配（时钟为 `Utc::now`）。
    #[must_use]
    pub fn new(store: Arc<dyn BindingStore>) -> Self {
        Self {
            store,
            now: Arc::new(Utc::now),
        }
    }

    /// 注入时钟（用例钉住 `expires_at`）。
    #[must_use]
    pub fn with_now(mut self, now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    /// 铸一枚令牌（上游 `Mint`）：明文只在返回值里；库里只有哈希。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    pub async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<BindingToken, BindingError> {
        let raw = random_binding_token();
        let expires_at = (self.now)() + BINDING_TOKEN_TTL;
        self.store
            .insert_token(&NewBindingToken {
                token_hash: hash_binding_token(&raw),
                workspace_id,
                installation_id,
                channel_user_id: channel_user_id.to_string(),
                expires_at,
            })
            .await
            .map_err(|message| BindingError::Store { message })?;
        Ok(BindingToken { raw, expires_at })
    }

    /// 兑换（上游 `RedeemAndBind`）：`multica_user_id` 来自**会话**，绝不来自令牌。
    ///
    /// # Errors
    ///
    /// 存储层故障（产品判决见 [`RedeemOutcome`]）。
    pub async fn redeem(
        &self,
        raw: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, BindingError> {
        self.store
            .redeem_and_bind(&hash_binding_token(raw), multica_user_id)
            .await
            .map_err(|message| BindingError::Store { message })
    }
}

// =====================================================================
// 纯函数（上游 `randomBindingToken` / `hashBindingToken`）
// =====================================================================

/// 32 字节随机 → base64url（无填充）（上游 `base64.RawURLEncoding`，逐字）。
///
/// 随机源见模块文档（本 crate 无 `rand` 依赖 ⇒ 两个 v4 UUID）。
#[must_use]
pub fn random_binding_token() -> String {
    use base64::Engine as _;

    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 令牌的存储哈希：`sha256(raw)` 的**小写 hex**（上游 `hashBindingToken`，逐字）。
#[must_use]
pub fn hash_binding_token(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    hex::encode(digest)
}

#[cfg(test)]
mod tests;
