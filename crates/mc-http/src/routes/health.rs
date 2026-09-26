//! `/api/health` / `/api/health/db` —— 本仓**自造**的两个探针（`local_only` 登记项）。
//!
//! 保留理由（`docs/64` §9.3，两条）：① `GET /api/health` **不是**上游 `/health` 的别名 ——
//! 它的语义是「服务 + DB 综合」，而上游 `/health` 是**纯 liveness**（不触库）；把它改成上游语义
//! 会破坏 `apps/mc-cli` 的探针与 `mc-openapi` / `mc-conformance` 的两个既有测试。
//! ② `local_only` 是**登记项**（`route_parity.py:20` 逐字：*registered here, absent upstream
//! (informational)*）、**不进任何门的分子分母** ⇒ 收敛的收益为 0、代价是牵连 4 处。
//!
//! M10-0（`LUM-2102`）删除了本文件的 `placeholder`：它的**唯一**调用者是 `mount.rs` 里那条
//! 幽灵占位 `GET /api/feature-flags`（上游根本没有这个键），该占位由本片预删
//! （`docs/64` §9.3）⇒ 函数留在原地就是死代码、会让门 ③ `-D warnings` 红。
//! 上面两条路由**逐字不动**。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use mc_db::health::HealthStatus;

use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
    pub db: &'static str,
}

pub async fn health(State(state): State<Arc<AppState>>) -> ApiResult<Json<HealthResponse>> {
    let h = mc_db::health::check(&state.db).await;
    Ok(Json(HealthResponse {
        status: "ok",
        service: "multica-rs",
        version: env!("CARGO_PKG_VERSION"),
        db: match h.status {
            HealthStatus::Healthy => "healthy",
            HealthStatus::Degraded => "degraded",
            HealthStatus::Unhealthy => "unhealthy",
        },
    }))
}

#[derive(Debug, Serialize)]
pub struct DbHealthResponse {
    pub status: String,
    pub latency_ms: u64,
    pub message: Option<String>,
}

pub async fn db_health(State(state): State<Arc<AppState>>) -> ApiResult<Json<DbHealthResponse>> {
    let h = mc_db::health::check(&state.db).await;
    Ok(Json(DbHealthResponse {
        status: format!("{:?}", h.status).to_lowercase(),
        latency_ms: h.latency_ms,
        message: h.message,
    }))
}
