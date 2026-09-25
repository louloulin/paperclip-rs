//! Slack adapter（上游 `internal/integrations/slack`（32 文件 / 16 非测试 / 4,490 上游行））。
//!
//! **状态：M7-3 落「入站回路」（`LUM-1768`）；出站面（Block Kit / 回复投递 / 命令历史 / 安装与
//! 绑定）归 M7-4**（`docs/60-M7-PLAN.md` §3.3 的写集表：本目录下每个子文件都有**一个**写者）。
//!
//! # 这个平台的面
//!
//! - `socket_mode`：**每个安装一条**连接（BYO 模型下每个安装带自己的 `xapp-` app token），
//!   接收循环在 [`inbound::SlackChannel::connect`] 里阻塞跑；
//! - BYO 安装（4 条 workspace 路由 + `/api/slack/binding/redeem`）—— **M7-4**；
//! - Block Kit 出站与回复投递（`chat.postMessage` / Markdown→`mrkdwn` / 线程化）—— **M7-4**。
//!
//! # 本目录的写者表（M7-3 / M7-4）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `config.rs` | **M7-3** | `config.go`（安装配置 + 凭据解密） |
//! | `inbound.rs` + `inbound/tests.rs` | **M7-3** | `inbound.go`（事件归一化 + 帧 → 信封） |
//! | `socket.rs` + `socket/tests.rs` | **M7-3** | `slack_channel.go`（Socket Mode 接收循环 + 工厂） |
//! | `media.rs` + `media/tests.rs` | **M7-3** | `media_ingest.go`（下载 / 上传 / 意图账本） |
//! | `mrkdwn.rs` | **M7-3** | `mrkdwn.go`（标准 Markdown → Slack `mrkdwn`） |
//! | `resolvers.rs` + `resolvers/tests.rs` | **M7-3** | `resolvers.go`（安装 / 身份 / 去重 / 会话 / 审计） |
//! | `outbound.rs` `replier.rs` `typing.rs` `history.rs` `slash.rs` `install.rs` `binding.rs` | M7-4 | 其余 11 个上游文件 |
//!
//! ## 写集勘误（**逐条登记**，照 M7-2 先例：`engine/{commands,session}/tests.rs`）
//!
//! `docs/60` §3.3 给 M7-3 的五格是 `slack/{inbound,resolvers,media,mrkdwn,config}.rs`。本片
//! **追加**的路径只有四个，理由都是**门 ⑩ 的 800 行硬限**（不是拆凑数字）：
//!
//! - `slack/socket.rs`：`inbound.rs` 的代码面（不含用例）就已经 ~940 行 ⇒ 按"归一化 / 传输"
//!   拆开，切点正好是上游 `inbound.go` 与 `slack_channel.go` 的边界；
//! - `slack/{inbound,media,resolvers,socket}/tests.rs`：四个文件的用例内联后都会越 800 行
//!   （`media` 1242 / `resolvers` 1121 / `inbound` 1516 / `socket` 835）⇒ 用例外置为子模块。
//!
//! 拆完每个文件都 ≤ 800 行，且**未动** `scripts/file_size_baseline.tsv`（只减不增）。
//! 依赖方向与边界契约一条未变（engine 仍不认得本目录；本目录仍不直接写 DB）。
//!
//! # 注册约定（五个 adapter 一致，别各自发明）
//!
//! - 工厂必须校验 `raw` 配置并返回 `Err`，**不要**交出半成品（[`crate::channel::Factory`] 的契约）；
//! - 部署密钥缺失 ⇒ 该平台**整体不装配**（判据在 `apps/mc-server/src/channels.rs`，
//!   `docs/60` §2.6 第 3 条）。**路由仍然存在**，并按各端点自己的"未配置"语义回响应
//!   （lark 列表是 200 空 + `install_supported:false`，**不是**统一 503）；
//! - 一切凭据只经 `mc_secrets::secretbox` 与 `mc-telemetry` 的 redaction 通道
//!   （`docs/60` §2.3）；
//! - adapter **不得**直接写 DB：只走 [`crate::engine::ChannelDeps`] 里注入的 port。
//!
//! # 解密器的接线（**交接项**，见 PR 描述与 `docs/32` §10）
//!
//! [`register`] 的签名（`&Registry` + `&ChannelDeps`）里**没有**部署密钥的位置 ——
//! `ChannelDeps` 是 M7-1 定死的形态，而密钥的**唯一读取口**是
//! `mc_http::state::ChannelKeys`（`mc-channel` 不得自己 `std::env::var`）。所以：
//!
//! - [`register`]（宿主当前调用的那个）用**失败关闭**的解密器注册工厂：配置里带密文令牌时，
//!   工厂**拒装配**并明说"没接线"，而不是把密文当明文用（与 anchor 的"密钥配了但端口没接线"
//!   同一条纪律）；同时打一条 `warn`；
//! - [`register_with`] 是**接线好的**入口：宿主把 `ChannelKeys::get(Slack)` 交给它即可
//!   （`SlackDeps::with_secret_box`）。宿主装配点 `apps/mc-server/src/channels.rs` 属 anchor 写集，
//!   所以这一步的落地归**引入它的那个片**（M7-4 的安装面 / M7 的 INT）。

