#![forbid(unsafe_code)]
//! `pc-plugin-state-store` —— plugin scoped state store 高级 facade。
//!
//! 对应 Node `server/src/services/plugin-state-store.ts`（237 行）。
//!
//! 设计目标：1:1 复刻 + Rust 增强
//! - 提供 `PluginStateStore`（typed handle）封装 pc-repos 的 plugin_state_store
//! - 提供 hook bus：让上层（audit / metrics / telemetry）监听 state 变更
//! - 提供 capability check helper：`require_read_capability` / `require_write_capability`
//! - 重新导出 pc-repos 的核心类型 / 常量
//!
//! 与 pc-repos 的区别：
//! - pc-repos 提供 SQL 实现 + 原 Node factory 风格 API
//! - 本 crate 提供 typed trait-based facade + 钩子 + capability helper

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

// ============================================================================
// Re-exports from pc-repos
// ============================================================================

pub use pc_repos::plugin_state_store::{
    plugin_state_store, ListPluginStateFilter, PluginStateRow, PluginStateScopeKind,
    PluginStateStore, PluginStateStoreError, PluginStateStoreResult, ScopeOptions,
    SetPluginStateInput, DEFAULT_NAMESPACE,
};

/// 时间戳类型别名（与 pc-repos 一致）。
pub use pc_repos::plugin_state_store::Timestamp;

// ============================================================================
// Errors
// ============================================================================

/// Plugin state store 服务错误（含 capability 检查失败）。
#[derive(Debug, Error)]
pub enum StateStoreError {
    #[error("missing read capability: {0}")]
    MissingReadCapability(&'static str),
    #[error("missing write capability: {0}")]
    MissingWriteCapability(&'static str),
    #[error("invalid scope kind: {0}")]
    InvalidScopeKind(String),
    #[error(transparent)]
    Store(#[from] PluginStateStoreError),
}

pub type StateStoreResult<T> = Result<T, StateStoreError>;

// ============================================================================
// Hook bus
// ============================================================================

/// Hook 事件 —— 序列化时 camelCase tag。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum StateStoreHookEvent {
    /// Get 调用（包含 plugin_id + scope kind + state_key）。
    GetRequested {
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        scope_id: Option<String>,
        state_key: String,
    },
    /// Get 命中（有 value）。
    GetHit {
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        state_key: String,
    },
    /// Get miss（无 value）。
    GetMiss {
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        state_key: String,
    },
    /// Set 调用。
    SetWritten {
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        scope_id: Option<String>,
        state_key: String,
    },
    /// Delete 调用。
    DeleteRemoved {
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        scope_id: Option<String>,
        state_key: String,
    },
    /// List 调用。
    Listed { plugin_id: Uuid, count: usize },
    /// DeleteAll 调用。
    DeleteAllRemoved { plugin_id: Uuid },
}

/// 扩展点 —— 监听 state 操作。
#[async_trait]
pub trait StateStoreHook: Send + Sync {
    async fn on_state_store_event(&self, event: StateStoreHookEvent);
}

/// Noop 实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopStateStoreHook;

#[async_trait]
impl StateStoreHook for NoopStateStoreHook {
    async fn on_state_store_event(&self, _event: StateStoreHookEvent) {}
}

/// 录制所有事件 —— 测试用。
#[derive(Debug, Default, Clone)]
pub struct RecordingStateStoreHook {
    events: Arc<tokio::sync::Mutex<Vec<StateStoreHookEvent>>>,
}

impl RecordingStateStoreHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.events.try_lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.events.try_lock() {
            g.clear();
        }
    }

    pub async fn events_snapshot_async(&self) -> Vec<StateStoreHookEvent> {
        self.events.lock().await.clone()
    }
}

#[async_trait]
impl StateStoreHook for RecordingStateStoreHook {
    async fn on_state_store_event(&self, event: StateStoreHookEvent) {
        self.events.lock().await.push(event);
    }
}

#[async_trait]
impl StateStoreHook for Arc<RecordingStateStoreHook> {
    async fn on_state_store_event(&self, event: StateStoreHookEvent) {
        (**self).on_state_store_event(event).await;
    }
}

// ============================================================================
// Capability check
// ============================================================================

/// 抽象 capability 来源 —— 上层从 session / auth context 注入。
#[async_trait]
pub trait CapabilityProvider: Send + Sync {
    /// 返回 plugin 是否拥有 `plugin.state.read` capability。
    async fn can_read_state(&self, plugin_id: Uuid) -> bool;
    /// 返回 plugin 是否拥有 `plugin.state.write` capability。
    async fn can_write_state(&self, plugin_id: Uuid) -> bool;
}

