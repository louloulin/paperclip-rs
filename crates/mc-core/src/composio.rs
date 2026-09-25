//! composio 领域类型 —— M8-0 anchor（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §5）。
//!
//! 本文件是 M8 里 **composio** 一侧的领域层：`user_composio_connection` 表的行投影 +
//! toolkit / auth-config 的解析形状。SDK 客户端（HTTP + `x-api-key`）、服务、state HMAC
//! 与 overlay 构建在 `mc-composio`，仓储在 `mc_repos::composio`，HTTP 面在
//! `mc_http::routes::composio`。
//!
//! # 为什么 composio 是**独立 crate**（`docs/61` §2.2 判据 1 的第二半）
//!
//! 上游 `router.go:1252` 逐字 `h.TaskService.Composio = svc` ⇒ composio 服务**同时**被
//! ① 5 条 HTTP 路由、② task 派发服务（算 per-task MCP overlay）使用。生产者必须在
//! `mc-http` 与派发层都能拿到 ⇒ 独立 crate（塞进 `mc-http` 会让依赖方向反向）。
//!
//! # 归属：连接属于**用户**，不属于 workspace（`docs/61` §1.1 第 4 簇）
//!
//! 4 条会话级路由（`/api/integrations/composio/*`）在 Auth 组内、**无 workspace 上下文**；
//! `user_composio_connection.user_id` 是唯一 owner 列。任何「按 workspace 查连接」的写法
//! 都是越权。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// `user_composio_connection` 行投影（`migrations/upstream/127_user_composio_connection.up.sql`）。
///
/// ⚠️ `connected_account_id` / `composio_user_id` 是**外部标识**（非密钥），可以进投影；
/// 真正的 bearer 只在 `mc-composio::service` 的会话 URL 里，**不进**本结构
/// （`docs/61` §2.4 的 redaction 第 1 条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposioConnection {
    pub id: Id,
    pub user_id: Id,
    pub toolkit_slug: String,
    pub auth_config_id: String,
    pub connected_account_id: String,
    pub composio_user_id: String,
    pub status: ComposioConnectionStatus,
    pub connected_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 连接状态（`user_composio_connection.status`，默认 `active`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComposioConnectionStatus {
    Active,
    Expired,
    Revoked,
}

impl ComposioConnectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    /// `from_str` 是对**存储/wire 值**的显式解析（未知值 ⇒ `None`）。
    /// 刻意**不**实现 `std::str::FromStr`：那个 trait 的错误类型会诱导调用侧用 `?`
    /// 掩盖「未知值」，而上游语义是「未知 ⇒ 不认识的 kind」，必须显式处理。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "expired" => Some(Self::Expired),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// toolkit 目录项（`GET /api/integrations/composio/toolkits` 的领域形状）。
///
/// 上游对 toolkit 目录是**动态解析**：auth-config 未配置的 toolkit **不出现**
/// （`docs/61` §6.5 的 M8-6 专属 `DoD`）⇒ 本结构把「哪些 auth config 可用」当输入，
/// 由 M8-6 的 `catalog.rs` 决定可见集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposioToolkit {
    pub slug: String,
    pub name: String,
    /// 该 toolkit 下可用的 auth config id（去掉未配置的之后）。
    pub auth_config_ids: Vec<String>,
    pub logo_url: Option<String>,
}