pub mod config;
pub mod inbound;
pub mod media;
pub mod mrkdwn;
pub mod resolvers;
pub mod socket;

use mc_core::channel::ChannelKind;

use crate::engine::ChannelDeps;
use crate::registry::Registry;
use config::SlackDeps;

/// 把本平台的工厂注册进 `registry`（**失败关闭**的解密器，见模块文档的接线一节）。
///
/// 签名里的两个实参就是 adapter 能拿到的全部外部世界：一个共享注册表 + 一个 port 袋。
pub fn register(registry: &Registry, _deps: &ChannelDeps) {
    tracing::warn!(
        "slack: registering the factory without a credential decrypter; encrypted installation \
         tokens will be refused at build time (call `mc_channel::slack::register_with` with the \
         deployment key to wire it)"
    );
    registry.register(ChannelKind::Slack, socket::factory(&SlackDeps::default()));
}

/// 接线好的注册入口（宿主把部署密钥交进来；见模块文档的接线一节）。
pub fn register_with(registry: &Registry, deps: &SlackDeps) {
    registry.register(ChannelKind::Slack, socket::factory(deps));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelConfig;
    use crate::engine::resolvers::{
        CommandClassifier, EngineResult, IssueCreator, NoCommands, RunTriggerer, SessionReader,
    };
    use crate::engine::supervisor::{
        AcquireLeaseParams, InstallationStore, LeaseStore, ReleaseLeaseParams,
    };
    use crate::engine::{ChannelDeps, Router, RouterConfig};
    use async_trait::async_trait;
    use base64::Engine as _;
    use mc_core::id::Id;
    use std::collections::HashSet;
    use std::sync::Arc;

    struct NoInstallations;

    #[async_trait]
    impl InstallationStore for NoInstallations {
        async fn list_active(&self) -> EngineResult<Vec<crate::engine::supervisor::Installation>> {
            Ok(Vec::new())
        }
    }

    struct NoLeases;

    #[async_trait]
    impl LeaseStore for NoLeases {
        async fn list_held(&self, _ids: &[Id]) -> EngineResult<HashSet<Id>> {
            Ok(HashSet::new())
        }
        async fn try_acquire(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
            Ok(())
        }
        async fn renew(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
            Ok(())
        }
        async fn release(&self, _params: ReleaseLeaseParams) -> EngineResult<()> {
            Ok(())
        }
    }

    struct NoTrigger;

    #[async_trait]
    impl RunTriggerer for NoTrigger {
        async fn schedule_chat_run(
            &self,
            _params: crate::engine::resolvers::ChatRunParams,
        ) -> EngineResult<()> {
            Ok(())
        }
        async fn drain(&self) -> EngineResult<()> {
            Ok(())
        }
    }

    struct NoReader;

    #[async_trait]
    impl SessionReader for NoReader {
        async fn workspace_identity(
            &self,
            _workspace_id: Id,
        ) -> EngineResult<crate::engine::resolvers::WorkspaceIdentity> {
            Ok(crate::engine::resolvers::WorkspaceIdentity::default())
        }
    }

    struct NoIssues;

    #[async_trait]
    impl IssueCreator for NoIssues {
        async fn create_issue(
            &self,
            _params: crate::engine::resolvers::ChannelIssueParams,
        ) -> EngineResult<crate::engine::resolvers::ChannelIssueOutcome> {
            Err(crate::engine::resolvers::EngineError::infra("unused"))
        }
    }

    fn deps() -> ChannelDeps {
        let router = Arc::new(Router::new(
            Arc::new(NoCommands) as Arc<dyn CommandClassifier>,
            Arc::new(NoTrigger),
            Arc::new(NoReader),
            Arc::new(NoIssues),
            RouterConfig::default(),
        ));
        ChannelDeps::new(
            Arc::clone(&router),
            Arc::new(NoInstallations),
            Arc::new(NoLeases),
        )
    }

    fn config(raw: serde_json::Value) -> ChannelConfig {
        ChannelConfig {
            kind: ChannelKind::Slack,
            raw,
            installation_id: None,
            handler: None,
        }
    }

    /// `register` 真的把工厂放进表里（起手补充动作 2：不留空壳），且**失败关闭**：
    /// 没接线时带密文的配置被拒，而不是把密文当明文。
    #[test]
    fn register_installs_a_fail_closed_factory() {
        let registry = Registry::new();
        assert!(registry.is_empty());
        register(&registry, &deps());
        assert_eq!(registry.kinds(), vec![ChannelKind::Slack]);
        let Err(error) = registry.build(config(serde_json::json!({
            "app_id": "A1",
            "app_token_encrypted": "QUJD",
            "bot_token_encrypted": "QUJD",
        }))) else {
            panic!("失败关闭的解密器必须拒掉带密文的配置")
        };
        assert_eq!(error.code(), "channel_invalid_config");
        // 错误文案不回显密文（凭据纪律）。
        assert!(!error.to_string().contains("QUJD"));
    }

    /// 接线好的入口：带上部署密钥就能造出真 Channel。
    #[test]
    fn register_with_secret_box_builds_a_channel() {
        let boxed = mc_secrets::secretbox::SecretBox::new(&[7u8; 32]).expect("key");
        let app = boxed.seal(b"xapp-real").expect("seal");
        let bot = boxed.seal(b"xoxb-real").expect("seal");
        let registry = Registry::new();
        register_with(&registry, &config::SlackDeps::with_secret_box(boxed));
        let channel = registry
            .build(config(serde_json::json!({
                "app_id": "A1",
                "bot_user_id": "UBOT",
                "app_token_encrypted": base64::engine::general_purpose::STANDARD.encode(app),
                "bot_token_encrypted": base64::engine::general_purpose::STANDARD.encode(bot),
            })))
            .expect("接线好就能造");
        assert_eq!(channel.kind(), ChannelKind::Slack);
        assert!(channel
            .capabilities()
            .has(crate::capability::Capability::TEXT));
    }

    /// 注册表上一旦有 Slack 工厂，`ChannelDeps` 的共享 handler 仍是同一个 `Router`。
    #[test]
    fn assembly_keeps_the_shared_handler() {
        let deps = deps();
        assert!(Arc::ptr_eq(
            &deps.handler(),
            &(Arc::clone(&deps.router) as crate::message::SharedInboundHandler)
        ));
    }
}
