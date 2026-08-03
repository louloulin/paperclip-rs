//! Worker 池：管理所有 plugin worker 进程的生命周期。
//!
//! 与原 `server/src/services/plugin-worker-manager.ts` 等价：
//! - 维护 `{plugin_id -> WorkerHandle}` 映射
//! - 支持动态启停 worker
//! - graceful shutdown 所有 worker

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};
use uuid::Uuid;

use crate::handle::{WorkerHandle, WorkerOptions};

#[derive(Clone, Default)]
pub struct WorkerPool {
    workers: Arc<RwLock<HashMap<Uuid, Arc<WorkerHandle>>>>,
}

impl WorkerPool {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动并注册一个 worker。
    pub async fn spawn(&self, options: WorkerOptions) -> Result<Arc<WorkerHandle>, String> {
        let handle = Arc::new(WorkerHandle::new(options.clone()));
        handle.start().await?;
        let plugin_id = options.plugin_id;
        self.workers.write().await.insert(plugin_id, handle.clone());
        Ok(handle)
    }

    /// 直接注册一个已启动的 worker（用于测试）。
    pub async fn register(&self, handle: Arc<WorkerHandle>) {
        self.workers.write().await.insert(handle.plugin_id, handle);
    }

    pub async fn get(&self, plugin_id: &Uuid) -> Option<Arc<WorkerHandle>> {
        self.workers.read().await.get(plugin_id).cloned()
    }

    pub async fn remove(&self, plugin_id: &Uuid) -> Option<Arc<WorkerHandle>> {
        self.workers.write().await.remove(plugin_id)
    }

    /// 优雅关闭并移除一个 worker。
    pub async fn shutdown_one(&self, plugin_id: &Uuid) -> Result<(), String> {
        let handle = self.remove(plugin_id).await;
        if let Some(handle) = handle {
            handle.shutdown().await?;
            info!(plugin_id = %plugin_id, "plugin worker shut down");
        }
        Ok(())
    }

    /// 关闭所有 worker（用于 server shutdown）。
    pub async fn shutdown_all(&self) {
        let handles: Vec<_> = {
            let mut workers = self.workers.write().await;
            workers.drain().map(|(_, h)| h).collect()
        };
        for handle in handles {
            if let Err(e) = handle.shutdown().await {
                error!(plugin_id = %handle.plugin_id, "shutdown failed: {e}");
            }
        }
    }

    #[must_use]
    pub async fn len(&self) -> usize {
        self.workers.read().await.len()
    }

    #[must_use]
    pub async fn is_empty(&self) -> bool {
        self.workers.read().await.is_empty()
    }

    #[must_use]
    pub async fn active_ids(&self) -> Vec<Uuid> {
        self.workers.read().await.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_options() -> WorkerOptions {
        WorkerOptions {
            plugin_id: Uuid::new_v4(),
            command: "/bin/echo".into(),
            args: vec!["hi".into()],
            cwd: None,
            env: vec![],
            plugin_version: "1.0.0".into(),
            manifest_version: "v1".into(),
            instance_id: Uuid::new_v4(),
            init_timeout: std::time::Duration::from_secs(2),
        }
    }

    #[tokio::test]
    async fn empty_pool() {
        let pool = WorkerPool::new();
        assert!(pool.is_empty().await);
        assert_eq!(pool.len().await, 0);
    }

    #[tokio::test]
    async fn spawn_echo_fails_initialize() {
        let pool = WorkerPool::new();
        let opts = test_options();
        let id = opts.plugin_id;
        let result = pool.spawn(opts).await;
        assert!(result.is_err());
        // failed start should not register
        assert!(pool.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn register_existing_handle() {
        let pool = WorkerPool::new();
        let handle = Arc::new(WorkerHandle::new(test_options()));
        let id = handle.plugin_id;
        pool.register(handle).await;
        assert_eq!(pool.len().await, 1);
        assert!(pool.get(&id).await.is_some());
    }

    #[tokio::test]
    async fn remove_returns_handle() {
        let pool = WorkerPool::new();
        let handle = Arc::new(WorkerHandle::new(test_options()));
        let id = handle.plugin_id;
        pool.register(handle).await;
        let removed = pool.remove(&id).await;
        assert!(removed.is_some());
        assert!(pool.is_empty().await);
    }

    #[tokio::test]
    async fn shutdown_all_is_idempotent_on_empty_pool() {
        let pool = WorkerPool::new();
        // should not panic
        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn shutdown_one_unknown_plugin_is_ok() {
        let pool = WorkerPool::new();
        let result = pool.shutdown_one(&Uuid::new_v4()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn active_ids_returns_all_keys() {
        let pool = WorkerPool::new();
        let h1 = Arc::new(WorkerHandle::new(test_options()));
        let h2 = Arc::new(WorkerHandle::new(test_options()));
        let id1 = h1.plugin_id;
        let id2 = h2.plugin_id;
        pool.register(h1).await;
        pool.register(h2).await;
        let mut ids = pool.active_ids().await;
        ids.sort();
        let mut expected = vec![id1, id2];
        expected.sort();
        assert_eq!(ids, expected);
        // Silence dead_code lint on PathBuf usage in this test module
        let _ = PathBuf::from("/tmp");
    }
}
