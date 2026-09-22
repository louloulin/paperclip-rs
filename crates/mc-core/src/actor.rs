//! Actor 注册表与 key。
//!
//! 提供 `ActorRegistry`：用 key 索引 typed actor 引用。
//! 简化设计：类型擦除后 actor 仅暴露 `as_any` 与 `kill_box`；调用方在各自
//! module 里实现 `AnyActor` trait。

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Actor 的命名空间 + 名称。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorKey {
    pub namespace: String,
    pub name: String,
}

impl ActorKey {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
}

/// 类型擦除的 actor 引用。
pub trait AnyActor: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn kill_box(&self) -> anyhow::Result<()>;
}

/// Actor 注册表（线程安全）。
#[derive(Default, Clone)]
pub struct ActorRegistry {
    inner: Arc<RwLock<HashMap<ActorKey, Arc<dyn AnyActor>>>>,
}

impl ActorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<A>(&self, key: ActorKey, actor: Arc<A>) -> anyhow::Result<()>
    where
        A: AnyActor + 'static,
    {
        let mut guard = self.inner.write().unwrap();
        let erased: Arc<dyn AnyActor> = actor;
        guard.insert(key, erased);
        Ok(())
    }

    pub fn get(&self, key: &ActorKey) -> Option<Arc<dyn AnyActor>> {
        self.inner.read().unwrap().get(key).cloned()
    }

    pub fn list(&self) -> Vec<ActorKey> {
        self.inner.read().unwrap().keys().cloned().collect()
    }

    /// 同步关闭所有已注册 actor（`kill_box` 本身同步，无需 `async`）。
    pub fn shutdown(&self) -> anyhow::Result<()> {
        let actors: Vec<Arc<dyn AnyActor>> = self.inner.read().unwrap().values().cloned().collect();
        for actor in actors {
            let _ = actor.kill_box();
        }
        Ok(())
    }
}

/// 启动一个 system actor，作为根 actor。
pub fn spawn_system_actor(name: &str) -> Arc<SystemActor> {
    Arc::new(SystemActor::new(name))
}

/// 简单系统 actor stub：仅用于保活 / heartbeat 注册。
#[derive(Debug)]
pub struct SystemActor {
    pub name: String,
}

impl SystemActor {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl AnyActor for SystemActor {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kill_box(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_uniqueness() {
        let a = ActorKey::new("system", "root");
        let b = ActorKey::new("system", "root");
        assert_eq!(a, b);
        let c = ActorKey::new("system", "heartbeat");
        assert_ne!(a, c);
    }

    #[test]
    fn registry_insert_and_get() {
        let reg = ActorRegistry::new();
        let sys = spawn_system_actor("root");
        reg.register(ActorKey::new("system", "root"), sys.clone())
            .unwrap();
        assert_eq!(reg.list().len(), 1);
        let got = reg.get(&ActorKey::new("system", "root"));
        assert!(got.is_some());
    }

    #[test]
    fn registry_shutdown_is_noop_for_stub() {
        let reg = ActorRegistry::new();
        let sys = spawn_system_actor("root");
        reg.register(ActorKey::new("system", "root"), sys).unwrap();
        // Should not panic.
        reg.shutdown().unwrap();
    }
}
