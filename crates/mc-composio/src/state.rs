//! callback state 的 HMAC 签发与校验 —— 上游 `integrations/composio/state.go`（92 行）
//! （M8-0 anchor 建桩，**实现归 M8-6**）。
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

/// state 的 HMAC 签发器（**不派生 `Debug`**：secret 绝不进日志）。
pub struct StateSigner {
    /// state secret（M8-6 的签发/校验用它；anchor 期还没被读）。
    #[allow(dead_code)]
    secret: String,
}

impl StateSigner {
    /// 从 secret 构造。
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    /// 签发一个绑定 `subject`（`user_id` / `workspace_id`）的 state —— **anchor 期是桩**，
    /// 实现归 M8-6。
    pub fn sign(&self, _subject: &str, _now_unix: i64) -> String {
        todo!("M8-6：state 签发（timestamp + nonce + HMAC）（docs/61 §4.1 的 M8-6 行）")
    }

    /// 校验 state 并解出 subject —— **anchor 期是桩**，实现归 M8-6。
    ///
    /// # Errors
    ///
    /// 篡改 / 过期 / 重放 / 格式非法四类，逐条对应 [`StateError`]。
    pub fn verify(&self, _state: &str, _now_unix: i64) -> Result<String, StateError> {
        todo!("M8-6：state 校验（四个反例）（docs/61 §6.5 的 M8-6 行）")
    }
}

impl std::fmt::Debug for StateSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateSigner")
            .field("secret", &"<redacted>")
            .finish()
    }
}
