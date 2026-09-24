//! 解析器：把**平台路由键**解析成 installation / `chat_session`（上游 `channel/engine/` 的
//! resolvers 面）。
//!
//! **状态：M7-0 anchor 只落签名**（`LUM-1765`）—— 本文件是 `todo!()` 位，实现归 **M7-1**。
//!
//! # 为什么它必须是**唯一实现点**（`docs/60` §2.1 / `docs/57` §3.1 的同一纪律）
//!
//! 五个平台的路由键各不相同（Lark 用 `app_id`，Slack 用 `team_id`，`WeCom` 用 `bot_id`，
//! `DingTalk` 用 `app_key`，Telegram 用 bot token 对应的 bot id），但**落点相同**：
//! 一行 `channel_installation`（`124` 的 `idx_channel_installation_type_appid` 就是
//! `(channel_type, config->>'app_id')` 上的函数唯一索引）。解析逻辑若抄进五个 adapter，
//! 就会出现五份"哪一行算命中"的判断 —— 那是同一张表五个真值。所以这里只留一个端口，
//! adapter 在构造时拿到的是**已解析好的** installation。
//!
//! # 命中的唯一语义（实现时别放宽）
//!
//! - 命中 = `(channel_type, 平台路由键)` 唯一确定一行 `status='active'` 的安装；
//! - 未命中 ⇒ `Ok(None)`（调用方按"这条消息不属于任何安装"处理，**不是**错误）；
//! - 同一键命中多行 ⇒ **实现错误**（索引保证不会），返回 `Err`，别"取第一条"。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use crate::channel::ChannelResult;
use crate::engine::ChannelDeps;

/// 解析结果：这条入站消息属于哪个安装、以及（若已建）哪个 `chat_session`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoute {
    pub kind: ChannelKind,
    pub installation_id: Id,
    /// 已建的会话绑定（`channel_chat_session_binding`）；还没有绑定时为 `None`
    /// （绑定是路由第 3 步的事，见 `router.rs`）。
    pub chat_session_id: Option<Id>,
}

/// 平台路由键 → installation 的解析端口。
///
/// 实现落点是 M7-1（走 `mc-repos` 的 `channel` 模块；**不在** adapter 里）。
#[async_trait]
pub trait ResolverSet: Send + Sync {
    /// 解析一条入站消息；未命中返回 `Ok(None)`（见模块文档的命中语义）。
    async fn resolve(&self, message: &InboundMessage) -> ChannelResult<Option<ResolvedRoute>>;
}

/// engine 用的默认解析器（M7-1 实现）。
///
/// ⚠️ anchor 期不可构造：本结构存在的意义只是把"实现类"的位置固定在**本文件**，
/// 免得 M7-1 顺手在 adapter 里再写一份解析。
pub struct InstallationResolver {
    deps: Arc<ChannelDeps>,
}

impl std::fmt::Debug for InstallationResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallationResolver")
            .field("deps", &self.deps)
            .finish()
    }
}

impl InstallationResolver {
    /// 装配（M7-1）。
    ///
    /// # Panics
    ///
    /// anchor 期未实现。
    pub fn new(_deps: Arc<ChannelDeps>) -> Self {
        todo!("M7-1：装配解析器（走 mc_repos::channel）")
    }
}

#[async_trait]
impl ResolverSet for InstallationResolver {
    async fn resolve(&self, _message: &InboundMessage) -> ChannelResult<Option<ResolvedRoute>> {
        todo!("M7-1：(channel_type, 平台路由键) → status='active' 的唯一安装行")
    }
}
