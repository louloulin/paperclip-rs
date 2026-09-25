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
pub mod strings;
pub mod types;

use crate::engine::ChannelDeps;
use crate::registry::Registry;

/// 把本平台的工厂注册进 `registry`（anchor 期空实现，见模块文档）。
///
/// 签名里的两个实参就是 adapter 能拿到的全部外部世界：一个共享注册表 + 一个 port 袋。
pub fn register(_registry: &Registry, _deps: &ChannelDeps) {
    // M7 的 M7-15 … M7-20 在这里 `registry.register(ChannelKind::WeCom, factory)`。
}
