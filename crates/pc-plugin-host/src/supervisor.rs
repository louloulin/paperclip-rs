//! Worker supervisor：监听 worker 进程退出，按指数 backoff 重启。
//!
//! 设计（对齐 Node `services/plugin-worker-manager.ts`）：
//! - 监测 `WorkerHandle::is_alive()` / 子进程 exit status
//! - 第一次失败立即重启
//! - 后续失败按 `base_delay_ms * 2^(restart_count-1)` 退避，cap 在 `max_delay_ms`
//! - 超过 `max_restarts` 次数后停止重启，标记 worker 为 `crashed`
//! - 健康事件通过 `tx` 异步发往调用方（live event）

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::handle::{WorkerHandle, WorkerOptions, WorkerState};
use crate::pool::WorkerPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorEvent {
    Restarted {
        plugin_id: Uuid,
        attempt: u32,
        next_delay_ms: u64,
    },
    Crashed {
        plugin_id: Uuid,
        reason: String,
    },
    Recovered {
        plugin_id: Uuid,
    },
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub max_restarts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub poll_interval_ms: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            poll_interval_ms: 1_000,
        }
    }
}

impl SupervisorConfig {
    /// 计算第 N 次重启的退避延迟（毫秒）。
    pub fn backoff_delay_ms(&self, attempt: u32) -> u64 {
        let attempt = attempt.max(1);
        let exp = attempt.saturating_sub(1).min(20);
        let raw = self.base_delay_ms.saturating_mul(1u64 << exp);
        raw.min(self.max_delay_ms)
    }
}

pub struct WorkerSupervisor {
    pool: Arc<WorkerPool>,
    config: SupervisorConfig,
    tx: mpsc::UnboundedSender<SupervisorEvent>,
}

impl WorkerSupervisor {
    pub fn new(pool: Arc<WorkerPool>, config: SupervisorConfig) -> (Self, mpsc::UnboundedReceiver<SupervisorEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { pool, config, tx }, rx)
    }

    pub fn config(&self) -> &SupervisorConfig {
        &self.config
    }

    pub fn pool(&self) -> &Arc<WorkerPool> {
        &self.pool
    }

    /// 启动后台 supervisor 任务，poll worker 状态并按需重启。
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(&self);
        tokio::spawn(async move {
            me.run_loop().await;
        })
    }

    async fn run_loop(self: Arc<Self>) {
        let interval = Duration::from_millis(self.config.poll_interval_ms);
        loop {
            tokio::time::sleep(interval).await;
            self.tick_once().await;
        }
    }

    /// 立即扫描一次所有 worker，重启已死掉的。
    pub async fn tick_once(&self) {
        let ids = self.pool.active_ids().await;
        for plugin_id in ids {
            // 失败重启策略：如果 worker 不再 alive，restart
            let handle = match self.pool.get(&plugin_id).await {
                Some(h) => h,
                None => continue,
            };
            if !handle.is_alive() {
                let state = handle.state().await;
                // 只重启之前启动过且处于 ready/running 状态的 worker；
                // 仍处于 starting 状态的可能是冷启动太慢，跳过。
                if matches!(state, WorkerState::Ready | WorkerState::Running | WorkerState::Error) {
                    if let Err(e) = self.restart_worker(handle.clone()).await {
                        warn!(plugin_id = %plugin_id, error = %e, "supervisor restart failed");
                    }
                }
            }
        }
    }

    /// 重启一个 worker（带指数 backoff）。
    pub async fn restart_worker(&self, handle: Arc<WorkerHandle>) -> Result<(), String> {
        let plugin_id = handle.plugin_id();
        let restart_count = handle.restart_count().await;
        if restart_count >= self.config.max_restarts {
            handle.mark_crashed().await;
            let _ = self.tx.send(SupervisorEvent::Crashed {
                plugin_id,
                reason: format!(
                    "worker crashed after {} restart attempts",
                    restart_count
                ),
            });
            return Err(format!("max restarts {} exceeded", self.config.max_restarts));
        }

        let delay = self.config.backoff_delay_ms(restart_count + 1);
        info!(
            plugin_id = %plugin_id,
            attempt = restart_count + 1,
            delay_ms = delay,
            "restarting worker"
        );

        // 先 shutdown 当前残留
        let _ = handle.shutdown().await;
        // 重新启动
        let options = handle.options_snapshot();
        if let Err(e) = handle.start_with_options(options).await {
            warn!(plugin_id = %plugin_id, error = %e, "worker restart start failed");
            return Err(e);
        }
        handle.bump_restart_count().await;

        let _ = self.tx.send(SupervisorEvent::Restarted {
            plugin_id,
            attempt: restart_count + 1,
            next_delay_ms: delay,
        });

        // backoff sleep 后报告 recovered
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let _ = self.tx.send(SupervisorEvent::Recovered { plugin_id });
        Ok(())
    }

    /// 强制重启 worker（不计入 backoff）。
    pub async fn force_restart(&self, plugin_id: Uuid) -> Result<(), String> {
        let handle = self
            .pool
            .get(&plugin_id)
            .await
            .ok_or_else(|| format!("plugin {plugin_id} not found"))?;
        let options = handle.options_snapshot();
        let _ = handle.shutdown().await;
        handle.start_with_options(options).await?;
        let _ = self.tx.send(SupervisorEvent::Restarted {
            plugin_id,
            attempt: 0,
            next_delay_ms: 0,
        });
        Ok(())
    }

    /// Spawn a fresh worker with options and register it.
    pub async fn spawn_and_register(
        &self,
        options: WorkerOptions,
    ) -> Result<Uuid, String> {
        let handle = self.pool.spawn(options).await?;
        let id = handle.plugin_id();
        self.pool.register(handle).await;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially() {
        let c = SupervisorConfig::default();
        assert_eq!(c.backoff_delay_ms(1), 500);
        assert_eq!(c.backoff_delay_ms(2), 1_000);
        assert_eq!(c.backoff_delay_ms(3), 2_000);
        assert_eq!(c.backoff_delay_ms(4), 4_000);
        assert_eq!(c.backoff_delay_ms(5), 8_000);
        // 16_000 < cap 30_000
        assert_eq!(c.backoff_delay_ms(6), 16_000);
        // 32_000 -> capped to 30_000
        assert_eq!(c.backoff_delay_ms(7), 30_000);
        assert_eq!(c.backoff_delay_ms(20), 30_000);
    }

    #[test]
    fn backoff_attempt_zero_or_one_safe() {
        let c = SupervisorConfig::default();
        // attempt=0 应作为 attempt=1 处理
        assert_eq!(c.backoff_delay_ms(0), c.backoff_delay_ms(1));
    }
}