/// 总是允许（用于测试 / 内部）。
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllCapabilities;

#[async_trait]
impl CapabilityProvider for AllowAllCapabilities {
    async fn can_read_state(&self, _plugin_id: Uuid) -> bool {
        true
    }
    async fn can_write_state(&self, _plugin_id: Uuid) -> bool {
        true
    }
}

// ============================================================================
// Service
// ============================================================================

/// Service —— 高级 facade，包含 capability check + hook bus + typed 错误。
pub struct PluginStateStoreService {
    db: pc_repos::Db,
    capabilities: Arc<dyn CapabilityProvider>,
    hooks: Vec<Arc<dyn StateStoreHook>>,
}

impl PluginStateStoreService {
    /// 构造（默认 capability + 无 hook）。
    pub fn new(db: pc_repos::Db) -> Self {
        Self {
            db,
            capabilities: Arc::new(AllowAllCapabilities),
            hooks: Vec::new(),
        }
    }

    /// 构造并注入 capability provider + hooks。
    pub fn with_dependencies(
        db: pc_repos::Db,
        capabilities: Arc<dyn CapabilityProvider>,
        hooks: Vec<Arc<dyn StateStoreHook>>,
    ) -> Self {
        Self {
            db,
            capabilities,
            hooks,
        }
    }

    /// Read a state value。
    pub async fn get(
        &self,
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        state_key: &str,
        opts: ScopeOptions,
    ) -> StateStoreResult<Option<Value>> {
        if !self.capabilities.can_read_state(plugin_id).await {
            return Err(StateStoreError::MissingReadCapability(
                "plugin.state.read required",
            ));
        }

        let scope_id_for_event = opts.scope_id.clone();
        self.fan_out(StateStoreHookEvent::GetRequested {
            plugin_id,
            scope_kind,
            scope_id: scope_id_for_event,
            state_key: state_key.to_string(),
        })
        .await;

        let store = plugin_state_store(&self.db);
        let value = store.get(plugin_id, scope_kind, state_key, opts).await?;

        if value.is_some() {
            self.fan_out(StateStoreHookEvent::GetHit {
                plugin_id,
                scope_kind,
                state_key: state_key.to_string(),
            })
            .await;
        } else {
            self.fan_out(StateStoreHookEvent::GetMiss {
                plugin_id,
                scope_kind,
                state_key: state_key.to_string(),
            })
            .await;
        }

        Ok(value)
    }

    /// Write (upsert) a state value。
    pub async fn set(&self, plugin_id: Uuid, input: SetPluginStateInput) -> StateStoreResult<()> {
        if !self.capabilities.can_write_state(plugin_id).await {
            return Err(StateStoreError::MissingWriteCapability(
                "plugin.state.write required",
            ));
        }

        let store = plugin_state_store(&self.db);
        store.set(plugin_id, input.clone()).await?;

        self.fan_out(StateStoreHookEvent::SetWritten {
            plugin_id,
            scope_kind: input.scope_kind,
            scope_id: input.scope_id.clone(),
            state_key: input.state_key,
        })
        .await;

        Ok(())
    }

    /// Delete a state value (idempotent).
    pub async fn delete(
        &self,
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        state_key: &str,
        opts: ScopeOptions,
    ) -> StateStoreResult<()> {
        if !self.capabilities.can_write_state(plugin_id).await {
            return Err(StateStoreError::MissingWriteCapability(
                "plugin.state.write required",
            ));
        }

        let store = plugin_state_store(&self.db);
        store
            .delete(plugin_id, scope_kind, state_key, opts.clone())
            .await?;

        self.fan_out(StateStoreHookEvent::DeleteRemoved {
            plugin_id,
            scope_kind,
            scope_id: opts.scope_id,
            state_key: state_key.to_string(),
        })
        .await;

        Ok(())
    }

    /// List all state entries for a plugin.
    pub async fn list(
        &self,
        plugin_id: Uuid,
        filter: ListPluginStateFilter,
    ) -> StateStoreResult<Vec<PluginStateRow>> {
        if !self.capabilities.can_read_state(plugin_id).await {
            return Err(StateStoreError::MissingReadCapability(
                "plugin.state.read required",
            ));
        }

        let store = plugin_state_store(&self.db);
        let rows = store.list(plugin_id, filter).await?;
        let count = rows.len();

        self.fan_out(StateStoreHookEvent::Listed { plugin_id, count })
            .await;
        Ok(rows)
    }

