//! Slack adapter（上游 `internal/integrations/slack`（32 文件 / 16 非测试 / 4,490 上游行））。
//!
//! **状态：M7-4 已落「出站 / 回复 / 命令历史 / 安装与绑定面」**（`LUM-1769`）——
//! M7-3 落的是入站回路（`LUM-1768`）。两片合起来构成 Slack 这一渠道的**完整收发回路**
//! （`docs/60-M7-PLAN.md` §4.2 的表：收来自 M7-3、发来自 M7-4、替身是本地 WS 服务端）。
//!
//! # 这个平台的面
//!
//! - `socket_mode`：**每个安装一条**连接（BYO 模型下每个安装带自己的 `xapp-` app token），
//!   接收循环在 [`inbound::SlackChannel::connect`] 里阻塞跑；
//! - BYO 安装（4 条 workspace 路由 + `/api/slack/binding/redeem`）—— [`install`] / [`binding`]；
//! - Block Kit 出站与回复投递（`chat.postMessage` / Markdown→`mrkdwn` / 线程化）——
//!   [`outbound`]（发送器）+ [`replier`]（判决 → 文案）；
//! - 斜杠命令（`/issue` / `/new` / `/clear`，从**同一条** Socket Mode 连接到达）—— [`slash`]；
//! - 「处理中」反应指示器 —— [`typing`]；会话历史读面 —— [`history`]。
//!
//! # 本目录的写者表（M7-3 / M7-4）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `config.rs` | **M7-3** | `config.go`（安装配置 + 凭据解密） |
//! | `inbound.rs` + `inbound/tests.rs` | **M7-3** | `inbound.go`（事件归一化 + 帧 → 信封） |
//! | `socket.rs` + `socket/tests.rs` | **M7-3**（M7-4 只改 `send` 的接线，见下） | `slack_channel.go`（接收循环 + 工厂） |
//! | `media.rs` + `media/tests.rs` | **M7-3** | `media_ingest.go`（下载 / 上传 / 意图账本） |
//! | `mrkdwn.rs` | **M7-3** | `mrkdwn.go`（标准 Markdown → Slack `mrkdwn`） |
//! | `resolvers.rs` + `resolvers/tests.rs` | **M7-3** | `resolvers.go`（安装 / 身份 / 去重 / 会话 / 审计） |
//! | `outbound.rs` + `outbound/tests.rs` | **M7-4** | `outbound.go` + `channel.go` 的 sender 那一半 |
//! | `replier.rs` + `replier/tests.rs` | **M7-4** | `replier.go` |
//! | `typing.rs` | **M7-4** | `typing_indicator.go` |
//! | `history.rs` + `history/{reader,text,flatten,tests}.rs` | **M7-4** | `history.go` |
//! | `slash.rs` + `slash/tests.rs` | **M7-4** | `slash_command.go` + `slash_control.go` |
//! | `install.rs` + `install/tests.rs` | **M7-4** | `install.go` + `byo_install.go` |
//! | `binding.rs` + `binding/tests.rs` | **M7-4** | `binding.go` |
//! | `mod.rs` + `tests.rs` | **M7-4**（模块表与注册面） | —— |
//!
//! ## 写集勘误（**逐条登记**，照 M7-2 / M7-3 先例）
//!
//! `docs/60` §3.3 给 M7-4 的格子是 `slack/{outbound,replier,typing,history,slash,install,`
//! `binding}.rs` 与 `routes/channels/slack.rs`。起手补充已追加 `slack/mod.rs`（本片要可见新模块、
//! 要接上出站面）与条件性的 `slack/socket.rs`。本片**再追加**的路径只有一类：用例文件与
//! `history` 的子文件。理由全是**门 ⑩ 的 800 行硬限**（不是拆凑数字），与 M7-3 对
//! `{inbound,media,resolvers,socket}/tests.rs` 的处理逐字同款：
//!
//! - `slack/{outbound,replier,slash,install,binding}/tests.rs`：用例内联后
//!   `outbound`（444 行 + 用例）/ `slash`（≈700 行 + 用例）都越 800；
//! - `slack/history/{reader,text,flatten,tests}.rs`：上游 `history.go` 一个文件 737 行，
//!   移植后 1,374 行，**必须**先拆（切点是上游自身的"读面 / 窗口与过滤 / 摊平与命名"三段）；
//! - `slack/tests.rs`：`mod.rs` 原有的三条注册用例搬出来，给下面的模块表腾位置。
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
//! # 解密器的接线（**交接项**，见 PR 描述与 `docs/32` §10 / §13.5）
//!
//! [`register`] 的签名（`&Registry` + `&ChannelDeps`）里**没有**部署密钥的位置 ——
//! `ChannelDeps` 是 M7-1 定死的形态，而密钥的**唯一读取口**是
//! `mc_http::state::ChannelKeys`（`mc-channel` 不得自己 `std::env::var`）。所以：
//!
//! - [`register`]（宿主当前调用的那个）用**失败关闭**的解密器注册工厂：配置里带密文令牌时，
//!   工厂**拒装配**并明说"没接线"，而不是把密文当明文用；同时打一条 `warn`；
//! - [`register_with`] 是**接线好的**入口：宿主把 `ChannelKeys::get(Slack)` 交给它即可
//!   （`SlackDeps::with_secret_box`）。
//!
//! ## M7-4 的状态：**工厂侧已闭环，解析器面仍悬空**
//!
//! | 面 | 状态 |
//! | --- | --- |
//! | `Channel::send`（出站） | **已闭环**：工厂交出带 [`outbound::Sender`] 的 channel（`socket.rs`） |
//! | 4 条路由（安装 / 绑定） | **已闭环**：`routes/channels/slack.rs` 自带 PG 实现与测试 |
//! | 解析器面（出站回复器 / 打字指示 / 斜杠命令） | **悬空**：装配点是 `apps/mc-server/src/channels.rs`（**anchor 写集**）⇒ 本片**不擅自扩写集**，改为提供 [`SlackResolverWiring`] + [`register_resolvers`] 这一个入口，由 INT / 后续锚点调一次 |
//!
//! 这张表就是"缺口登记"的形式：**哪一半闭环、哪一半等谁**，一眼可查（同 §13.5 的手法）。

