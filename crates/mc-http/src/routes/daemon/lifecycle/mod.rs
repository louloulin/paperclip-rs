//! daemon 生命周期路由：register / deregister / heartbeat / ws /
//! workspaces / repos / runtime-profiles（R7 拆分自 `daemon.rs`）。
//!
//! 上游落点：`server/internal/handler/daemon.go`（register L405 / deregister L939 /
//! heartbeat L1089 / ws `daemon_ws.go:12`）、`daemon_workspace.go:27`、
//! `runtime_profile.go:640`、`daemon_rpc.go:45`。
//!
//! 文件布局（R7 拆文件，门 ⑩ 的 800 行上限）：
//! - `register.rs`：`POST /api/daemon/register` 与它的投影 / 元数据辅助
//! - `deregister.rs`：`POST /api/daemon/deregister`
//! - `heartbeat.rs`：`POST /api/daemon/heartbeat` 与 ack 的唯一生产者
//! - `workspaces.rs`：`GET /api/daemon/workspaces` 及其 `:workspaceId/repos` /
//!   `:workspaceId/runtime-profiles`
//! - `ws()`：留在本文件（升级入口，`daemon/ws.rs` 只做帧处理）

mod deregister;
mod heartbeat;
mod register;
mod workspaces;

pub(crate) use deregister::deregister;
pub(crate) use heartbeat::{heartbeat, heartbeat_ack, runtime_gone_ack};
pub(crate) use register::register;
pub(crate) use workspaces::{list_workspaces, runtime_profiles, workspace_repos};

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::Arc;

use mc_repos::daemon::DaemonRepo;
use mc_ws::identity::ClientIdentity;

use super::scope::{db_err, not_found, DaemonActor, DaemonAuth};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// GET /api/daemon/ws
// ---------------------------------------------------------------------------

/// upstream `DaemonWebSocket`（`daemon_ws.go:12`）→ `Hub::handle_websocket`。
///
/// 身份在这里构造：daemon token 给出 `daemon_id` + 该机器已登记的全部 runtime id；
/// 用户身份给出 `user_id` + 全部 membership 工作区。上游在 upgrade 时做批量鉴权并把
/// 结果存成连接租约，本地不缓存（`ClientIdentity` 的 D4 说明 + `docs/32` 偏离表）。
///
/// 查询里的 `runtime_id` / `runtime_ids` 收窄按上游 `parseRuntimeIDs` +
/// `buildDaemonWebSocketIdentity` 处理：**不在本连接名下 ⇒ 404 `runtime not found`**，
/// 收窄结果直接写进 `ClientIdentity.runtime_ids`（因此决定 hub 的投递面）。
/// 解析与校验在 [`super::ws::requested_runtime_ids`] / [`super::ws::narrow_runtime_ids`]。
///
/// `runtime_ids` 与 `user_id` 都为空时 `Hub` 会回 400 且**不升级**。
pub(crate) async fn ws(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Query(query): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let repo = DaemonRepo::new(&state.db);
    let requested = super::ws::requested_runtime_ids(&query);
    let identity = match &auth.actor {
        DaemonActor::Daemon {
            workspace_id,
            daemon_id,
        } => match repo.runtime_ids_for_daemon(*workspace_id, daemon_id).await {
            Ok(ids) => {
                let full: Vec<String> = ids.iter().map(ToString::to_string).collect();
                match super::ws::narrow_runtime_ids(&full, &requested) {
                    Ok(runtime_ids) => ClientIdentity {
                        daemon_id: daemon_id.clone(),
                        workspace_id: workspace_id.to_string(),
                        runtime_ids,
                        ..ClientIdentity::default()
                    },
                    Err(err) => return err.into_response(),
                }
            }
            Err(e) => return db_err(e).into_response(),
        },
        DaemonActor::User {
            user_id,
            daemon_id: _,
        } => {
            // 用户连接的 runtime scope 本地**不加载**（`runtime_ids` 只由 `mdt_` token
            // 填），所以非空收窄 fail-closed 回 404 —— 与上游「不在你名下 ⇒ 404」同文案。
            // 上游允许用户连接声明自己可见的 runtime；本地缺这条查询，登记在 `docs/43-M3-7-FU-WS-CLOSE.md`。
            if !requested.is_empty() {
                return not_found("runtime not found").into_response();
            }
            let workspaces = match repo.list_workspaces_for_user(*user_id).await {
                Ok(rows) => rows,
                Err(e) => return db_err(e).into_response(),
            };
            ClientIdentity {
                // 用户连接**不带**机器标识：上游只从 daemon 中间件（`mdt_` token）填
                // `ClientIdentity.DaemonID`（`daemon_ws.go:44`）。dev-mode 的 `X-Daemon-Id`
                // 是 HTTP 腿的偏离 D-1，不能让它在 WS 的 RPC 分发里冒充 daemon 身份。
                daemon_id: String::new(),
                user_id: user_id.to_string(),
                workspace_ids: workspaces
                    .into_iter()
                    .map(|(id, _)| id.to_string())
                    .collect(),
                ..ClientIdentity::default()
            }
        }
    };
    // 注入 WS 的两条 handler（心跳 / RPC）。放这里而不是启动期：`mount_slice_daemon()`
    // 拿不到 `Arc<AppState>`，而 handler 只在有人真的升级 WS 时才被用到；幂等闸在
    // `install_ws_handlers` 里（`OnceLock`）—— 这是它在全仓**唯一**的调用点。
    super::install_ws_handlers(&state);
    state.daemon_hub.handle_websocket(ws, identity)
}
