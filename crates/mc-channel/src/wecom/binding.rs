//! `WeCom` 的用户绑定令牌流程（上游 `internal/integrations/wecom/binding.go`，**265 行**）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3）。
//! - **为什么必须有它**（上游注释逐字）：aibot 智能机器人事件里的 `userid` 是
//!   **按 `(bot, user)` 匿名化**的（`T` 前缀），与该企业真实 `userid` / 邮箱**没有任何关系**
//!   ⇒ 任何"隐式身份启发式"（邮箱前缀匹配、corp userid 查表）都不成立。
//!   **一张显式的绑定表是唯一正确的答案**。
//! - **走泛化表**：`channel_binding_token` / `channel_user_binding`，`channel_type='wecom'`
//!   （上游逐字：`mirrors slack.BindingTokenService — runs on the generic channel_binding_token
//!   / channel_user_binding tables with channel_type='wecom'`）。
//!
//! # 四条安全判据（逐条可测）
//!
//! 1. **明文不进库**：只存 `sha256(raw)` 的小写 hex（上游 `hashBindingToken`）；
//! 2. **一个不透明错误盖住三种失败**（不存在 / 已消费 / 已过期）—— 上游逐字：
//!    `One opaque error for all three avoids a replay timing oracle`；
//! 3. **兑换是原子的**：消费令牌、校验成员资格、插绑定行**同一个事务**提交 ⇒
//!    非成员的一次尝试**不烧掉**令牌（上游逐字：`returning before Commit rolls the
//!    consume back`）；
//! 4. **令牌表跨 adapter 共享** ⇒ 别的 adapter 的令牌**不能**从 `WeCom` 兑换。上游的做法是
//!    先 consume 再 `if row.ChannelType != channelTypeWecom { invalid }`，失败靠回滚；
//!    本仓把这条写进 [`BindingStore::redeem_and_bind`] 的 `WHERE` 里（**等价**：
//!    认不出 ⇒ 令牌**没被消费**）。
//!
//! # 铸令牌要节流（上游那段长注释的三条判据，逐条保留）
//!
//! 一个未绑定用户**每发一条消息**都会走到 `needs_binding` ⇒ 不设下限的话，六行字就写六枚
//! 令牌、发六条链接，而用户只会点最后一条。[`BINDING_TOKEN_MINT_INTERVAL`] 是一个
//! **突发窗口**（60s），不是会话级窗口 —— 差别是承重的：抑制一次铸令牌等于断言"已经发出去
//! 的那条链接在用户手上"，而**没有任何东西**能验证这一点（发送只在传输层收下帧时就返回，
//! socket 的 ack 是异步的）。窗口拉长，第一条提示一丢就变成**死胡同**：每条消息都回"点我
//! 发给你的链接"，指向一个从未到达的东西。60s 做到了这个节流本来要做的事
//! （一口气敲的六行仍只写一行），而猜错的代价是**多一条消息**，不是十分钟的机器人复读。
//! 它必须**明显短于** [`BINDING_TOKEN_TTL`]，否则被节流的用户被指回去的那条链接可能正好过期。
//!
//! # 随机性（登记 `docs/32` §31 的 D5）
//!
//! 上游是 `crypto/rand` 读 32 字节再 base64url。本 crate 的依赖集里**没有** `rand`
//! （`docs/60` §3.1 冻结：M7 各片不得新增三方依赖），故取两个 v4 UUID（各 122 bit 熵，
//! 合计 244 bit）拼成 32 字节 —— 熵量级与上游同级（15 分钟单次令牌），**线长也相同**，
//! 只是这 32 字节不是均匀分布。与 M7-9（dingtalk）逐字同一处手法。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! [`BindingToken`] **手写 `Debug`**：明文令牌与它的哈希都不打印（前者是凭据、后者是
//! 可离线比对的物证）；错误变体只带静态文案。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use mc_core::channel::BindingTokenTtl;
use mc_core::id::Id;
use sha2::{Digest, Sha256};

/// 令牌寿命（上游 `BindingTokenTTL = 15 * time.Minute`）。
///
/// 数据库的 `CHECK (expires_at <= created_at + INTERVAL '15 minutes')`
/// （`channel_binding_token_ttl_cap`）钉着同一条上限 ⇒ 配错也铸不出更长的令牌。
pub const BINDING_TOKEN_TTL: Duration = Duration::seconds(900);

/// 一个用户多久可以铸一枚新令牌（上游 `BindingTokenMintInterval = time.Minute`）。
pub const BINDING_TOKEN_MINT_INTERVAL: Duration = Duration::seconds(60);

/// 令牌寿命的**新类型**形态（`mc-core` 的锚点类型；与 [`BINDING_TOKEN_TTL`] 同值）。
#[must_use]
pub fn binding_token_ttl() -> BindingTokenTtl {
    BindingTokenTtl::from_seconds(900).expect("900 ≤ 900")
}

// =====================================================================
// 错误（上游的三个哨兵）
// =====================================================================

/// 绑定面的失败（上游 `ErrBindingTokenInvalid` / `ErrBindingAlreadyAssigned` /
/// `ErrBindingNotWorkspaceMember`）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// 令牌不存在 / 已消费 / 已过期 / **属于别的 adapter**（一个不透明错误，判据 2 与 4）。
    #[error("wecom: binding token invalid or expired")]
    TokenInvalid,
    /// 这个 `WeCom` userid 已经绑到**另一个** Multica 用户（转移必须显式解绑）。
    #[error("wecom: user id is already bound to a different user")]
    AlreadyAssigned,
    /// 兑换者不是令牌所属 workspace 的成员（HTTP 边界翻成 403）。
    #[error("wecom: redeemer is not a workspace member")]
    NotMember,
    /// 存储层故障（不透明：不回显 SQL / 参数）。
    #[error("wecom: binding store failure: {message}")]
    Store { message: String },
}

