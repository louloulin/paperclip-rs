//! MCP 服务器库领域类型 —— M8-0 anchor（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §2.3 / §5）。
//!
//! 本文件是 M8 里 **workspace MCP 服务器库**的领域层：`workspace_mcp_server` /
//! `agent_mcp_server` 两张表的行投影 + transport 枚举 + per-task overlay 的容器形状。
//!
//! # 这不是 remote MCP 客户端（`docs/61` §2.3 的「不得重复实现」清单）
//!
//! `crates/mc-mcp/**` 是 **remote MCP 客户端**（JSON-RPC over HTTP + OAuth，M6 已交付）；
//! `crates/mc-daemon/src/mcp/**` 是 daemon 侧**运行时** MCP（读 provider 原生配置 + 本地合并）。
//! 本文件与它们**零交集**：M8 只补「workspace 服务器库的 CRUD + agent 绑定」这一层，
//! 并**复用** daemon 侧已经实现的「runtime 层做底、agent 层同名覆盖」语义（见
//! [`overlay`] 的合并契约）。**禁止**在 `mc-mcp` 里加库/仓储、**禁止**复制 daemon 的去敏逻辑。
//!
//! # write-only 硬约束（`docs/61` §2.7 第 5 条）
//!
//! `workspace_mcp_server.config` 是**含密钥的** JSONB（`urls` / `headers` / `env`），
//! 上游注释逐字：「the stored entries are **write-only**」⇒ 响应 DTO **永不**含值字段。
//! 领域层保留 `config` 原值（写侧要它），脱敏是 HTTP 层的事。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::id::Id;
use super::timestamp::Timestamp;

pub mod overlay;

pub use overlay::{merge_task_overlay, McpOverlayError};

/// `workspace_mcp_server` 行投影（`migrations/upstream/315_workspace_mcp_server.up.sql`）。
///
/// ⚠️ 手写 `Debug`（**[`WorkspaceMcpServer::fmt`]**）：`config` 可能含第三方凭证
/// （`headers` / `env` 的值）⇒ 打印时**只列 transport 与是否有 env/headers**，不吐内容
/// （`docs/61` §2.4 的 redaction 第 1 条）。
#[derive(Clone, PartialEq)]
pub struct WorkspaceMcpServer {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    /// 原始 JSONB 条目（写侧要它；读侧由 DTO 剥掉值字段）。
    pub config: Value,
    pub created_by: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl std::fmt::Debug for WorkspaceMcpServer {
    /// 手写脱敏：`config` **绝不**整体打印。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceMcpServer")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("name", &self.name)
            .field("transport", &self.transport())
            // 值字段（可能含第三方凭证）**绝不**整体打印。
            .field("config", &"<redacted, write-only>")
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl WorkspaceMcpServer {
    /// 条目声明的 transport（`config.type` 或 `config.transport`，两者上游都出现过）。
    pub fn transport(&self) -> Option<McpTransport> {
        McpTransport::from_config(&self.config)
    }
}

/// `agent_mcp_server` 行投影 —— agent 对 workspace 服务器的绑定与开关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpBinding {
    pub agent_id: Id,
    pub server_id: Id,
    pub enabled: bool,
    pub created_at: Timestamp,
}

/// MCP 条目的 transport（`mcp_config` 里 `type` / `transport` 字段的规范化三值）。
///
/// 上游 `mcp_config` 不强类型化（就是一个 JSON 对象），但所有 runtime 消费的都是这三种；
/// anchor 把它钉成枚举，让 M8-3 的**校验反例**（非法 transport）有一个单一判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    /// 本地子进程（`command` + `args`）。
    Stdio,
    /// 远端 SSE。
    Sse,
    /// 远端流式 HTTP。
    Http,
}

impl McpTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Sse => "sse",
            Self::Http => "http",
        }
    }

    /// `from_str` 是对**存储/wire 值**的显式解析（未知值 ⇒ `None`）。
    /// 刻意**不**实现 `std::str::FromStr`：那个 trait 的错误类型会诱导调用侧用 `?`
    /// 掩盖「未知值」，而上游语义是「未知 ⇒ 不认识的 kind」，必须显式处理。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "stdio" => Some(Self::Stdio),
            "sse" => Some(Self::Sse),
            "http" | "streamable-http" | "streamable_http" => Some(Self::Http),
            _ => None,
        }
    }

    /// 从条目 JSON 读 transport：先看 `type`，再看 `transport`；都没有则按有无 `command`
    /// 判 `Stdio`（这是 Claude 风格的默认）。
    pub fn from_config(config: &Value) -> Option<Self> {
        for key in ["type", "transport"] {
            if let Some(raw) = config.get(key).and_then(Value::as_str) {
                if let Some(kind) = Self::from_str(raw) {
                    return Some(kind);
                }
            }
        }
        if config.get("command").is_some() {
            return Some(Self::Stdio);
        }
        if config.get("url").is_some() {
            return Some(Self::Http);
        }
        None
    }
}