pub mod binding;
pub mod config;
pub mod history;
pub mod inbound;
pub mod install;
pub mod media;
pub mod mrkdwn;
pub mod outbound;
pub mod replier;
pub mod resolvers;
pub mod slash;
pub mod socket;
pub mod typing;

use std::sync::Arc;

use mc_core::channel::ChannelKind;
use mc_repos::channel::binding::ChannelBindingRepo;
use mc_repos::channel::dedup::ChannelInboundDedupRepo;
use mc_repos::channel::inbound_audit::ChannelInboundAuditRepo;
use mc_repos::channel::installation::ChannelInstallationRepo;
use mc_repos::channel::session::ChannelChatSessionRepo;
use mc_repos::member::MemberRepo;

use crate::engine::{ChannelDeps, Router};
use crate::registry::Registry;
use config::{Decrypter, SlackDeps};
use replier::{BindingMinter, OutboundLedger, SlackOutboundReplier};
use resolvers::SlackResolverSet;
use typing::TypingIndicatorManager;

/// 密钥盒缺失时用的**失败关闭**解密器（见模块文档的接线一节）。
///
/// 单独抽出来是为了让下面两个注册函数共用同一条纪律，而不是各写一份 `warn`。
fn fail_closed_decrypter() -> Decrypter {
    Decrypter::fail_closed()
}

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

/// 注册表工厂的**失败关闭**形态（显式给出，便于测试与自检断言"就是它"）。
#[must_use]
pub fn fail_closed_deps() -> SlackDeps {
    SlackDeps {
        decrypt: fail_closed_decrypter(),
    }
}

// =====================================================================
// 解析器面的装配（模块文档「登记缺口」那一段的落地形态）
// =====================================================================

