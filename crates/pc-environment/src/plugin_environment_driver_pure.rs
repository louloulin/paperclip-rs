// SPDX-License-Identifier: MIT
//
// R679 parity: `plugin-environment-driver.ts` pure functions (1:1 to Node).
//
// Reference (Node):
//   paperclip/server/src/services/plugin-environment-driver.ts
//
// Only the **pure** parts of the file are mirrored here. The async DB /
// plugin worker functions (17 of them) are intentionally deferred until
// the lower-level `Db` and `PluginWorkerManager` abstractions can be
// expressed as Rust traits and reused across `pc-*` crates.

use serde::{Deserialize, Serialize};

/// Mirrors Node `PluginEnvironmentConfig` minimal subset consumed by the
/// pure functions in this module. The full `PluginEnvironmentConfig` type
/// already exists in `config.rs`; we only need `pluginKey` and
/// `driverKey` for `plugin_driver_provider_key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEnvironmentDriverKey {
    pub plugin_key: String,
    pub driver_key: String,
}

/// Mirrors Node constant `RPC_OVERHEAD_BUFFER_MS = 30_000`.
pub const RPC_OVERHEAD_BUFFER_MS: u64 = 30_000;

/// Mirrors Node constant `DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS = 2_000`.
pub const DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS: u64 = 2_000;

/// 1:1 parity with Node `pluginDriverProviderKey`.
/// Returns `"${pluginKey}:${driverKey}"`.
#[inline]
pub fn plugin_driver_provider_key(config: &PluginEnvironmentDriverKey) -> String {
    format!("{}:{}", config.plugin_key, config.driver_key)
}

/// 1:1 parity with Node `resolvePluginExecuteRpcTimeoutMs`.
///
/// Precedence rules (mirrored exactly from Node):
///   1. If `requested_timeout_ms` is a finite number > 0, use `trunc(requested_timeout_ms)`.
///   2. Else if `config["timeoutMs"]` is a number > 0, use `trunc(config["timeoutMs"])`.
///   3. Else `baseMs` is `None`.
/// Returns `Some(baseMs + RPC_OVERHEAD_BUFFER_MS)` when a base was resolved,
/// otherwise `None`.
pub fn resolve_plugin_execute_rpc_timeout_ms(
    requested_timeout_ms: Option<f64>,
    config: &serde_json::Value,
) -> Option<u64> {
    let mut base_ms: Option<u64> = None;

    if let Some(req) = requested_timeout_ms {
        if req.is_finite() && req > 0.0 {
            base_ms = Some(req.trunc() as u64);
        }
    }

    if base_ms.is_none() {
        if let Some(n) = config.get("timeoutMs") {
            if let Some(f) = n.as_f64() {
                if f.is_finite() && f > 0.0 {
                    base_ms = Some(f.trunc() as u64);
                }
            } else if let Some(i) = n.as_i64() {
                if i > 0 {
                    base_ms = Some(i as u64);
                }
            } else if let Some(u) = n.as_u64() {
                if u > 0 {
                    base_ms = Some(u);
                }
            }
        }
    }

    base_ms.map(|n| n.saturating_add(RPC_OVERHEAD_BUFFER_MS))
}
