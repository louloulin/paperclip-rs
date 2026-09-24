//! 注册表：`kind → Factory`（上游 `server/internal/integrations/channel/registry.go`）。
//!
//! - **写者**：M7-0 建（本 anchor）；**M7-1** 补"last-writer-wins / `ErrUnknownType` /
//!   `types()` 字典序"三条用例。
//!
//! # 三条语义（逐字对齐上游，别"改好一点"）
//!
//! 1. **last-writer-wins**：注册一个已有工厂的 kind 会**静默替换**它。这是**故意**的 ——
//!    部署方可以在内置 adapter 之后注册自己的实现来覆盖它，**不需要**先注销。
//! 2. **空 kind / `None` 工厂被忽略**：注册它们只会把失败推到 `build` 期，所以直接不记。
//! 3. **`build` 无工厂 ⇒ [`ChannelError::UnknownType`]**（带 kind 名）；有工厂则原样返回
//!    工厂的结果（工厂应当返回 `Err` 而不是半成品）。
//!
//! ⚠️ **anchor 期为什么已经实现了这三条**：`apps/mc-server/src/channels.rs` 在**进程启动
//! 路径**上构造本表并调 `register`，一个 `todo!()` 会让服务器起不来（`docs/32` §10 的
//! 归位判断）。这是纯数据结构，不含任何平台分支；`Channel` trait 的**实现**仍全部归 M7-1。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use mc_core::channel::ChannelKind;

use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};

/// `kind → Factory` 的注册表（并发安全：上游注释"构造后可并发使用"）。
#[derive(Default)]
pub struct Registry {
    factories: RwLock<HashMap<ChannelKind, Factory>>,
}

impl std::fmt::Debug for Registry {
    /// 工厂是 `Arc<dyn Fn…>`（不可打印）；只打印已注册的 kind 列表。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Registry")
            .field("kinds", &self.kinds())
            .finish()
    }
}

impl Registry {
    /// 空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 绑定 `kind` 的工厂（last-writer-wins，见模块文档第 1 条）。
    pub fn register(&self, kind: ChannelKind, factory: Factory) {
        let mut factories = self
            .factories
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        factories.insert(kind, factory);
    }

    /// 查工厂。
    pub fn lookup(&self, kind: ChannelKind) -> Option<Factory> {
        let factories = self
            .factories
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        factories.get(&kind).cloned()
    }

    /// 用注册的工厂实例化一条 Channel；没有工厂 ⇒ [`ChannelError::UnknownType`]。
    pub fn build(&self, config: ChannelConfig) -> ChannelResult<Arc<dyn Channel>> {
        let kind = config.kind;
        let factory = self.lookup(kind).ok_or_else(|| ChannelError::UnknownType {
            kind: kind.to_string(),
        })?;
        factory(config)
    }

    /// 已注册的 kind（**字典序**：map 迭代序不稳定，诊断与测试都要确定性）。
    pub fn kinds(&self) -> Vec<ChannelKind> {
        let factories = self
            .factories
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut kinds: Vec<ChannelKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }

    /// 是否一个工厂都没注册（`apps/mc-server` 的"未配置 ⇒ 不装配"判据用它）。
    pub fn is_empty(&self) -> bool {
        self.factories
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;
    use async_trait::async_trait;
    use mc_core::channel::message::{OutboundMessage, SendResult};

    /// 只用来验证注册语义的最小 Channel（M7-1 的 adapter 会取代它）。
    struct Stub {
        kind: ChannelKind,
        caps: Capability,
    }

    #[async_trait]
    impl Channel for Stub {
        fn kind(&self) -> ChannelKind {
            self.kind
        }
        async fn connect(&self) -> ChannelResult<()> {
            Ok(())
        }
        async fn disconnect(&self) -> ChannelResult<()> {
            Ok(())
        }
        async fn send(&self, _out: OutboundMessage) -> ChannelResult<SendResult> {
            Ok(SendResult::single("stub-message-id"))
        }
        fn capabilities(&self) -> Capability {
            self.caps
        }
    }

    fn factory(kind: ChannelKind, caps: Capability) -> Factory {
        Arc::new(move |_config| Ok(Arc::new(Stub { kind, caps }) as Arc<dyn Channel>))
    }

    fn config(kind: ChannelKind) -> ChannelConfig {
        ChannelConfig {
            kind,
            raw: serde_json::json!({}),
            installation_id: None,
            handler: None,
        }
    }

    /// 未注册 ⇒ `UnknownType`（带 kind 名）；注册后能 build 出正确 kind 的实例。
    #[test]
    fn build_without_factory_is_unknown_type() {
        let registry = Registry::new();
        assert!(registry.is_empty());
        // `unwrap_err` 不可用：`Ok` 侧是 `Arc<dyn Channel>`（无 `Debug`）。
        let Err(error) = registry.build(config(ChannelKind::Slack)) else {
            panic!("未注册的 kind 必须失败");
        };
        assert_eq!(
            error,
            ChannelError::UnknownType {
                kind: "slack".into()
            }
        );
        assert_eq!(error.code(), "channel_unknown_type");

        registry.register(
            ChannelKind::Slack,
            factory(ChannelKind::Slack, Capability::TEXT),
        );
        assert!(!registry.is_empty());
        let channel = registry.build(config(ChannelKind::Slack)).unwrap();
        assert_eq!(channel.kind(), ChannelKind::Slack);
        assert_eq!(channel.capabilities(), Capability::TEXT);
    }

    /// last-writer-wins：后注册的工厂**静默替换**先前的（这是部署覆盖内置实现的机制）。
    #[test]
    fn registration_is_last_writer_wins() {
        let registry = Registry::new();
        registry.register(
            ChannelKind::Lark,
            factory(ChannelKind::Lark, Capability::TEXT),
        );
        registry.register(
            ChannelKind::Lark,
            factory(ChannelKind::Lark, Capability::RICH_CARD),
        );
        let channel = registry.build(config(ChannelKind::Lark)).unwrap();
        assert_eq!(channel.capabilities(), Capability::RICH_CARD, "后者获胜");
        assert_eq!(registry.kinds(), vec![ChannelKind::Lark]);
    }

    /// `kinds()` 是字典序（map 迭代序不稳定 ⇒ 诊断输出必须确定）。
    #[test]
    fn kinds_are_sorted() {
        let registry = Registry::new();
        for kind in [
            ChannelKind::WeCom,
            ChannelKind::Slack,
            ChannelKind::Lark,
            ChannelKind::DingTalk,
            ChannelKind::Telegram,
        ] {
            registry.register(kind, factory(kind, Capability::TEXT));
        }
        let names: Vec<&str> = registry.kinds().iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            vec!["dingtalk", "lark", "slack", "telegram", "wecom"]
        );
    }

    /// 工厂自己的错误原样穿出（不被注册表改写）。
    #[test]
    fn factory_errors_pass_through() {
        let registry = Registry::new();
        registry.register(
            ChannelKind::Lark,
            Arc::new(|_config| {
                Err(ChannelError::InvalidConfig {
                    kind: "lark".into(),
                    reason: "missing app_id".into(),
                })
            }),
        );
        let Err(error) = registry.build(config(ChannelKind::Lark)) else {
            panic!("工厂返回 Err 时 build 必须失败");
        };
        assert_eq!(error.code(), "channel_invalid_config");
        assert_eq!(
            error.to_string(),
            "channel: invalid configuration for lark: missing app_id"
        );
    }
}
