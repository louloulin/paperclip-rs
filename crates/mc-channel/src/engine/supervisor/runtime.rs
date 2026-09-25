//! 监管器的运行期自由函数：续租循环 + 取消等待 + 令牌/时间辅助。
//!
//! - **写者**：M7-1。上游 `engine/supervisor.go` 的 `renewLeaseUntil` / `sleep` / `jitter` /
//!   `leaseToken` / `newNodeID` 一批。
//! - 拆出本文件是门 ⑩ 的要求（`supervisor.rs` 超 800 行 ⇒ 拆）。

use std::sync::Arc;
use std::time::Duration;

use mc_core::timestamp::Timestamp;
use tokio::sync::watch;

use crate::channel::ChannelConfig;
use crate::engine::resolvers::{EngineError, PipelineError};
use crate::message::SharedInboundHandler;

use super::ports::{AcquireLeaseParams, Installation, LeaseStore};
use super::Config;

/// 续租循环：按 `lease_renew_interval` 续；丢租约 / 传输错误各自处理。
pub(super) async fn renew_loop(
    leases: Arc<dyn LeaseStore>,
    row: Installation,
    token: String,
    cfg: Config,
    lease_lost: Arc<tokio::sync::Notify>,
    mut stop: watch::Receiver<bool>,
) {
    let mut confirmed_until = plus((cfg.now)(), cfg.lease_ttl).as_datetime()
        - chrono_duration(cfg.lease_expiry_safety_margin);
    let mut delay = cfg.lease_renew_interval;
    loop {
        let remaining = confirmed_until - (cfg.now)().as_datetime();
        if remaining <= chrono::Duration::zero() {
            tracing::warn!(installation_id = %row.id, "channel engine: last confirmed lease expired");
            lease_lost.notify_waiters();
            return;
        }
        let wait = delay.min(remaining.to_std().unwrap_or_default());
        tokio::select! {
            biased;
            () = wait_stop(&mut stop) => return,
            () = tokio::time::sleep(wait) => {}
        }
        let started = (cfg.now)();
        let attempt = leases
            .renew(AcquireLeaseParams {
                installation_id: row.id,
                kind: row.kind,
                token: token.clone(),
                expires_at: plus(started, cfg.lease_ttl),
                ttl: cfg.lease_ttl,
            })
            .await;
        match attempt {
            Ok(()) => {
                confirmed_until = plus(started, cfg.lease_ttl).as_datetime()
                    - chrono_duration(cfg.lease_expiry_safety_margin);
                delay = cfg.lease_renew_interval;
            }
            Err(error) if is_lease_held(&error) => {
                tracing::warn!(installation_id = %row.id, "channel engine: lease lost; tearing down connection");
                lease_lost.notify_waiters();
                return;
            }
            Err(error) => {
                tracing::warn!(installation_id = %row.id, code = error.code_hint(), "channel engine: lease renewal error");
                delay = cfg.lease_error_retry_interval;
            }
        }
    }
}

/// 造 Channel 的配置（engine **从不**读 `raw` 里面）。
pub(super) fn channel_config(row: &Installation, handler: &SharedInboundHandler) -> ChannelConfig {
    ChannelConfig {
        kind: row.kind,
        raw: row.config.clone(),
        installation_id: Some(row.id),
        handler: Some(Arc::clone(handler)),
    }
}

/// 等停止信号（已经置位则立即返回）。
pub(super) async fn wait_stop(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    let _ = stop.changed().await;
}

/// 可取消的等待：返回 `true` = 等之前就被取消了（调用方应当收尾返回）。
pub(super) async fn sleep_cancellable(
    duration: Duration,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    if duration.is_zero() {
        return *stop.borrow();
    }
    tokio::select! {
        biased;
        () = wait_stop(stop) => true,
        () = tokio::time::sleep(duration) => false,
    }
}

pub(super) fn is_lease_held(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Pipeline(PipelineError::LeaseNotAcquired)
    )
}

pub(super) fn chrono_duration(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or_default()
}

pub(super) fn plus(timestamp: Timestamp, duration: Duration) -> Timestamp {
    Timestamp::from(timestamp.as_datetime() + chrono_duration(duration))
}

pub(super) fn elapsed(started: &Timestamp, now: &Timestamp) -> Duration {
    (now.as_datetime() - started.as_datetime())
        .to_std()
        .unwrap_or_default()
}

/// 每监管任务的租约令牌：进程级 `node_id` + 任务的 generation。
///
/// 令牌让**同一进程内**前后两个任务（轮换路径）拿到不同的令牌 —— 老任务的迟到释放就不会 CAS
/// 命中并删掉后继者刚拿到的租约。
///
/// ⚠️ 这是**内部 CAS 标记，不是凭据**：从不发给任何平台，单独存在也换不到任何权限。即便如此
/// 也**不**进日志字段（上游 GH #7132 把明文 `lease_token=` 字段当成了泄漏凭据）；这里只打
/// `node_id` + `lease_gen`。
pub(super) fn lease_token(node_id: &str, generation: u64) -> String {
    format!("{node_id}-g{generation}")
}

/// 每进程唯一的 16 字节 hex 形态令牌（上游 `newNodeID`；不引随机数依赖）。
pub(super) fn new_node_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    let folded = u64::try_from(nanos).unwrap_or(u64::MAX) ^ ((nanos >> 64) as u64);
    format!("{pid:08x}{folded:016x}")
}
