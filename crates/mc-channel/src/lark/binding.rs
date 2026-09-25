//! lark **绑定令牌面**：铸一枚单次令牌 / 兑换并绑 / 安装者直接绑定
//! （上游 `internal/integrations/lark/binding_token.go`，**304 行**）。
//!
//! - **写者**：M7-14（`docs/60-M7-PLAN.md` §3.3）。
//! - **本文件是 `POST /api/lark/binding/redeem` 的实现侧**：那是唯一一条由**用户动作**
//!   写出 `lark_user_binding` 一行的路径。兑换者的身份取自**会话**、不是令牌 ——
//!   偷来的令牌因此绑不到攻击者的账号上；令牌只证明"这个 `open_id` 请求过绑定"，
//!   把它与登录用户合成才是那条 `(open_id ↔ user)` 映射。
//! - **表的选择**：**遗留** `lark_binding_token` / `lark_user_binding`（与
//!   [`super::installation`] 的 **D1** 同一条口径 —— 上游转发到 `channel_*`，本仓按合并树的
//!   硬约束走遗留表）。
//!
//! # 三条从上游逐字搬来的判决
//!
//! 1. **明文令牌只出现一次**：`Mint` 是包里唯一产出明文的地方，之后服务端只有哈希
//!    （`sha256` 十六进制）⇒ 从库里恢复不出原文。明文必须经安全信道送给收件人
//!    （Lark 私聊在传输中被平台加密），**永不**落日志；
//! 2. **兑换是事务性的**：消费令牌与插 `lark_user_binding` 一起提交 ⇒ 兑换失败**不会**
//!    烧掉令牌，成功**不会**留下一枚"已消费但没用"的令牌；
//! 3. **三种失败各有自己的状态码**（[`BindingError::http_status`]）：令牌未知 / 已消费 /
//!    已过期统一成 **410 Gone**（**故意不分**：可区分会产生令牌重放的计时预言机，且对用户
//!    没有产品价值）；同一个 `open_id` 已属于**别的**用户 ⇒ **409**；兑换者不是成员 ⇒ **403**。
//!
//! # 与上游的形态差异（登记 `docs/32` §30 的 **D3**）
//!
//! 上游 `RedeemAndBind` 自己开事务、自己跑三次 `qtx.*`；本仓把它整段落成端口方法
//! [`BindingStore::redeem_and_bind`]（SQL 在 `mc-http` 的实现里），于是 `mc-channel` 里
//! 只剩**判决**与**哈希**这两件可测的纯逻辑。上游的 `BindInstallerTx`（设备流成功路径上的
//! 安装者自动绑定）不在本 trait 上：它必须与安装行插入**同一个**事务 ⇒ 折进
//! [`super::registration::RegistrationStore::commit_install`]（同一个事实，只有一个归属）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 本文件**没有**任何 `tracing::*`；[`BindingError`] 只带稳定码与静态文案；
//! 明文令牌只在 [`MintedBinding::raw`] 里出现一次，且**不**派生能打印它的类型。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::id::Id;

use super::replier::{BindingTokenMinter, MintedBinding};
use super::types::BINDING_TOKEN_TTL;

/// 兑换成功之后的三个字段（上游 `RedeemedBindingToken`）——
/// 前端据此渲染"你已通过 <workspace> 绑定"，不再多取一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedBinding {
    /// 令牌所属 workspace。
    pub workspace_id: Id,
    /// 令牌所属安装。
    pub installation_id: Id,
    /// 令牌里的 Lark `open_id`（**逐字**，不归一）。
    pub lark_open_id: String,
}

/// 绑定面的失败（上游三个哨兵 + 两个不透明档）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// 令牌不存在 / 已消费 / 已过期（**三合一**，见模块文档第 3 条；HTTP 410）。
    #[error("binding token invalid or expired")]
    TokenInvalid,
    /// 这个 `open_id` 已绑到**别的** Multica 用户（HTTP 409）。
    #[error("this Lark account is already bound to a different Multica user")]
    AlreadyAssigned,
    /// 兑换者不是该 workspace 的成员（HTTP 403）。
    #[error("binding refused (are you a workspace member?)")]
    NotWorkspaceMember,
    /// 存储层故障（不透明；HTTP 500）。
    #[error("lark: binding store failure: {message}")]
    Store {
        /// 端口回的错误正文。
        message: String,
    },
}

impl BindingError {
    /// 稳定错误码（上游 `writeError` 的文案族）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TokenInvalid => "lark_binding_token_invalid",
            Self::AlreadyAssigned => "lark_binding_already_assigned",
            Self::NotWorkspaceMember => "lark_binding_not_workspace_member",
            Self::Store { .. } => "lark_binding_failed",
        }
    }

    /// 上游响应矩阵的状态码。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::TokenInvalid => 410,
            Self::AlreadyAssigned => 409,
            Self::NotWorkspaceMember => 403,
            Self::Store { .. } => 500,
        }
    }

    /// 从一次兑换判决映射（`Bound` 没有错误形态 ⇒ `None`）。
    #[must_use]
    pub fn from_redeem(outcome: &RedeemOutcome) -> Option<Self> {
        match outcome {
            RedeemOutcome::Bound(_) => None,
            RedeemOutcome::TokenInvalid => Some(Self::TokenInvalid),
            RedeemOutcome::AlreadyAssigned => Some(Self::AlreadyAssigned),
            RedeemOutcome::NotWorkspaceMember => Some(Self::NotWorkspaceMember),
        }
    }
}

