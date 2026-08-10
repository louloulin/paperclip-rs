//! 提供方 retry 助手。
//!
//! 在 typed error 之上提供 `retry_transient` / `with_backoff` 两个
//! 公开函数，让新 provider（Vault AppRole、AWS STS chain）用同一套
//! 重试策略。
//!
//! 行为：
//! - 仅对 [`SecretProviderError::is_transient`] == true 的错误重试；
//! - 指数 backoff：`base * 2^attempt`，封顶 `max_delay`；
//! - 总尝试次数 = `max_attempts`；
//! - 限流（429）时使用上游 `Retry-After`（若有），否则按 backoff 计算；
//! - 错误消息保留 typed category，便于日志聚合。

use std::future::Future;
use std::time::Duration;

use crate::error::{SecretErrorCategory, SecretProviderError};

/// 重试策略。
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最大尝试次数（含首次）。
    pub max_attempts: u32,
    /// 初始 backoff。
    pub base_delay: Duration,
    /// 最大 backoff 封顶。
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(5),
        }
    }
}

impl RetryPolicy {
    #[must_use]
    pub fn new(max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay,
        }
    }

    /// 给定 0-based 失败序号（0 = 第一次失败后要等多久），返回 backoff 时长。
    /// 公式：`base * 2^(attempt+1)`，封顶 `max_delay`。
    #[must_use]
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        // 2^(attempt+1) 但不溢出；attempt 上限设到 30 以防 saturating_pow 触发
        // panic：2^31 已经在 u32 边界，所以最多 attempt+1=30。
        let exp = 2u32.saturating_pow(attempt.min(30) + 1);
        let delay = self.base_delay.saturating_mul(exp);
        delay.min(self.max_delay)
    }
}

/// 对一个 future 运行重试，仅对 transient 错误重试。
///
/// 调用方提供 typed error；老 provider 如果仍返回 `String`
/// 可先用 [`SecretProviderError::classify`] 包装。
pub async fn retry_transient<F, Fut, T>(
    policy: &RetryPolicy,
    mut op: F,
) -> Result<T, SecretProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, SecretProviderError>>,
{
    let mut last_err: Option<SecretProviderError> = None;
    for attempt in 0..policy.max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !e.is_transient() || attempt + 1 == policy.max_attempts {
                    return Err(e);
                }
                let delay = match e.retry_after {
                    Some(ra) if ra < policy.max_delay => ra,
                    _ => policy.backoff_for(attempt),
                };
                tokio::time::sleep(delay).await;
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        SecretProviderError::new(SecretErrorCategory::Other, "retry exhausted without error")
    }))
}

/// Convenience: 把 `Result<T, String>` 适配为 `Result<T, SecretProviderError>`
/// 并按 policy 重试。
///
/// 实现与 [`retry_transient`] 等价，只是在循环里把 String 错误分类为
/// typed error，避免捕获异步块逃逸出 `FnMut` 闭包。
pub async fn retry_string<F, Fut, T>(
    policy: &RetryPolicy,
    mut op: F,
) -> Result<T, SecretProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut last_err: Option<SecretProviderError> = None;
    for attempt in 0..policy.max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(msg) => {
                let e = SecretProviderError::classify(&msg);
                if !e.is_transient() || attempt + 1 == policy.max_attempts {
                    return Err(e);
                }
                let delay = match e.retry_after {
                    Some(ra) if ra < policy.max_delay => ra,
                    _ => policy.backoff_for(attempt),
                };
                tokio::time::sleep(delay).await;
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        SecretProviderError::new(SecretErrorCategory::Other, "retry exhausted without error")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn retry_succeeds_on_third_attempt() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let policy = RetryPolicy::new(5, Duration::from_millis(1), Duration::from_millis(5));
        let res: Result<&str, _> = retry_string(&policy, || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 {
                    Err("request timed out".to_string())
                } else {
                    Ok("ok")
                }
            }
        })
        .await;
        assert_eq!(res.unwrap(), "ok");
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_does_not_retry_auth_failure() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let policy = RetryPolicy::new(5, Duration::from_millis(1), Duration::from_millis(5));
        let res: Result<&str, _> = retry_string(&policy, || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err("invalid token".to_string())
            }
        })
        .await;
        let err = res.unwrap_err();
        assert_eq!(err.category(), SecretErrorCategory::Auth);
        assert_eq!(counter.load(Ordering::SeqCst), 1, "auth should not retry");
    }

    #[tokio::test]
    async fn retry_exhausts_and_returns_last_error() {
        let policy = RetryPolicy::new(3, Duration::from_millis(1), Duration::from_millis(5));
        let res: Result<(), _> = retry_string(&policy, || async {
            Err("HTTP 503".to_string())
        })
        .await;
        let err = res.unwrap_err();
        assert_eq!(err.category(), SecretErrorCategory::Upstream);
    }

    #[test]
    fn backoff_grows_exponentially() {
        let p = RetryPolicy::new(10, Duration::from_millis(100), Duration::from_secs(2));
        assert_eq!(p.backoff_for(0), Duration::from_millis(200));
        assert_eq!(p.backoff_for(1), Duration::from_millis(400));
        assert_eq!(p.backoff_for(2), Duration::from_millis(800));
        assert_eq!(p.backoff_for(3), Duration::from_millis(1600));
        assert_eq!(p.backoff_for(4), Duration::from_millis(2000)); // capped at 2s
        assert_eq!(p.backoff_for(20), Duration::from_millis(2000));
    }

    #[test]
    fn backoff_max_attempts_floored_at_one() {
        let p = RetryPolicy::new(0, Duration::from_millis(10), Duration::from_millis(100));
        assert_eq!(p.max_attempts, 1);
    }
}
