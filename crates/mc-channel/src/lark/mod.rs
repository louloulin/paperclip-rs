//! Feishu / Lark adapter（上游 `internal/integrations/lark`（72 文件 / 37 非测试 / 10,957 上游行））。
//!
//! **状态：M7-0 anchor 只落空 `register()`**（`LUM-1765` / `docs/60-M7-PLAN.md` §5）——
//! 本文件的**唯一**内容是"这个平台在这里注册工厂"的位置声明；实现归 M7-10 … M7-14
//! （`docs/60` §3.3 的写集表：本目录下的每个子文件都有**一个**写者）。
//!
//! # 这个平台的面（实现者按这三条认领自己的文件）
//!
//! - 自建 WS 长连接（`POST /callback/ws/endpoint` 引导 + 分片重组）；
//! - 5 条路由 + `/api/lark/binding/redeem`；
//! - **两套表并存**（泛化 `channel_*` + 遗留 `lark_*`），不得合并；
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
//! M7-10 … M7-14 的事。anchor 的 `register()` 是**空实现**，因此注册表在 anchor 期是空的
//! （这也正是"零路由、零读数的变化"的形态证据）。

use crate::engine::ChannelDeps;
use crate::registry::Registry;

/// 把本平台的工厂注册进 `registry`（anchor 期空实现，见模块文档）。
///
/// 签名里的两个实参就是 adapter 能拿到的全部外部世界：一个共享注册表 + 一个 port 袋。
pub fn register(_registry: &Registry, _deps: &ChannelDeps) {
    // M7 的 M7-10 … M7-14 在这里 `registry.register(ChannelKind::Lark, factory)`
    // （注册键是枚举变体 `Lark`；**存库**口径才是 `feishu`，见 `ChannelKind::storage_str`）。
}
