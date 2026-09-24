//! `mc-channel`：渠道**运行时**层（`Channel` trait / `Registry` / `Capability` / engine /
//! slack · lark · dingtalk · wecom · telegram 五个 adapter）。
//!
//! **状态：M7-0 anchor 只落文件与边界**（`LUM-1765` / `docs/60-M7-PLAN.md` §5）—— 本文件
//! 只有模块声明与下面的归属表；`engine/` 里的四个文件是**签名 + `todo!()` 位**，五个
//! adapter 的 `register()` 是**空实现**。所以本 crate 现在可编译、可门禁、可独立验收，
//! 但**不实现任何路由、不碰任何平台 wire**。
//!
//! ## 为什么是**一个**新 crate（`docs/60` §2.2 的三条判据）
//!
//! 1. **不被 daemon 与 http 同时依赖**：上游 `internal/daemon` 零引用 `internal/integrations`，
//!    渠道侧与 runtime 侧**经表**通信（渠道入站建 issue/task 行，daemon 认领 task 行）
//!    ⇒ 渠道运行时不该落进 `mc-core` / `mc-daemon`；
//! 2. **per-channel trait 抽象是硬需求**：engine 只依赖 trait、adapter 只依赖 trait，
//!    这条编译边界需要一个 crate 内的 `channel.rs` + 五个平台模块；trait 与其 5 个实现
//!    必须同在**一个** crate，否则共享件（`channel/` + `channel/engine/` 共 5,315 上游行）
//!    会无处可去、只能再造第 6 个 crate；
//! 3. **凭据共用**（验签不存在，见 `docs/60` §1.5）：共用件是**密钥端口**，落
//!    `mc-secrets`；这不构成"把 5 个 adapter 也塞进共享 crate"的理由 —— 理由仍是判据 2。
//!
//! ## 边界契约（`docs/60` §2.6，逐条可测）
//!
//! 1. **engine 不知道平台**：`engine/**` 不得 `use` 任何 `slack::` / `lark::` / … 具体类型；
//!    跨边界只走 [`channel::Channel`] / [`message::InboundHandler`] /
//!    `mc_core::channel::message::{InboundMessage, OutboundMessage}`。
//!    反向同理：adapter **不得**直接写 DB，只走注入进来的 port。
//! 2. **凭据只经 `mc_secrets::secretbox`**：任何 adapter / 路由不得把明文 secret 放进
//!    `Debug` / `Display` / 日志插值（`docs/60` §2.3 四条判据）。
//! 3. **未配置 = 该渠道不装配**：装配判据是**部署密钥存在**，装配点在
//!    `apps/mc-server/src/channels.rs`；路由侧的"未配置"响应**逐 fixture 对齐**
//!    （lark 列表是 200 空 + `install_supported:false`，**不是**统一 503）。
//! 4. **入站是 push，不是 poll**：[`channel::Channel::connect`] **阻塞跑接收循环**，把归一化
//!    消息交给构造时注入的 [`message::InboundHandler`]；非 nil error = 基础设施失败，
//!    nil = 已分类（产品性丢弃**不是**错误）。
//! 5. **出站不阻塞 ACK**：handler 触发的任何回复（绑定卡 / 离线提示 / 打字指示）脱离
//!    adapter 的 ACK 路径。
//!
//! ## 上游与写者（`docs/60` §3.3 的写集表）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` | M7-0（本 anchor） | —— |
//! | `src/channel.rs` | M7-0 建 / **M7-1 填** | `channel/channel.go`（`Channel` 五方法 / `Config` / `Factory`） |
//! | `src/registry.rs` | M7-0 建 / **M7-1 填** | `channel/registry.go`（`Registry` / `ErrUnknownType`） |
//! | `src/capability.rs` | M7-0 建 / **M7-1 填** | `channel/capability.go`（8 位位图 + `String()`） |
//! | `src/message.rs` | M7-0 建 / **M7-1 填** | `channel/handler.go`（`InboundHandler`） |
//! | `src/engine/{mod,router,supervisor,resolvers}.rs` | M7-0 建 / **M7-1 填** | `channel/engine/**`（5,315 行里的路由/监管/解析） |
//! | `src/engine/{session,batcher,lease,commands}.rs` | **M7-2** | `channel/engine/**`（会话/租约/命令） |
//! | `src/slack/**` | M7-3 / M7-4 | `internal/integrations/slack` |
//! | `src/telegram/**` | M7-5 / M7-6 | `internal/integrations/telegram` |
//! | `src/dingtalk/**` | M7-7 / M7-8 / M7-9 | `internal/integrations/dingtalk` |
//! | `src/lark/**` | M7-10 … M7-14 | `internal/integrations/lark` |
//! | `src/wecom/**` | M7-15 … M7-20 | `internal/integrations/wecom` |
//!
//! ⚠️ **`src/engine/mod.rs` 的两个 stage-2 写者**：本文件只声明 anchor 拥有的四个子模块；
//! M7-2 落 `session` / `batcher` / `lease` / `commands` 时**自己**在这里追加四行 `pub mod …;`
//! （先起跑的那片追加，另一片 rebase）。这条交接纪律写在 `docs/32` §10，避免两片同时改它。
//!
//! ## 不做什么（anchor 的硬边界）
//!
//! - **零路由逻辑**：24 条渠道路由全在 `mc-http/src/routes/channels/**`，由 M7-4/5/9/14/15 填；
//! - **零平台 wire**：没有 Slack Socket Mode 信封解析、没有 Lark WS 帧解码、没有 `DingTalk`
//!   Stream 帧、没有 `WeCom` aibot 帧、没有 Telegram `getUpdates` —— 全是各 adapter 片的事；
//! - **零新迁移**：22 张渠道表都在 `migrations/upstream/**`（`docs/60` §6.4）；
//! - **不读 env**：部署密钥的读取口只有一个（`mc_http::state::ChannelKeys`）。

pub mod capability;
pub mod channel;
pub mod dingtalk;
pub mod engine;
pub mod lark;
pub mod message;
pub mod registry;
pub mod slack;
pub mod telegram;
pub mod wecom;

pub use capability::Capability;
pub use channel::{Channel, ChannelConfig, ChannelError, Factory};
pub use message::InboundHandler;
pub use registry::Registry;
