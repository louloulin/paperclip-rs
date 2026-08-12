//! Terminal session store + auth 抽象。
//!
//! R629：定义终端会话查询与 host key pin 验证的 trait 接口。
//! 真实实现（`pc-repos::environment_terminal_session_store`）通过依赖注入提供。
//!
//! 与 Node 上游 `environmentCustomImageTerminalSessionStore` 1:1 对齐：
//! - `get_session(setup_session_id, terminal_session_id)` → session（含 expiresAt + ssh payload + role）
//! - `verify_or_pin_host_key(terminal_session_id, host_key_sha256)` → bool
//!
//! 设计：
//! - 纯 trait，无 IO 直接耦合（DB 在真实实现里，测试用 InMemoryStore）
//! - error 统一 `String`（与 Node 风格一致，便于 handler 直接 surface 给 WS error frame）

use chrono::{DateTime, Utc};

/// 单条 terminal session 的不透明记录。
///
/// 字段是 mintype 镜像 Node 端 `EnvironmentCustomImageTerminalSessionRecord`：
///   - `id` — terminal session id
///   - `setup_session_id` — 父 setup session id
///   - `expires_at` — 会话过期时间（absolute UTC）
///   - `ssh_host/port/username` — SSH 连接参数
///   - `verify_host_key_sha256` — caller 注入到 connector 的 host key 验证 callback
///
/// 字段命名刻意保留 snake_case：DB 层是 source of truth，这里是 DB → connector 的 DTO 边界。
#[derive(Debug, Clone)]
pub struct TerminalSessionRecord {
    pub id: String,
    pub setup_session_id: String,
    pub expires_at: DateTime<Utc>,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_username: String,
}

/// Host key verify 决策（与 Node `verifyOrPinHostKey` 一致：返回 true 即接受）。
pub type HostKeyVerifier = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Terminal session store 接口（mockable）。
///
/// 真实实现走 `pc-repos` 的 DB layer；
/// 测试用 `InMemoryStore` 直接喂数据。
#[async_trait::async_trait]
pub trait TerminalSessionStore: Send + Sync {
    /// 根据 setup_session_id + terminal_session_id 查 session。
    async fn get_session(
        &self,
        setup_session_id: &str,
        terminal_session_id: &str,
    ) -> Result<Option<TerminalSessionRecord>, String>;

    /// 验证或 pin 一个 host key。
    /// 接受 → 写入新 pin 或复用现有；拒绝 → 返回 false（host key mismatch）。
    async fn verify_or_pin_host_key(
        &self,
        terminal_session_id: &str,
        host_key_sha256: &str,
    ) -> Result<bool, String>;
}

// ============================================================================
// InMemoryStore（单测用）
// ============================================================================

use std::collections::HashMap;
use std::sync::Mutex;

pub struct InMemoryStore {
    inner: Mutex<InMemoryStoreInner>,
}

struct InMemoryStoreInner {
    /// key = (setup_session_id, terminal_session_id) → record
    sessions: HashMap<(String, String), TerminalSessionRecord>,
    /// key = terminal_session_id → set of pinned host key sha256
    pins: HashMap<String, Vec<String>>,
    /// test hook：若 Some(cb)，每次 verify_or_pin_host_key 调 cb（不修改默认行为）
    pin_hook: Option<Box<dyn Fn(&str, &str) -> bool + Send + Sync>>,
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(InMemoryStoreInner {
                sessions: HashMap::new(),
                pins: HashMap::new(),
                pin_hook: None,
            }),
        }
    }

    pub fn insert(&self, record: TerminalSessionRecord) {
        let mut g = self.inner.lock().expect("poisoned");
        g.sessions
            .insert((record.setup_session_id.clone(), record.id.clone()), record);
    }

    /// 设置 verify_or_pin_host_key 的 test hook：返回 cb(terminal_session_id, host_key_sha256) → bool
    pub fn set_pin_hook<F>(&self, cb: F)
    where
        F: Fn(&str, &str) -> bool + Send + Sync + 'static,
    {
        let mut g = self.inner.lock().expect("poisoned");
        g.pin_hook = Some(Box::new(cb));
    }
}

#[async_trait::async_trait]
impl TerminalSessionStore for InMemoryStore {
    async fn get_session(
        &self,
        setup_session_id: &str,
        terminal_session_id: &str,
    ) -> Result<Option<TerminalSessionRecord>, String> {
        let g = self.inner.lock().expect("poisoned");
        Ok(g.sessions
            .get(&(setup_session_id.into(), terminal_session_id.into()))
            .cloned())
    }

    async fn verify_or_pin_host_key(
        &self,
        terminal_session_id: &str,
        host_key_sha256: &str,
    ) -> Result<bool, String> {
        let mut g = self.inner.lock().expect("poisoned");
        if let Some(hook) = &g.pin_hook {
            return Ok(hook(terminal_session_id, host_key_sha256));
        }
        let pins = g
            .pins
            .entry(terminal_session_id.into())
            .or_insert_with(Vec::new);
        if pins.is_empty() {
            pins.push(host_key_sha256.into());
            Ok(true)
        } else {
            Ok(pins.iter().any(|p| p == host_key_sha256))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, setup_id: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            id: id.into(),
            setup_session_id: setup_id.into(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
            ssh_username: "root".into(),
        }
    }

    #[tokio::test]
    async fn in_memory_store_get_returns_inserted() {
        let store = InMemoryStore::new();
        store.insert(record("t-1", "s-1"));
        let got = store.get_session("s-1", "t-1").await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().id, "t-1");
    }

    #[tokio::test]
    async fn in_memory_store_get_missing_returns_none() {
        let store = InMemoryStore::new();
        let got = store.get_session("s-1", "t-1").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn in_memory_store_pin_then_verify_same() {
        let store = InMemoryStore::new();
        // 首次：pin
        let ok = store.verify_or_pin_host_key("t-1", "hk-abc").await.unwrap();
        assert!(ok);
        // 二次：相同 hk → 接受
        let ok = store.verify_or_pin_host_key("t-1", "hk-abc").await.unwrap();
        assert!(ok);
        // 二次：不同 hk → 拒绝
        let ok = store.verify_or_pin_host_key("t-1", "hk-XYZ").await.unwrap();
        assert!(!ok);
    }

    #[tokio::test]
    async fn in_memory_store_pin_hook_overrides_default() {
        let store = InMemoryStore::new();
        store.set_pin_hook(|_id, _hk| false);
        let ok = store.verify_or_pin_host_key("t-1", "hk-abc").await.unwrap();
        assert!(!ok);
    }
}
