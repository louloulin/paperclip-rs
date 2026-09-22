//! 验证码（多因素 / 邮箱验证 / 邀请码）。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use mc_core::Id;

/// 验证码用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCodePurpose {
    EmailVerification,
    PasswordReset,
    TwoFactor,
    WorkspaceInvite,
}

impl VerificationCodePurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::TwoFactor => "two_factor",
            Self::WorkspaceInvite => "workspace_invite",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationCode {
    pub id: Id,
    pub user_id: Option<Id>,
    pub email: Option<String>,
    pub purpose: VerificationCodePurpose,
    pub code_hash: String,
    pub attempts: u32,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait VerificationCodeStore: Send + Sync {
    async fn put(&self, code: VerificationCode) -> anyhow::Result<()>;
    async fn get(&self, id: Id) -> anyhow::Result<Option<VerificationCode>>;
    async fn increment_attempts(&self, id: Id) -> anyhow::Result<u32>;
    async fn consume(&self, id: Id) -> anyhow::Result<()>;
}

#[derive(Default, Clone)]
pub struct InMemoryVerificationStore {
    inner: Arc<RwLock<HashMap<Id, VerificationCode>>>,
}

#[async_trait]
impl VerificationCodeStore for InMemoryVerificationStore {
    async fn put(&self, code: VerificationCode) -> anyhow::Result<()> {
        let mut guard = self.inner.write().unwrap();
        guard.insert(code.id, code);
        Ok(())
    }

    async fn get(&self, id: Id) -> anyhow::Result<Option<VerificationCode>> {
        let guard = self.inner.read().unwrap();
        Ok(guard.get(&id).cloned())
    }

    async fn increment_attempts(&self, id: Id) -> anyhow::Result<u32> {
        let mut guard = self.inner.write().unwrap();
        if let Some(c) = guard.get_mut(&id) {
            c.attempts += 1;
            Ok(c.attempts)
        } else {
            Ok(0)
        }
    }

    async fn consume(&self, id: Id) -> anyhow::Result<()> {
        let mut guard = self.inner.write().unwrap();
        if let Some(c) = guard.get_mut(&id) {
            c.consumed_at = Some(Utc::now());
        }
        Ok(())
    }
}

/// 生成 6 位数字验证码。
pub fn generate_code() -> String {
    let mut rng = rand::thread_rng();
    let n: u32 = rng.gen_range(0..1_000_000);
    format!("{n:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_code_format() {
        for _ in 0..100 {
            let c = generate_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|c| c.is_ascii_digit()));
        }
    }
}
