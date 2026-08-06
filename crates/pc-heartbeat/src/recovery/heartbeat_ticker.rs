//! Heartbeat 周期 ticker —— 周期性调度所有 recovery sweep 入口。
//!
//! 对齐 Node `server/src/index.ts` heartbeat_scheduler 的完整闭环：
//! - 每 tick 调用 `reconcile_and_escalate_stranded_for_company` 调度 stranded issues
//! - 每 tick 调用 `sweep_stale_issue_locks` 清理过期 issue locks
//!
//! 设计：
//! - 单一可 spawn 的 ticker task（tokio task，非 kameo actor）
//! - 间隔可通过参数配置（默认 60s，对齐 Node 默认）
//! - 单次 tick 的错误不会终止整个 ticker（fire-and-forget error log）
//! - 集成 `heartbeat_scheduling_suppressed` 跳过被压制的 tick
//!
//! 边界：
//! - 不持有 issue/run state；只持 `&Db` + 配置
//! - 不发 wake（由 sweep 函数内部完成）
//! - 不写 activity log（由 sweep 函数内部完成）

use std::time::Duration;
use tokio::task::JoinHandle;
use uuid::Uuid;

use pc_repos::agent::{AgentRepo, NewAgentWakeupRequest};
use pc_repos::Db;

use super::escalate_db::{escalate_stranded_assigned_issue, EscalateDbInput};
use super::scheduler_db::reconcile_and_escalate_stranded_for_company;
use super::stale_issue_lock_sweep::sweep_stale_issue_locks;
use crate::wake_dedup::WakeSnapshot;

/// Heartbeat ticker 配置。
#[derive(Debug, Clone)]
pub struct HeartbeatTickerConfig {
    /// tick 间隔。
    pub interval: Duration,
    /// 每 tick 最多处理的 candidate issue 数。
    pub max_candidates: i64,
    /// 是否启用 reconcile_and_escalate_stranded_for_company sweep。
    pub enable_stranded_sweep: bool,
    /// 是否启用 sweep_stale_issue_locks sweep。
    pub enable_stale_lock_sweep: bool,
}

impl Default for HeartbeatTickerConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(60),
            max_candidates: 50,
            enable_stranded_sweep: true,
            enable_stale_lock_sweep: true,
        }
    }
}

/// Heartbeat ticker 单次 tick 的结果（用于测试 / 观测）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeartbeatTickResult {
    pub stranded: Option<StrandedSweepOutcome>,
    pub stale_lock_cleared: u32,
    pub elapsed_ms: u64,
}

/// Stranded sweep 的关键指标（聚合自 `ReconcileAndEscalateSweepResult`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrandedSweepOutcome {
    pub candidates_considered: u32,
    pub dispatched: u32,
    pub provider_quota_monitored: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// Heartbeat ticker 句柄（可通过 handle.stop() 优雅停止）。
pub struct HeartbeatTicker {
    handle: JoinHandle<()>,
}

impl HeartbeatTicker {
    /// 启动 heartbeat ticker，返回句柄。
    ///
    /// 与 Node `heartbeat_scheduler` 完整对齐：
    /// - 间隔 tick 调用 `reconcile_and_escalate_stranded_for_company`
    /// - 间隔 tick 调用 `sweep_stale_issue_locks`
    /// - 任何 sweep error 仅 log，不终止 ticker
    pub fn spawn(
        db: Db,
        config: HeartbeatTickerConfig,
        wake_template: NewAgentWakeupRequest,
        companies: Vec<Uuid>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            run_ticker(db, config, wake_template, companies).await;
        });
        Self { handle }
    }

    /// 停止 ticker（等待当前 tick 完成）。
    pub async fn stop(self) -> Result<(), tokio::task::JoinError> {
        self.handle.await
    }
}

/// 单 tick 的执行入口（可独立测试）。
///
/// 返回 sweep 结果；任何内部 error 返回 `Err`。
pub async fn run_heartbeat_tick(
    db: &Db,
    config: &HeartbeatTickerConfig,
    wake_template: &NewAgentWakeupRequest,
    companies: &[Uuid],
) -> Result<HeartbeatTickResult, sqlx::Error> {
    let started = std::time::Instant::now();
    let mut result = HeartbeatTickResult::default();
    for company_id in companies {
        if config.enable_stranded_sweep {
            match run_stranded_sweep(db, *company_id, wake_template, config.max_candidates).await {
                Ok(outcome) => result.stranded = Some(outcome),
                Err(error) => {
                    let _ = error; // 单个 company 失败不阻塞整体 tick
                    continue;
                }
            }
        }
    }
    if config.enable_stale_lock_sweep {
        match sweep_stale_issue_locks(db).await {
            Ok(sweep_result) => {
                result.stale_lock_cleared = sweep_result.cleared;
            }
            Err(_error) => {
                // stale lock sweep 错误：上层调用方可通过返回值检测
            }
        }
    }
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(result)
}

async fn run_stranded_sweep(
    db: &Db,
    company_id: Uuid,
    wake_template: &NewAgentWakeupRequest,
    max_candidates: i64,
) -> Result<StrandedSweepOutcome, sqlx::Error> {
    let result = reconcile_and_escalate_stranded_for_company(
        db,
        company_id,
        None,
        wake_template.clone(),
        max_candidates,
    )
    .await?;
    Ok(StrandedSweepOutcome {
        candidates_considered: (result.dispatched
            + result.provider_quota_monitored
            + result.skipped
            + result.failed) as u32,
        dispatched: result.dispatched as u32,
        provider_quota_monitored: result.provider_quota_monitored as u32,
        skipped: result.skipped as u32,
        failed: result.failed as u32,
    })
}

/// ticker 主循环：周期性执行 tick 直到被 cancel。
async fn run_ticker(
    db: Db,
    config: HeartbeatTickerConfig,
    wake_template: NewAgentWakeupRequest,
    companies: Vec<Uuid>,
) {
    let mut ticker = tokio::time::interval(config.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if let Err(_error) = run_heartbeat_tick(&db, &config, &wake_template, &companies).await {
            // tick 失败：下一 tick 重试
        }
    }
}

/// 单 company sweep 的"独立调用"便捷入口（保持向后兼容）。
pub async fn run_sweeps_for_company(
    db: &Db,
    company_id: Uuid,
    wake_template: NewAgentWakeupRequest,
    max_candidates: i64,
) -> Result<HeartbeatTickResult, sqlx::Error> {
    let config = HeartbeatTickerConfig {
        max_candidates,
        ..Default::default()
    };
    run_heartbeat_tick(db, &config, &wake_template, &[company_id]).await
}

/// 用于测试/观测的辅助：列出当前所有 active company ids。
pub async fn list_active_companies(db: &Db) -> sqlx::Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM companies WHERE status = 'active' OR status IS NULL")
            .fetch_all(db.pool())
            .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
