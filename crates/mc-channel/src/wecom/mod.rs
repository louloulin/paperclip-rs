//! `WeCom` adapter（上游 `internal/integrations/wecom`（90 文件 / 33 非测试 / 15,022 上游行））。
//!
//! **状态：M7-0 anchor 只落空 `register()`**（`LUM-1765` / `docs/60-M7-PLAN.md` §5）——
//! 本文件的**唯一**内容是"这个平台在这里注册工厂"的位置声明；实现归 M7-15 … M7-20
//! （`docs/60` §3.3 的写集表：本目录下的每个子文件都有**一个**写者）。
//!
//! # 这个平台的面（实现者按这三条认领自己的文件）
//!
//! - aibot WebSocket（per-installation supervisor）；
//! - 4 条路由 + `/api/wecom/binding/redeem`；
//! - 媒体面（下载/上传 + CIDR 白名单 + `media_crypt`）与打字/限流/去重；
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
//! # 不做什么（anchor 期）
//!
//! 本文件**不含**任何平台 wire 代码、不含帧格式、不含 HTTP 客户端 —— 五行之外的一切都是
//! M7-15 … M7-20 的事。anchor 的 `register()` 是**空实现**，因此注册表在 anchor 期是空的
//! （这也正是"零路由、零读数的变化"的形态证据）。

// M7-15（`LUM-1780` / `docs/60-M7-PLAN.md` §3.3）：wecom 的契约 / 凭据 / 安装与绑定面。
// 追加这 7 行是本片写集的**唯一** mod.rs 改动（写集勘误见 `docs/32` §31 的 D1）：
// 上游 `internal/integrations/wecom/{types,credentials,credential_probe,installation,store,
// binding,strings,language,metrics}.go` 的本地落点。
pub mod binding;
pub mod credentials;
pub mod installation;
pub mod metrics;
pub mod store;
pub mod stream_store;
pub mod strings;
pub mod types;
pub mod ws_frame;
pub mod ws_sender;

// M7-17（`LUM-1782` / `docs/60-M7-PLAN.md` §3.3）：wecom 的中继与出站回复面。
// 追加这 4 行是本片写集的**唯一** mod.rs 改动（写集勘误见 `docs/32` §34 的 D12）：
// 上游 `internal/integrations/wecom/{relay_outbound,outbound,outbound_outcome,replier}.go`
// 的本地落点。`relay.rs` 上游 1,578 行 ⇒ 按 §6.3 拆出 `relay/` 子目录（同 `ws_frame/`、
// `ws_sender/` 的先例），`outbound.rs` / `replier.rs` 各带一个 `tests.rs`。
pub mod media_crypt;
pub mod media_download;
pub mod media_guard;
pub mod media_ingest;
pub mod media_stream;
pub mod media_upload;
pub mod outbound;
pub mod outbound_media;
pub mod outcome;
pub mod relay;
pub mod replier;

// M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3）：wecom 的**媒体面**（下载 / 上传 / 地址闸 /
// 加解密 / 摄入解析器 / 出站附件投递）。上面同一段里的 7 行就是它：
// `media_crypt` / `media_download` / `media_guard` / `media_ingest` / `media_stream` /
// `media_upload` / `outbound_media`（`rustfmt` 把它们与 M7-17 那 4 行并成一个按字母序的连续段）。
//
// 🔴 写集勘误（`docs/32` §35 的 D1，同一类第 14 次）：issue rev 5 的写集只列了 **6** 个文件，
// 而派生表 `docs/fixtures/m7-slice-upstream-files.tsv` 的 M7-18 行里有第 7 个
// （`internal/integrations/wecom/media_stream.go`，上游 171 行，流式解密那条路）⇒ 本片一并落地。
// 另按 §6.3 的门 ⑩ 拆分：`media_download.rs` / `media_stream.rs` / `media_ingest.rs` 各带一个
// 子目录（子模块 + `tests.rs`），`media_upload.rs` / `media_guard.rs` / `media_crypt.rs` /
// `outbound_media.rs` 各带一个 `tests.rs`。

use crate::engine::ChannelDeps;
use crate::registry::Registry;

// M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）：wecom 的**入站与解析**面 —— 本片是 wecom
// 子波的**最后一个代码片**，也是这一波的门禁证据（端到端收发回路）的承担者。
// 追加这 5 行是本片写集的**mod.rs 改动**（issue rev 5 的写集漏项，同一类第 15 次 ——
// 与 M7-17 的 D12 / M7-18 的 D1 同款：新文件必须先被这里声明才进编译单元，否则连 `dead_code`
// 都不报）。`register()` 的**填充**也是本片的（anchor 把它留成空实现给"注册工厂与渠道路由的归口片"）。
pub mod inbox_message;
pub mod markdown;
pub mod resolvers;
pub mod seal;
pub mod wecom_channel;

// 本片在 `crate::wecom::` 这一层给出的一小撮常用名。
pub use inbox_message::{build_inbox_markdown, InboxCardRenderer};
pub use markdown::{break_member_links, MemberLinkBreaker};
pub use resolvers::{wecom_msg_from_raw, WeComResolverSet, ORIGIN_WECOM_CHAT};
pub use seal::{classify_seal, fallback_budget, SealVerdict};
pub use wecom_channel::inbound::WeComInboundMessage;
pub use wecom_channel::{
    register_resolvers, register_with, SenderRegistry, WeComChannel, WeComDeps, DEFAULT_WS_URL,
    SEND_NOT_SUPPORTED, SUBSCRIBE_TIMEOUT,
};

// M7-20（`LUM-1785` / `docs/60-M7-PLAN.md` §3.3）：wecom 的**打字指示 / 限流 / 去重 / 追踪**面 ——
// 本片是 wecom 子波的**最后一个代码片**（M7 只剩 M7-21 INT）。
// 追加这 5 行是本片写集的 mod.rs 改动（issue 正文的写集此前只列了 5 个**新**文件，本节把它正式
// 补进正典写集 —— 同一类勘误第 16 次，先例见 `docs/32` §33 的 D1 / §34 的 D12 / §35 的 D1 与 §38）：
// 新文件必须先被这里声明才进编译单元，否则连 `dead_code` 都不报。
// 上游来源：`internal/integrations/wecom/{typing_indicator.go,rate_limit.go,senders_registry.go,
// dedupe_redis.go,trace.go}`（2,214 行，非测试口径）。
pub mod dedupe;
pub mod rate_limit;
pub mod senders;
pub mod trace;
pub mod typing;

/// 把本平台的工厂注册进 `registry`（**失败关闭**，与 slack / dingtalk 同款）。
///
/// 宿主（`apps/mc-server/src/channels.rs`）只拿得到 [`ChannelDeps`] —— 那里**没有**部署密钥，
/// 所以这条路径注册的工厂造不出任何 Channel：它每一次 `build` 都以 `channel_invalid_config`
/// 失败，而不是假装接上了。要真接线，宿主调
/// [`wecom_channel::register_with`] 并交进一个 [`WeComDeps`]。
pub fn register(registry: &Registry, deps: &ChannelDeps) {
    wecom_channel::register(registry, deps);
}
