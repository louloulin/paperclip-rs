//! Actor 运行时抽象（基于 kameo）。
//!
//! 设计目标：
//! - 高内聚：所有 actor 运行时原语（spawn、消息、引用）来自 kameo
//! - 低耦合：调用方只依赖本 crate 重导出的 `ActorRef` / 消息 trait
//! - 领域适配：通过 [`DomainMessage`] 让领域消息携带 actor 主体身份
//!
//! 适用场景：
//! - pc-heartbeat：每个 agent run 启动一个 actor 编排生命周期
//! - pc-realtime：live-event bus 由 actor 持有广播状态
//! - pc-plugin-host：每个插件 worker 一个 actor

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use kameo::actor::{Actor, ActorRef, Spawn};
use kameo::error::Infallible;
use kameo::message::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type StopFuture = Pin<Box<dyn Future<Output = Result<(), kameo::error::SendError>> + Send>>;
type StopActor = Arc<dyn Fn() -> StopFuture + Send + Sync>;
type IsAlive = Arc<dyn Fn() -> bool + Send + Sync>;

/// Actor 的稳定业务身份，不暴露 kameo 内部分配的 [`kameo::actor::ActorId`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorKey {
    pub kind: String,
    pub id: String,
}

impl ActorKey {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorRegistryError {
    #[error("actor already registered: {0:?}")]
    AlreadyRegistered(ActorKey),
    #[error("actor type does not match registry key: {0:?}")]
    TypeMismatch(ActorKey),
    #[error("actor shutdown failed: {0}")]
    Shutdown(String),
}

struct RegisteredActor {
    actor_ref: Arc<dyn Any + Send + Sync>,
    is_alive: IsAlive,
    stop: StopActor,
}

/// 进程内 Actor 注册表。
///
/// 业务层使用 [`ActorKey`] 定位 Actor；具体类型通过 `get::<A>` 在边界处校验。
/// 注册表同时保留类型擦除后的停止句柄，使 composition root 可以统一优雅关闭。
#[derive(Clone, Default)]
pub struct ActorRegistry {
    actors: Arc<Mutex<HashMap<ActorKey, RegisteredActor>>>,
}

impl ActorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<A: Actor>(
        &self,
        key: ActorKey,
        actor_ref: ActorRef<A>,
    ) -> Result<(), ActorRegistryError> {
        let mut actors = self.actors.lock().expect("actor registry mutex poisoned");
        if actors.get(&key).is_some_and(|entry| (entry.is_alive)()) {
            return Err(ActorRegistryError::AlreadyRegistered(key));
        }

        let alive_ref = actor_ref.clone();
        let stop_ref = actor_ref.clone();
        actors.insert(
            key,
            RegisteredActor {
                actor_ref: Arc::new(actor_ref),
                is_alive: Arc::new(move || alive_ref.is_alive()),
                stop: Arc::new(move || {
                    let actor_ref = stop_ref.clone();
                    Box::pin(stop_actor(actor_ref))
                }),
            },
        );
        Ok(())
    }

    pub fn get<A: Actor>(&self, key: &ActorKey) -> Result<ActorRef<A>, ActorRegistryError> {
        let actors = self.actors.lock().expect("actor registry mutex poisoned");
        let Some(entry) = actors.get(key) else {
            return Err(ActorRegistryError::TypeMismatch(key.clone()));
        };
        entry
            .actor_ref
            .downcast_ref::<ActorRef<A>>()
            .cloned()
            .ok_or_else(|| ActorRegistryError::TypeMismatch(key.clone()))
    }

    pub fn unregister(&self, key: &ActorKey) -> bool {
        self.actors
            .lock()
            .expect("actor registry mutex poisoned")
            .remove(key)
            .is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.actors
            .lock()
            .expect("actor registry mutex poisoned")
            .is_empty()
    }

    pub async fn shutdown(&self) -> Result<(), ActorRegistryError> {
        let entries = {
            let mut actors = self.actors.lock().expect("actor registry mutex poisoned");
            actors.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };

        for entry in entries {
            (entry.stop)()
                .await
                .map_err(|error| ActorRegistryError::Shutdown(error.to_string()))?;
        }
        Ok(())
    }
}

async fn stop_actor<A: Actor>(actor_ref: ActorRef<A>) -> Result<(), kameo::error::SendError> {
    actor_ref.stop_gracefully().await
}

/// 重导出 kameo 的核心类型，便于上层一致引用。
pub mod kameo_api {
    pub use kameo::actor::{Actor, ActorId, ActorRef, Recipient, ReplyRecipient, WeakActorRef};
    pub use kameo::error::{ActorStopReason, Infallible, SendError};
    pub use kameo::mailbox::{bounded, unbounded, MailboxReceiver, MailboxSender};
    pub use kameo::message::{Context, Message};
    pub use kameo::reply::Reply;
}

/// 领域消息：所有 actor 间消息携带执行主体，便于审计与追踪。
///
/// `from` 标识消息发送方（用户/agent/system），`payload` 为领域数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainMessage<P> {
    pub from: MessageOrigin,
    pub payload: P,
}

/// 消息来源：与 [`crate::Actor`] 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessageOrigin {
    User { id: String },
    Agent { id: Uuid },
    System,
}

