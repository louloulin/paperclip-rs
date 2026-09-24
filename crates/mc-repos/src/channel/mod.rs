//! channel 仓储：22 张渠道表按**面**分文件（上游 `internal/integrations/**` 的查询面）。
//!
//! - **状态**：M7-0 anchor 只落文件与边界（`LUM-1765` / `docs/60-M7-PLAN.md` §3.3）——
//!   本文件只有模块声明与下面的归属表；八个子模块都是 doc-only 桩，由 M7-1 / M7-2 各自填
//!   自己的文件（**一个文件一个写者**，`docs/60` §3.3）。
//! - **表的落法**（22 张，**本波 0 新迁移**，全部已在 `migrations/upstream/**`；
//!   `docs/60` §6.4 的清点，逐文件实测见 §10 命令 7）：
//!
//! | 表 | 迁移 | 本模块的落点 |
//! | --- | --- | --- |
//! | `channel_installation` | `124` | `installation.rs` |
//! | `channel_user_binding` | `124` | `binding.rs` |
//! | `channel_binding_token` | `124` | `binding.rs` |
//! | `channel_chat_session_binding` | `124` + `420`/`421`/`422`（代际与活跃路由） | `session.rs` |
//! | `channel_chat_context_generation` | `377`/`378`/`379` | `session.rs` |
//! | `channel_inbound_message_dedup` | `124` | `dedup.rs` |
//! | `channel_inbound_audit` | `124` | `inbound_audit.rs` |
//! | `channel_outbound_message` | `425` + `426`/`430` | `outbound.rs` |
//! | `channel_outbound_card_message` | `124` | `outbound.rs` |
//! | `channel_reply_delivery` | `502` + `503`/`504`/`505`/`506` | `delivery.rs` |
//! | `channel_task_delivery` | `420` + `423`/`424`/`427`/`428`/`429` | `delivery.rs` |
//! | `channel_media_pending_object` | `227`…`232` | `media.rs` |
//! | `lark_installation` | `109` + `112`/`116` | `installation.rs` |
//! | `lark_user_binding` | `109` | `binding.rs` |
//! | `lark_binding_token` | `109` | `binding.rs` |
//! | `lark_chat_session_binding` | `109` + `122` | `session.rs` |
//! | `lark_inbound_message_dedup` | `109` + `113` | `dedup.rs` |
//! | `lark_inbound_audit` | `109` | `inbound_audit.rs` |
//! | `lark_outbound_card_message` | `109` | `outbound.rs` |
//! | `dingtalk_bot_identity` | `387`/`388`/`389` + `474` | `installation.rs` |
//! | `dingtalk_group_presence` | `383`/`384`/`385`/`386` | `installation.rs` |
//! | `dingtalk_group_route` | `304`…`307` + `382`（**路由已退役，见下**） | `installation.rs` |
//!
//! - **两套表并存（**不得**合并）**：`lark_*`（泛化之前的 per-channel 表）与
//!   `channel_*`（`124` 之后的泛化层）在上游**同时在用**（`lark_installation` 有 25 处非测试
//!   命中）⇒ 仓储层按上游实际读取路径实现两套，**不许**图省事把 lark 并到泛化层
//!   （会静默丢数据）。这条是 `docs/60` §6.4 / R-M7-5 的硬要求。
//! - **已退役的路由**：`dingtalk_group_route` 对应的
//!   `GET /api/workspaces/{id}/dingtalk/group-routes` 上游已删除（不在 456 条里，
//!   且 `integration_test.go:786` 主动断言 **404**）⇒ 本模块**不**为该路径建读面；
//!   行级读写归 M7-9（`group-routes` 必须保持 404，`docs/60` §1.6）。
//!
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款，**抄不要另立**）：
//!   裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、
//!   jsonb → `serde_json::Value`、bytea → `Vec<u8>`；列表查询必须带 `workspace_id` 收窄
//!   （跨工作区读 = 越权，上游一律 404 而不是 403）。
//! - **不做什么**：
//!   - 不做领域转换（`channel_installation` 行 → `mc_core::channel::Installation` 的转换在
//!     调用侧，本模块只给行结构）；
//!   - 不做进程内替代品（租约 / 去重 / 安装会话的**无 Redis** 降级在 `mc-channel`，见
//!     `docs/60` §2.5）；
//!   - 不写迁移（本波 0 新迁移）。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `installation.rs` | M7-1 | `channel_installation` + lark 遗留安装行 + dingtalk 三张身份/群表 |
//! | `binding.rs` | M7-1 | `channel_{user_binding,binding_token}` + lark 两套 |
//! | `outbound.rs` | M7-1 | `channel_outbound_message` / `channel_{,lark_}outbound_card_message` |
//! | `delivery.rs` | M7-1 | `channel_reply_delivery` / `channel_task_delivery` |
//! | `media.rs` | M7-1 | `channel_media_pending_object`（在途媒体账本） |
//! | `session.rs` | M7-2 | `channel_chat_session_binding` / `channel_chat_context_generation` / lark 遗留 |
//! | `dedup.rs` | M7-2 | `channel_inbound_message_dedup` + lark 遗留（两阶段幂等 + claim 围栏） |
//! | `inbound_audit.rs` | M7-2 | `channel_inbound_audit` + lark 遗留（**非内容**丢弃审计） |

pub mod binding;
pub mod dedup;
pub mod delivery;
pub mod inbound_audit;
pub mod installation;
pub mod media;
pub mod outbound;
pub mod session;