/// 一次兑换的判决（端口 `redeem_and_bind` 的返回；**事务已在实现里提交/回滚**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// 消费 + 绑定都在同一个事务里落好了。
    Bound(RedeemedBinding),
    /// 令牌不存在 / 已消费 / 已过期。
    TokenInvalid,
    /// `(installation, open_id)` 上的绑定已存在，且指向**别的**用户。
    AlreadyAssigned,
    /// 兑换者不是令牌所属 workspace 的成员。
    NotWorkspaceMember,
}

/// 绑定面的存储口（实现住在 `mc-http`；见模块文档的 **D3**）。
#[async_trait]
pub trait BindingStore: Send + Sync {
    /// 落一枚令牌的**哈希**（明文不进来 —— 服务边界之外它已经不存在）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn insert_token(
        &self,
        token_hash: &str,
        workspace_id: Id,
        installation_id: Id,
        lark_open_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), String>;

    /// 一个事务里：消费令牌 → 成员校验 → 插绑定。
    ///
    /// # Errors
    ///
    /// 存储层故障（三个业务判决走 `Ok(RedeemOutcome::…)`，不是 `Err`）。
    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String>;
}

/// 绑定令牌服务（上游 `BindingTokenService`）。
///
/// **不派生 `Debug`**：它持有端口与时钟（"能打印"本身不该存在）。
pub struct BindingTokenService {
    store: Arc<dyn BindingStore>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl BindingTokenService {
    /// 用真实时钟装配（上游 `NewBindingTokenService`）。
    #[must_use]
    pub fn new(store: Arc<dyn BindingStore>) -> Self {
        Self::with_clock(store, Arc::new(Utc::now))
    }

    /// 注入时钟（过期行为要确定性 ⇒ 测试钉住时间而不是睡觉；上游
    /// `NewBindingTokenServiceWithClock` 的 seam）。
    #[must_use]
    pub fn with_clock(
        store: Arc<dyn BindingStore>,
        now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        Self { store, now }
    }

    /// 铸一枚单次令牌并落它的哈希，返回**明文 + 过期时刻**。
    ///
    /// 明文必须经安全信道送给目标收件人，**永不**落日志。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    pub async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        open_id: &str,
    ) -> Result<MintedBinding, String> {
        let raw = random_binding_token();
        let expires_at = (self.now)() + BINDING_TOKEN_TTL;
        self.store
            .insert_token(
                &hash_token(&raw),
                workspace_id,
                installation_id,
                open_id,
                expires_at,
            )
            .await?;
        Ok(MintedBinding { raw, expires_at })
    }

    /// 兑换一枚令牌并写入绑定（**一个事务**）。
    ///
    /// # Errors
    ///
    /// 三个业务判决（[`BindingError`]）+ 存储层故障。
    pub async fn redeem_and_bind(
        &self,
        raw: &str,
        multica_user_id: Id,
    ) -> Result<RedeemedBinding, BindingError> {
        let outcome = self
            .store
            .redeem_and_bind(&hash_token(raw), multica_user_id)
            .await
            .map_err(|message| BindingError::Store { message })?;
        match outcome {
            RedeemOutcome::Bound(bound) => Ok(bound),
            other => Err(BindingError::from_redeem(&other).unwrap_or(BindingError::TokenInvalid)),
        }
    }
}

#[async_trait]
impl BindingTokenMinter for BindingTokenService {
    /// 满足 M7-13 出站回复器（`crate::lark::replier`）的端口 —— 它只记 warn、**不**回显令牌。
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        open_id: &str,
    ) -> Result<MintedBinding, String> {
        BindingTokenService::mint(self, workspace_id, installation_id, open_id).await
    }
}

/// 一枚随机令牌的明文（上游 `randomToken(32)`）。
///
/// 上游是 `crypto/rand` 读 32 字节再 base64url；本 crate 的依赖集里**没有** `rand`
/// （M7-0 一次接好的依赖边，`docs/60` §2.2）⇒ 用**两个** v4 UUID 拼出同样 32 字节的
/// 密码学随机（`uuid` 的 v4 走的是同一族的 CSPRNG）。与
/// `wecom::binding::random_binding_token` 同一手法。
///
/// base64url 且无填充 ⇒ 令牌能干净地嵌进绑定 URL，不带需要转义的字符。
#[must_use]
pub fn random_binding_token() -> String {
    use base64::Engine as _;

    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 令牌的落库形态：`sha256` 的十六进制小写（上游 `hashToken`）。
#[must_use]
pub fn hash_token(raw: &str) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests;