impl<P> DomainMessage<P> {
    pub fn system(payload: P) -> Self {
        Self {
            from: MessageOrigin::System,
            payload,
        }
    }

    pub fn from_user(id: impl Into<String>, payload: P) -> Self {
        Self {
            from: MessageOrigin::User { id: id.into() },
            payload,
        }
    }

    pub fn from_agent(id: Uuid, payload: P) -> Self {
        Self {
            from: MessageOrigin::Agent { id },
            payload,
        }
    }
}

/// 系统 actor：最小可用的 actor，演示 kameo 集成。
///
/// 仅持有 `name`；用于基础设施级别的 actor 池（如实时广播）。
///
/// 所有 actor 必须实现 [`kameo::Actor`]：
/// - `Args` 为构造参数类型（常用 `Self`）
/// - `Error` 为 actor 内部错误类型（最简为 `Infallible`）
/// - `on_start` 在 spawn 时调用一次
/// - `on_stop` 在停止时调用一次
#[derive(Debug)]
pub struct SystemActor {
    pub name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl SystemActor {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            started_at: chrono::Utc::now(),
        }
    }
}

impl Actor for SystemActor {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(state: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(state)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: kameo::actor::WeakActorRef<Self>,
        _reason: kameo::error::ActorStopReason,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// 心跳消息示例：演示 actor 间消息传递。
///
/// 通过 `#[derive]` 或手动实现 [`Message`]：
/// - `Reply` 为响应类型
/// - `handle` 在 actor 任务里执行
#[derive(Debug)]
pub struct PingMsg {
    pub payload: String,
}

impl Message<PingMsg> for SystemActor {
    type Reply = String;

    async fn handle(
        &mut self,
        msg: PingMsg,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        format!("{}:ack:{}", self.name, msg.payload)
    }
}

/// 启动一个 [`SystemActor`] 并返回其 [`ActorRef`]。
///
/// 这是上层调用 kameo 的统一入口（避免直接散落 `kameo::spawn`）。
#[must_use]
pub fn spawn_system_actor(name: impl Into<String>) -> ActorRef<SystemActor> {
    SystemActor::spawn(SystemActor::new(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_system_actor_and_ping() {
        let actor_ref = spawn_system_actor("test");
        let reply = actor_ref
            .ask(PingMsg {
                payload: "hello".into(),
            })
            .await
            .unwrap();
        assert_eq!(reply, "test:ack:hello");
        actor_ref.stop_gracefully().await.unwrap();
    }

    #[tokio::test]
    async fn domain_message_constructors() {
        let sys: DomainMessage<&str> = DomainMessage::system("boot");
        assert!(matches!(sys.from, MessageOrigin::System));

        let user: DomainMessage<&str> = DomainMessage::from_user("alice", "hi");
        assert!(matches!(user.from, MessageOrigin::User { .. }));

        let agent: DomainMessage<&str> = DomainMessage::from_agent(Uuid::nil(), "report");
        assert!(matches!(agent.from, MessageOrigin::Agent { .. }));
    }

    #[test]
    fn kameo_api_re_exports_compile() {
        fn assert_actor<T: Actor>() {}
        assert_actor::<SystemActor>();
    }

    #[tokio::test]
    async fn registry_returns_the_same_typed_actor() {
        let registry = ActorRegistry::new();
        let actor_ref = spawn_system_actor("registered");

        registry
            .register(ActorKey::new("system", "primary"), actor_ref.clone())
            .unwrap();

        let resolved = registry
            .get::<SystemActor>(&ActorKey::new("system", "primary"))
            .unwrap();
        assert_eq!(resolved.id(), actor_ref.id());
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn registry_rejects_duplicate_live_actor() {
        let registry = ActorRegistry::new();
        let key = ActorKey::new("heartbeat-run", "run-1");
        let first = spawn_system_actor("first");
        let second = spawn_system_actor("second");

        registry.register(key.clone(), first).unwrap();
        let error = registry.register(key.clone(), second.clone()).unwrap_err();

        assert_eq!(error, ActorRegistryError::AlreadyRegistered(key));
        second.stop_gracefully().await.unwrap();
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn registry_replaces_stopped_actor() {
        let registry = ActorRegistry::new();
        let key = ActorKey::new("heartbeat-run", "run-2");
        let stopped = spawn_system_actor("stopped");
        registry.register(key.clone(), stopped.clone()).unwrap();
        stopped.stop_gracefully().await.unwrap();

        let replacement = spawn_system_actor("replacement");
        registry.register(key.clone(), replacement.clone()).unwrap();

        assert_eq!(
            registry.get::<SystemActor>(&key).unwrap().id(),
            replacement.id()
        );
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn registry_shutdown_stops_and_removes_all_actors() {
        let registry = ActorRegistry::new();
        let first = spawn_system_actor("first");
        let second = spawn_system_actor("second");
        registry
            .register(ActorKey::new("system", "first"), first.clone())
            .unwrap();
        registry
            .register(ActorKey::new("system", "second"), second.clone())
            .unwrap();

        registry.shutdown().await.unwrap();

        assert!(!first.is_alive());
        assert!(!second.is_alive());
        assert!(registry.is_empty());
    }
}
