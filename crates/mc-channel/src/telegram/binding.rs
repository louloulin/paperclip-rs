//! Telegram 的用户绑定令牌流程：**铸**一枚单次令牌 → 用户在 web app 里**兑换** →
//! Telegram 用户 id 绑到 Multica 账号
//! （上游 `internal/integrations/telegram/binding.go`，171 行）。
//!
//! - **写者**：M7-5（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §17.1）。
//! - **上游定位**：未绑定的 Telegram 用户给 bot 发消息 ⇒ 出站回复器（`replier.rs`）铸一枚
//!   令牌并回「点这里绑定」；用户点进 `AppURL + /telegram/bind?token=…`；
//!   `POST /api/telegram/binding/redeem` 把令牌里的 Telegram user id 绑到**会话身份**
//!   （不是令牌里的）。
//! - **走泛化表**：`channel_binding_token` / `channel_user_binding`，`channel_type='telegram'`
//!   （与 Slack 面同形：上游 lark 的 `ChannelStore` 把 `feishu` 写死，所以不能复用它的服务）。
//!
//! # 四条安全判据（逐条可测）
//!
//! 1. **明文不进库**：只存 `sha256(raw)` 的 hex（上游 `hashBindingToken`）。
//! 2. **同一个不透明错误覆盖三种失败**（不存在 / 已消费 / 已过期）—— 上游逐字：
//!    `One opaque error avoids a replay timing oracle`。
//! 3. **兑换是原子的**：消费令牌、校验成员资格、插绑定行**同一事务**提交 ⇒ 非成员的一次
//!    尝试**不烧掉**令牌（上游逐字：`returning before Commit rolls the consume back`）。
//! 4. **令牌表跨 adapter 共享** ⇒ 别的 adapter 的令牌**不能**从 Telegram 兑换。上游的做法是
//!    先 consume 再 `validateBindingTokenChannel`，失败时靠外层事务回滚；本仓把这条写进
//!    [`BindingStore::redeem_and_bind`] 的 `WHERE` 里（**等价**：认不出 ⇒ 令牌**没被消费**）。
//!
//! # 幂等（本片的专属验收）
//!
//! 重复兑换同一枚令牌 ⇒ 第二次是 [`RedeemOutcome::TokenInvalid`]（`consumed_at` 已被 CAS
//! 占住），**不会**重复插 `channel_user_binding` 行；而"同一个用户重复绑同一个
//! `(installation, 平台用户 id)`"由实现的 `ON CONFLICT … DO UPDATE … WHERE
//! multica_user_id = EXCLUDED…` 保证既不重复插行、也不把别人的绑定抢过来。
//!
//! # 与上游的一处形态差异（登记 `docs/32` §17.2）
//!
//! 上游的 `RedeemAndBind` 自己持有 `pgx.Tx` 并逐条驱动 SQL；本仓把"一个事务里的三段"收进
//! [`BindingStore::redeem_and_bind`] 这**一个端口方法**（层次铁律：channel 层不碰 SQL）
//! ⇒ 业务判决（三种错误、成员闸门、跨 adapter 拒绝）仍在**本文件**可测，而"原子性"由实现
//! 保证；实现必须逐字照上游顺序：consume → membership → insert，失败即回滚。
//!
//! # 随机性（登记 `docs/32` §17.2）
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
///
/// 数据库的 `CHECK` 钉着同一条上限 ⇒ 配错也铸不出更长的令牌。
pub const BINDING_TOKEN_TTL: Duration = Duration::minutes(15);

// =====================================================================
// 错误（上游的三个哨兵）
// =====================================================================

/// 绑定面的失败（上游 `ErrBindingTokenInvalid` / `ErrBindingAlreadyAssigned` /
/// `ErrBindingNotWorkspaceMember`）。
///
/// `redeem` 的判决由 [`RedeemOutcome`] 表达（那是**产品**结果）；本枚举给铸令牌与存储故障用。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// 令牌不存在 / 已消费 / 已过期 / **属于别的 adapter**（同一个不透明错误，判据 2 与 4）。
    #[error("telegram: binding token invalid or expired")]
    TokenInvalid,
    /// 这个 Telegram user id 已绑到**另一个** Multica 用户（转移必须显式解绑）。
    #[error("telegram: user id is already bound to a different user")]
    AlreadyAssigned,
    /// 兑换者不是令牌所属 workspace 的成员（HTTP 边界翻成 403）。
    #[error("telegram: redeemer is not a workspace member")]
    NotMember,
    /// 存储层故障（不透明：不回显 SQL / 参数）。
    #[error("telegram: binding store failure: {message}")]
    Store { message: String },
}

impl BindingError {
    /// 稳定错误码（诊断 / 日志用；**不回显**任何凭据或明文）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TokenInvalid => "telegram_binding_token_invalid",
            Self::AlreadyAssigned => "telegram_binding_already_assigned",
            Self::NotMember => "telegram_binding_not_member",
            Self::Store { .. } => "telegram_binding_store_error",
        }
    }

    /// HTTP 状态码（上游 `handler/telegram.go` 的 switch：410 / 409 / 403）。
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
    pub raw: String,
    pub expires_at: DateTime<Utc>,
}

/// 兑换成功的结果（上游 `RedeemedBindingToken`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedBinding {
    pub workspace_id: Id,
    pub installation_id: Id,
    /// 令牌里的 Telegram user id（**不是**兑换者的 —— 那来自会话）。
    pub channel_user_id: String,
}

/// 兑换的判决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// 绑好了。
    Bound(RedeemedBinding),
    /// 不存在 / 已消费 / 已过期 / 属于别的 adapter（四者**同**结果）。
    TokenInvalid,
    /// 这个 Telegram id 已属于另一个 Multica 用户。
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
/// [`BindingStore::redeem_and_bind`] **必须**在一个事务里按
/// consume → membership → insert 的顺序跑（见模块文档的形态差异），且 consume 那一步必须
/// 按 `channel_type = 'telegram'` 收窄（判据 4）。
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
/// 随机源见模块文档：两个 v4 UUID 拼成 32 字节（本 crate 无 `rand` 依赖）。
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