    /// Delete all state entries owned by a plugin.
    pub async fn delete_all(&self, plugin_id: Uuid) -> StateStoreResult<()> {
        if !self.capabilities.can_write_state(plugin_id).await {
            return Err(StateStoreError::MissingWriteCapability(
                "plugin.state.write required",
            ));
        }

        let store = plugin_state_store(&self.db);
        store.delete_all(plugin_id).await?;

        self.fan_out(StateStoreHookEvent::DeleteAllRemoved { plugin_id })
            .await;
        Ok(())
    }

    async fn fan_out(&self, event: StateStoreHookEvent) {
        for h in &self.hooks {
            h.on_state_store_event(event.clone()).await;
        }
    }
}

// ============================================================================
// Capability check helpers
// ============================================================================

/// Read capability 检查（公开 helper，便于上层直接调用）。
pub async fn require_read_capability(
    caps: &dyn CapabilityProvider,
    plugin_id: Uuid,
) -> StateStoreResult<()> {
    if caps.can_read_state(plugin_id).await {
        Ok(())
    } else {
        Err(StateStoreError::MissingReadCapability(
            "plugin.state.read required",
        ))
    }
}

/// Write capability 检查（公开 helper）。
pub async fn require_write_capability(
    caps: &dyn CapabilityProvider,
    plugin_id: Uuid,
) -> StateStoreResult<()> {
    if caps.can_write_state(plugin_id).await {
        Ok(())
    } else {
        Err(StateStoreError::MissingWriteCapability(
            "plugin.state.write required",
        ))
    }
}

// ============================================================================
// Scope kind helpers
// ============================================================================

/// 从字符串解析 scope kind（无效返回错误）。
pub fn parse_scope_kind(s: &str) -> StateStoreResult<PluginStateScopeKind> {
    PluginStateScopeKind::parse(s).ok_or_else(|| StateStoreError::InvalidScopeKind(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r710_default_namespace_constant() {
        assert_eq!(DEFAULT_NAMESPACE, "default");
    }

    #[test]
    fn r710_scope_kind_round_trip() {
        for k in [
            PluginStateScopeKind::Instance,
            PluginStateScopeKind::Company,
            PluginStateScopeKind::Project,
            PluginStateScopeKind::Issue,
            PluginStateScopeKind::Agent,
        ] {
            assert_eq!(parse_scope_kind(k.as_str()).unwrap(), k);
        }
        assert!(parse_scope_kind("unknown").is_err());
    }

    #[test]
    fn r710_state_store_hook_event_tag_is_camel_case() {
        let v = serde_json::to_value(StateStoreHookEvent::DeleteAllRemoved {
            plugin_id: Uuid::nil(),
        })
        .unwrap();
        assert_eq!(v["type"], "deleteAllRemoved");
    }

    #[test]
    fn r710_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoopStateStoreHook>();
        assert_send_sync::<RecordingStateStoreHook>();
        assert_send_sync::<StateStoreHookEvent>();
        assert_send_sync::<AllowAllCapabilities>();
    }

    #[tokio::test]
    async fn r710_allow_all_capabilities() {
        let caps = AllowAllCapabilities;
        assert!(caps.can_read_state(Uuid::new_v4()).await);
        assert!(caps.can_write_state(Uuid::new_v4()).await);
    }

    #[tokio::test]
    async fn r710_recording_hook_captures_event() {
        let h = RecordingStateStoreHook::default();
        h.on_state_store_event(StateStoreHookEvent::GetHit {
            plugin_id: Uuid::new_v4(),
            scope_kind: PluginStateScopeKind::Instance,
            state_key: "k".into(),
        })
        .await;
        assert_eq!(h.len(), 1);
        h.clear();
        assert!(h.is_empty());
    }

    #[tokio::test]
    async fn r710_require_read_capability_helper() {
        let caps = AllowAllCapabilities;
        assert!(require_read_capability(&caps, Uuid::new_v4()).await.is_ok());

        struct Deny;
        #[async_trait]
        impl CapabilityProvider for Deny {
            async fn can_read_state(&self, _: Uuid) -> bool {
                false
            }
            async fn can_write_state(&self, _: Uuid) -> bool {
                false
            }
        }
        assert!(require_read_capability(&Deny, Uuid::new_v4())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn r710_require_write_capability_helper() {
        struct Deny;
        #[async_trait]
        impl CapabilityProvider for Deny {
            async fn can_read_state(&self, _: Uuid) -> bool {
                false
            }
            async fn can_write_state(&self, _: Uuid) -> bool {
                false
            }
        }
        assert!(require_write_capability(&Deny, Uuid::new_v4())
            .await
            .is_err());
    }
}