impl BindingError {
    /// 稳定错误码（诊断 / 日志用；**不回显**任何凭据或明文）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TokenInvalid => "wecom_binding_token_invalid",
            Self::AlreadyAssigned => "wecom_binding_already_assigned",
            Self::NotMember => "wecom_binding_not_member",
            Self::Store { .. } => "wecom_binding_store_error",
        }
    }

    /// HTTP 状态码（上游 `RedeemWecomBindingToken` 的 switch：410 / 409 / 403）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::TokenInvalid => 410,
            Self::AlreadyAssigned => 409,
            Self::NotMember => 403,
            Self::Store { .. } => 500,
        }
    }

    /// 从兑换判决映射（`Bound` ⇒ `None`）。
    #[must_use]
    pub fn from_redeem(outcome: &RedeemOutcome) -> Option<Self> {
        match outcome {
            RedeemOutcome::Bound(_) => None,
            RedeemOutcome::TokenInvalid => Some(Self::TokenInvalid),
            RedeemOutcome::AlreadyAssigned => Some(Self::AlreadyAssigned),
            RedeemOutcome::NotMember => Some(Self::NotMember),
        }
    }
}

// =====================================================================
// 值对象
// =====================================================================

/// 一枚刚铸出的令牌（上游 `BindingToken`）：**明文只在返回值里出现一次**。
///
/// 手写 `Debug`（凭据纪律）：明文与哈希都不打印。
#[derive(Clone, PartialEq, Eq)]
pub struct BindingToken {
    /// 明文（嵌进绑定链接，经 aibot WebSocket 交给用户；**不落库、不进日志**）。
    ///
    /// [`BindingToken::reused`] 为真时它是**空串**：那个明文从来没有被存下来过
    /// （表里只有哈希），所以"再给一次"在物理上不可能 —— 调用方只能把用户指回上一条消息。
    pub raw: String,
    /// 过期时间（`channel_binding_token.expires_at`）。
    pub expires_at: DateTime<Utc>,
    /// 节流命中（上游 `Reused`）：这次**没有**写新行，`raw` 为空。
    pub reused: bool,
}

impl fmt::Debug for BindingToken {
    /// 手写脱敏：明文与哈希都不打印。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BindingToken")
            .field("raw", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("reused", &self.reused)
            .finish()
    }
}

impl BindingToken {
    /// 这次只是"上一条链接还在你手里"（上游 `Reused`）。
    #[must_use]
    pub fn is_reused(&self) -> bool {
        self.reused
    }
}

/// 兑换成功的结果（上游 `RedeemedBindingToken`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedBinding {
    pub workspace_id: Id,
    pub installation_id: Id,
    /// 令牌里的 `WeCom` userid（**不是**兑换者的 —— 那来自会话）。
    pub channel_user_id: String,
}

/// 兑换的判决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// 绑好了。
    Bound(RedeemedBinding),
    /// 不存在 / 已消费 / 已过期 / 属于别的 adapter（四者**同**结果）。
    TokenInvalid,
    /// 这个 `WeCom` userid 已属于另一个 Multica 用户。
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

/// 绑定面的读写口（上游 `bindingMintQueries` + `*db.Queries` + `pgx.Tx` 的那几条语句）。
///
/// [`BindingStore::redeem_and_bind`] **必须**在一个事务里按
/// consume → membership → insert 的顺序跑，且 consume 那一步必须按
/// `channel_type = 'wecom'` 收窄（判据 4）。
#[async_trait]
pub trait BindingStore: Send + Sync {
    /// 落一枚令牌（`token_hash` 冲突 = 调用方重复用哈希 ⇒ 实现报错）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String>;

    /// 查"这个用户在这个安装上是否已有一枚**活着**的令牌"（上游
    /// `FindLiveChannelBindingToken`：未消费、未过期、且 `created_at` 在铸令牌间隔之内）。
    ///
    /// 返回活令牌的**过期时间**（节流命中时调用方要把它回给用户）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn find_live_token(
        &self,
        installation_id: Id,
        channel_user_id: &str,
        mint_interval: Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, String>;

    /// 原子兑换：消费令牌 + 校验成员资格 + 建绑定行。
    ///
    /// # Errors
    ///
    /// 存储层故障（产品判决见 [`RedeemOutcome`]）。
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
    /// 手写：端口与时钟都不可打印 ⇒ 只说明端口的存在性。
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

    /// 注入时钟（用例钉住 `expires_at` 与节流窗口）。
    #[must_use]
    pub fn with_now(mut self, now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    /// 铸一枚令牌（上游 `Mint`）：明文只在返回值里，库里只有哈希。
    ///
    /// 节流命中 ⇒ [`BindingToken::reused`] 为真、`raw` 为空（见模块文档那段长注释）。
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
        let now = (self.now)();
        let live = self
            .store
            .find_live_token(
                installation_id,
                channel_user_id,
                BINDING_TOKEN_MINT_INTERVAL,
                now,
            )
            .await
            .map_err(|message| BindingError::Store { message })?;
        if let Some(expires_at) = live {
            return Ok(BindingToken {
                raw: String::new(),
                expires_at,
                reused: true,
            });
        }

        let raw = random_binding_token();
        let expires_at = now + BINDING_TOKEN_TTL;
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
        Ok(BindingToken {
            raw,
            expires_at,
            reused: false,
        })
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