/// 解析器面的装配袋（M7-4）。
///
/// 六个**泛化渠道仓储** + 解密器是必需项；出站回复器要的绑定服务 / 记账口是可选
/// （缺任一个 ⇒ 绑定卡与出站记账那一支关掉，状态告知照发 —— 上游同）。
pub struct SlackResolverWiring {
    pub installations: ChannelInstallationRepo,
    pub bindings: ChannelBindingRepo,
    pub members: MemberRepo,
    pub dedup: ChannelInboundDedupRepo,
    pub sessions: Arc<ChannelChatSessionRepo>,
    pub audits: ChannelInboundAuditRepo,
    pub decrypt: Decrypter,
    /// web app 主机（绑定链接要它；空串 ⇒ 绑定卡被跳过）。
    pub app_url: String,
    /// 绑定令牌服务（缺 ⇒ 跳过绑定卡）。
    pub binding_minter: Option<Arc<dyn BindingMinter>>,
    /// 出站记账口（缺 ⇒ 不记 `channel_outbound_message`，历史过滤随之变弱）。
    pub ledger: Option<Arc<dyn OutboundLedger>>,
}

impl std::fmt::Debug for SlackResolverWiring {
    /// 手写：只有仓储与解密器（后者自己脱敏），**没有**任何凭据字段。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackResolverWiring")
            .field("decrypt", &self.decrypt)
            .field("app_url", &self.app_url)
            .field("binding_minter", &self.binding_minter.is_some())
            .field("ledger", &self.ledger.is_some())
            .finish_non_exhaustive()
    }
}

impl SlackResolverWiring {
    /// 装配（不含绑定服务 / 记账口）。
    ///
    /// `too_many_arguments`：这八个实参就是上游 `SlackResolverWiring` 的字段本身
    /// （六个泛化仓储 + 解密器 + app 主机），收进一个结构只是把同一组字段挪一层。
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        installations: ChannelInstallationRepo,
        bindings: ChannelBindingRepo,
        members: MemberRepo,
        dedup: ChannelInboundDedupRepo,
        sessions: Arc<ChannelChatSessionRepo>,
        audits: ChannelInboundAuditRepo,
        decrypt: Decrypter,
        app_url: impl Into<String>,
    ) -> Self {
        Self {
            installations,
            bindings,
            members,
            dedup,
            sessions,
            audits,
            decrypt,
            app_url: app_url.into(),
            binding_minter: None,
            ledger: None,
        }
    }

    /// 挂上绑定令牌服务（出站回复器的绑定卡要用它）。
    #[must_use]
    pub fn with_binding_minter(mut self, minter: Arc<dyn BindingMinter>) -> Self {
        self.binding_minter = Some(minter);
        self
    }

    /// 挂上出站记账口。
    #[must_use]
    pub fn with_ledger(mut self, ledger: Arc<dyn OutboundLedger>) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// 组装出本片的三件套（出站回复器 + 打字指示 + M7-3 的五个必填端口）。
    #[must_use]
    pub fn into_resolver_set(self) -> SlackResolverSet {
        let sender = Arc::new(outbound::Sender::http());
        let replier = Arc::new(SlackOutboundReplier::new(
            Arc::clone(&sender),
            self.decrypt.clone(),
            self.binding_minter.clone(),
            self.ledger.clone(),
            self.app_url.clone(),
            None,
        ));
        let typing = Arc::new(TypingIndicatorManager::http(
            Some(Arc::new(typing::RepoInstallationConfigs::new(
                self.installations.clone(),
            ))),
            self.decrypt.clone(),
        ));
        SlackResolverSet::from_repos(
            self.installations,
            self.bindings,
            self.members,
            self.dedup,
            self.sessions,
            self.audits,
        )
        .with_replier(replier)
        .with_typing(typing)
    }
}

/// 把 Slack 的解析器面注册进 `router`（宿主 / INT 片调**一次**）。
///
/// `Router::register` 是 last-writer-wins，所以重复调用是安全的（但没必要）。
pub fn register_resolvers(router: &Router, wiring: SlackResolverWiring) {
    router.register(
        ChannelKind::Slack,
        wiring.into_resolver_set().into_engine_set(),
    );
}

#[cfg(test)]
mod tests;
