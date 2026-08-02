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

use kameo::actor::{Actor, ActorRef, Spawn};
use kameo::error::Infallible;
use kameo::message::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use kameo::mailbox::Mailbox;
pub use kameo::request::SendError;

/// 重导出 kameo 的核心类型，便于上层一致引用。
pub mod kameo_api {
    pub use kameo::actor::{Actor, ActorId, ActorRef, Recipient, ReplyRecipient, WeakActorRef};
    pub use kameo::error::{ActorStopReason, Infallible};
    pub use kameo::mailbox::Mailbox;
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
        Self { from: MessageOrigin::System, payload }
    }

    pub fn from_user(id: impl Into<String>, payload: P) -> Self {
        Self { from: MessageOrigin::User { id: id.into() }, payload }
    }

    pub fn from_agent(id: Uuid, payload: P) -> Self {
        Self { from: MessageOrigin::Agent { id }, payload }
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
        Self { name: name.into(), started_at: chrono::Utc::now() }
    }
}

impl Actor for SystemActor {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(state: Self::Actor::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        tracing::info!(actor = %state.name, "system actor started");
        Ok(state)
    }

    async fn on_stop(&mut self, _actor_ref: kameo::actor::WeakActorRef<Self>, _reason: kameo::error::ActorStopReason) -> Result<(), Self::Error> {
        tracing::info!(actor = %self.name, "system actor stopped");
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

    async fn handle(&mut self, msg: PingMsg, _ctx: kameo::message::Context<Self, Self::Reply>) -> Self::Reply {
        tracing::debug!(actor = %self.name, payload = %msg.payload, "ping received");
        format!("{}:ack:{}", self.name, msg.payload)
    }
}

/// 启动一个 SystemActor 并返回其 ActorRef。
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
        let reply = actor_ref.ask(PingMsg { payload: "hello".into() }).await.unwrap();
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
        // 验证重导出的关键 trait 在作用域内可见
        fn _check<A: Actor>() {
            fn _assert_send<T: Send>(_: T) {}
            // Actor 的约束会保证 Send
        }
    }
}
