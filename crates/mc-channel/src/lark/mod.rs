//! Feishu / Lark adapter（上游 `internal/integrations/lark`（72 文件 / 37 非测试 / 10,957 上游行））。
//!
//! **状态：子模块已落地到 M7-14；`register()` 已接线**（`LUM-1765` / `docs/60-M7-PLAN.md` §5）。
//! 本文件的职责是"这个平台在这里注册工厂"的位置声明 + 子模块声明；M7-10 … M7-13 落了
//! 客户端 / WS / 入站 / 出站四棵子树，M7-14 落**安装与绑定面 + 5 条路由**并填上 `register()`。落地进度：M7-10 落 `client`/`http_client`/`params`/`types`
//! （lark HTTP 客户端与类型），M7-11 落 `ws_connector`/`ws_endpoint`/`ws_frame`/
//! `ws_frame_decoder`（自建 WS 长连接），M7-12 落入站回路，M7-13 落**出站 / 回复 / 会话桥**
//! （`store`/`channel_store`/`audit`/`typing`/`replier`/`outbound`）；其余（安装与绑定面 + 5 条
//! 路由 + 注册工厂）归 M7-14。
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
use mc_core::channel::ChannelKind;

// M7-10（`LUM-1775`）落地的四个子模块：**只追加这四行**（见本文件「不做什么」）。
pub mod client;
pub mod http_client;
pub mod params;
pub mod types;

// M7-11（`LUM-1776`）落地的四个子模块（自建 WS 长连接面）：
// `ws_frame` = 二进制帧信封 + 分片重组；`ws_frame_decoder` = 事件 JSON 解码；
// `ws_endpoint` = `POST /callback/ws/endpoint` 引导（**我方发往 lark 的地址**，
// 不是本服务暴露的路由 —— 见 `docs/60` §1.5）；`ws_connector` = 会话与帧循环。
pub mod ws_connector;
pub mod ws_endpoint;
pub mod ws_frame;
pub mod ws_frame_decoder;

// M7-12（`LUM-1777`）落地的五个子模块（lark 入站回路）：
// `content_flatten` = 正文摊平 + 提及改写 + markdown 探测；`enricher` = 富上下文装配
// （引用 / 转发 / 群近况）；`media` = 媒体引用抽取与摄入；`resolvers` = 安装 / 身份 / 去重 /
// 会话 / 审计的解析器集合；`feishu_channel` = 归一化 + `Channel` 实现 + 工厂。
pub mod content_flatten;
pub mod enricher;
pub mod feishu_channel;
pub mod media;
pub mod resolvers;

// M7-13（`LUM-1778`）落地的六个子模块（出站 / 回复 / 会话桥）：
// `store` = 存储值形状与两族表的 config 边界；`channel_store` = 数据层桥（上游 `ChannelStore`）；
// `audit` = 丢弃审计写入面；`typing` = 打字指示生命周期；`replier` = 判决回复器；
// `outbound` = 任务生命周期 → 文本 / markdown 卡 / 错误卡 / 卡片 patch。
pub mod audit;
pub mod channel_store;
pub mod outbound;
pub mod replier;
pub mod store;
pub mod typing;

// M7-14（`LUM-1779`）落地的四个子模块（安装与绑定面 + 两条启动期回填）：
// `installation` = `lark_installation` 的读 / 写 / 撤销 / upsert 与 `InstallError`；
// `registration` = RFC 8628 设备流（`begin` → 轮询 → 终态）+ 会话状态表；
// `binding` = 单次绑定令牌的铸与兑换；`backfill` = 两条一次性的升级修复。
pub mod backfill;
pub mod binding;
pub mod installation;
pub mod registration;

/// 把本平台的工厂注册进 `registry`（**失败关闭**的解密器与未接线的连接器，见下）。
///
/// 签名里的两个实参就是 adapter 能拿到的全部外部世界：一个共享注册表 + 一个 port 袋。
/// 没有部署密钥时宿主会走这一支（`docs/60` §2.4 / §2.6 第 3 条：该渠道**整体不装配**），
/// 于是注册进去的工厂只会在 `build` 时**响亮地**拒绝（[`feishu_channel::factory`] 的
/// `InvalidConfig`），而不是交出一个假装连上的半成品 —— 与 slack / telegram / dingtalk 的
/// `register` 同一手法。接线好的那一支走 [`register_with`]。
#[cfg(test)]
mod tests;

pub fn register(registry: &Registry, _deps: &ChannelDeps) {
    tracing::warn!(
        "lark: registering the factory without a wired connector; installations will be refused \
         at build time (call `mc_channel::lark::register_with` with the deployment connector to \
         wire it)"
    );
    let deps = feishu_channel::FeishuChannelDeps {
        connector: None,
        api_client: std::sync::Arc::new(client::StubApiClient::new()),
        decrypter: feishu_channel::Decrypter::fail_closed(),
        enricher: None,
    };
    registry.register(ChannelKind::Lark, feishu_channel::factory(deps));
}

/// 接线好的注册入口（宿主把部署连接器与解密器交进来）。
pub fn register_with(registry: &Registry, deps: feishu_channel::FeishuChannelDeps) {
    registry.register(ChannelKind::Lark, feishu_channel::factory(deps));
}
