//! M4-3（LUM-1474）：chat 面**共享基建** —— 请求上下文、时间戳 / UUID 解析、请求体解码。
//!
//! 为什么放在 `session` 的私有子模块里（`chat/mod.rs` 由 M4-0 anchor 冻结、切片不改）：
//! `message`（M4-3）与 `bar`（M4-3）都要用同一份 scope 与本文件里的编解码助手，而 anchor
//! 预置的目录形态只有 `chat/{session,message,bar,task}.rs` 四个文件 —— 没有 `chat/context.rs`
//! 的落点。把共享件放进 `session.rs` 声明的私有子模块，三个 M4-3 文件就能共用而不碰
//! `mod.rs`；M4-4 的 `task.rs` 若需要，再按 `docs/42` §4.2 的写集矩阵自行复制或上提。
//!
//! 本文件的每个助手都对应一条**上游真值**，逐条列在各自文档注释里。**不要**改成
//! 「看起来更 Rust」的写法（trim / 更宽的时间格式 / 更宽松的 JSON 解码都会改变状态码）。

use std::collections::HashSet;

use axum::body::Bytes;
use axum::http::HeaderMap;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::chat_draft_restore::ChatDraftRestoreRepo;
use mc_repos::chat_message::ChatMessageRepo;
use mc_repos::chat_pinned_agent::ChatPinnedAgentRepo;
use mc_repos::chat_session::{ChatSessionRepo, ChatSessionRow};

use crate::routes::agents::AgentScope;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 错误助手（复用 `routes::agents` 的 `pub(crate)` 版本，语义与全仓一致）
// ---------------------------------------------------------------------------

pub(crate) use crate::routes::agents::{bad_request, forbidden, not_found, repo_err};

/// 上游 `parseUUIDOrBadRequest(w, raw, field)`（`internal/handler/handler.go:662`）：
/// `util.ParseUUID` 就是 `pgtype.UUID.Scan`，**不 trim 空白**，失败文案是 `"invalid " + field`。
///
/// ⚠️ 不要用 `routes::agents::parse_uuid`（它会 `trim()` 并生成另一种文案）：那条是本仓
/// 给路径参数写的宽容版本，与上游文案对不上。
pub(crate) fn parse_uuid_field(raw: &str, field: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(raw).map_err(|_| bad_request(format!("invalid {field}")))
}

/// 上游 `timestampToString`（`internal/util/pgx.go:90`）= Go `time.RFC3339` =
/// **秒精度 + UTC 字面 `Z`**（不是 `RFC3339Nano`，也不是本仓 comments 用的 Micros）。
///
/// `chat_session` / `chat_message` / `chat_draft_restore` 的所有响应时间戳都走它。
pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 上游分页游标的时间格式：Go `time.RFC3339Nano`（`chat.go:1246`）。
///
/// Go 的 `RFC3339Nano` 会**去掉小数末尾的 0**，小数全为 0 时连小数点一起去掉
/// （`t.AppendFormat` 的 `'9'` 语义）。`chrono` 没有等价格式项：`SecondsFormat::Nanos`
/// 固定 9 位、`AutoSi` 只在 0/3/6/9 位之间选，两者都不能复现「尾零裁剪」。
/// ⇒ 这里按 Nanos 渲染后再裁尾零。PG `TIMESTAMPTZ` 的精度是微秒，所以裁完最多 6 位小数。
pub(crate) fn cursor_ts(t: DateTime<Utc>) -> String {
    let rendered = t.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let Some((head, tail)) = rendered.split_once('.') else {
        return rendered;
    };
    let digits = tail.strip_suffix('Z').unwrap_or(tail);
    let trimmed = digits.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{head}Z")
    } else {
        format!("{head}.{trimmed}Z")
    }
}

/// 请求体 → (DTO, 原始字段表)，对齐上游 `json.NewDecoder(r.Body).Decode(&req)`。
///
/// 与 `routes::agents::crud::decode_body` 是**同款本地副本**（各切片各自持有，见
/// `docs/42` §4.2「一个文件一个写集」：那条是 M3-5 的文件，本片不改）。chat 的 6 个写接口
/// 在上游用的是**同一种**解码器，要复现的语义有三条：
///
/// 1. **空体是错误**：Go 的 `Decode` 在空体（或只有空白）上报 `io.EOF` ⇒ 400
///    `invalid request body`。这一条**不能**省：空体若当默认值会退化成 `pin` 接口的
///    200（而不是上游的 400）。非法 JSON、顶层不是对象（数组 / 数字 / 字符串）同归 400。
/// 2. **裸 `null` 不是错误**：Go 反序列化 `null` 到结构体是 no-op（零值）⇒ 继续走各自的
///    分支（`agent_id is required` / `exactly one of …` / 旗标 `false`）。
/// 3. **原始字段表必须留**：返回的 `JsonMap` 是「字段是否存在」与「显式 `null`」的判定依据
///    —— 上游 `json.RawMessage` 在字段存在时非 nil（哪怕值是 `null`），而 `*string` 遇到
///    `null` 仍是 nil。`update_session` 的「恰好一个」判据、`title` 的「非 null 才算存在」
///    都依赖它。
pub(crate) fn decode_body<T: DeserializeOwned + Default>(
    body: &Bytes,
) -> Result<(T, JsonMap<String, JsonValue>), Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Err(bad_request("invalid request body"));
    }
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    if value.is_null() {
        return Ok((T::default(), JsonMap::new()));
    }
    let JsonValue::Object(raw) = value else {
        return Err(bad_request("invalid request body"));
    };
    let typed: T = serde_json::from_value(JsonValue::Object(raw.clone()))
        .map_err(|_| bad_request("invalid request body"))?;
    Ok((typed, raw))
}

/// Go `encoding/json` 对 `float64` 的渲染（上游 `ChatPinnedAgentResponse.Position`）。
///
/// Go 用最短往返表示，整数值渲染成 `1`、`2`；`serde_json` 的 `f64` 会渲染成 `1.0`
/// ⇒ 直接派生 `Serialize` 会和上游差一个 `.0`。这里对整数值改走整数通道。
///
/// 边界：`position` 由 `COALESCE(MAX(position), 0) + 1` 产生，实际取值恒为小的整数；
/// 非整数（理论上不可能，除非有人手工写库）落回 `f64` 渲染，`|v| >= 2^53` 的极端值
/// 与 Go 的指数记法可能不同（不可达）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct GoFloat64(pub(crate) f64);

impl Serialize for GoFloat64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        const EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53
        let value = self.0;
        if value.fract() == 0.0 && value.abs() < EXACT_INT_LIMIT {
            #[allow(clippy::cast_possible_truncation)]
            return serializer.serialize_i64(value as i64);
        }
        serializer.serialize_f64(value)
    }
}

/// 上游 `decodeChatQuickActions`（`chat.go:2072`）：把该行 jsonb 里的 `quick_actions`
/// 解成数组、**截到前 3 条**（`chatQuickActionResponseLimit = 3`）；空 / 非数组 / 解析失败
/// 一律降级成 `[]`（上游先 `len(raw)==0` 短路，再 `Unmarshal` 失败 / `nil` 也返回空数组）。
///
/// 本片把**已经存好的 jsonb 原样透传**：写入侧（M4-4）与上游用同一套结构体，字节已经
/// 与上游一致；再解一遍 `ChatQuickAction` 会把该类型的定义拆成两处、且 M4-4 还要重建
/// 同一个 struct。截顶与降级语义逐字保留。
pub(crate) fn quick_actions_json(stored: &JsonValue) -> JsonValue {
    const RESPONSE_LIMIT: usize = 3;
    let JsonValue::Array(items) = stored else {
        return JsonValue::Array(Vec::new());
    };
    JsonValue::Array(items.iter().take(RESPONSE_LIMIT).cloned().collect())
}

// ---------------------------------------------------------------------------
// 请求上下文
// ---------------------------------------------------------------------------

/// 一次 chat 请求的「workspace + 调用者 + 角色 + 各 chat 仓储」五元组。
///
/// 上游把成员身份判定放在 `RequireWorkspaceMember` 中间件里（`router.go:1948` 那个
/// `r.Group`，**包住全部 chat 路由含 unpin**），把 agent 可见性放在
/// `accessibleAgentIDs` / `canAccessPrivateAgent` / `canInvokeAgent` 里。本仓没有
/// 中间件层，于是把前两者收进 [`AgentScope::resolve`]（成员身份，404 `workspace`）+
/// [`ChatScope::accessible_agent_ids`]，让每个 handler 只写自己的业务分支。
pub(crate) struct ChatScope {
    /// 成员身份 + 角色 + agent 仓储（`can_invoke` / `can_access_private` / `filter_accessible`）。
    pub(crate) agent: AgentScope,
    /// `chat_session` 读写。
    pub(crate) sessions: ChatSessionRepo,
    /// `chat_message` 读取（写面属 M4-4）。
    pub(crate) messages: ChatMessageRepo,
    /// `chat_pinned_agent` 读写（快捷栏）。
    pub(crate) pinned: ChatPinnedAgentRepo,
    /// `chat_draft_restore` 读写。
    pub(crate) drafts: ChatDraftRestoreRepo,
}

impl ChatScope {
    /// 解析 workspace（400 `invalid workspace id`）→ 成员身份（404 `workspace`）→ 各仓储。
    pub(crate) async fn resolve(
        state: &AppState,
        user: AuthUser,
        headers: &HeaderMap,
        query: &std::collections::HashMap<String, String>,
    ) -> Result<Self, Error> {
        let agent = AgentScope::resolve(state, user, headers, query).await?;
        Ok(Self {
            agent,
            sessions: ChatSessionRepo::new(state.db.clone()),
            messages: ChatMessageRepo::new(state.db.clone()),
            pinned: ChatPinnedAgentRepo::new(state.db.clone()),
            drafts: ChatDraftRestoreRepo::new(state.db.clone()),
        })
    }

    /// workspace id（`Id` 形式）。
    pub(crate) fn workspace_id(&self) -> Id {
        self.agent.workspace_id
    }

    /// 调用者 id。
    pub(crate) fn user_id(&self) -> Id {
        self.agent.user_id
    }

    /// 上游 `accessibleAgentIDs`（`agent_access.go:297`）：调用者**可见**的 agent id 集合。
    ///
    /// ⚠️ 用 `include_archived = true`（上游 `ListAllAgents` 是 `WHERE workspace_id = $1
    /// AND kind = 'user'`，**没有** `archived_at IS NULL`）：归档 agent 仍算「可见」，
    /// 因为归档会话必须继续出现在列表里、也能继续被取消 pin。`tasks.rs` 里那处
    /// `list(ws, false)` 是 M3 遗留的另一条口径，别照抄。
    ///
    /// 上游这里失败会写 500 `failed to resolve agent access`；本仓按全仓约定把仓储错误
    /// 折成标准错误信封（见模块头的偏离说明），因此返回 `Error` 交给调用方 `?`。
    pub(crate) async fn accessible_agent_ids(&self) -> Result<HashSet<Uuid>, Error> {
        let all = self
            .agent
            .repo
            .list(self.workspace_id(), true)
            .await
            .map_err(|e| repo_err(e, "agent"))?;
        let ids: Vec<Uuid> = all.iter().map(|a| a.id).collect();
        let targets = self.agent.targets_by_agent(&ids).await?;
        Ok(self
            .agent
            .filter_accessible(all, &targets)
            .into_iter()
            .map(|a| a.id)
            .collect())
    }

    /// 上游 `loadChatSessionForUser`（`chat.go:252`）：解析 id → 本 workspace 取行 → 归属校验。
    ///
    /// 顺序与文案逐字：`invalid chat session id`（400）→ `chat session not found`（404）→
    /// `not your chat session`（403）。**没有** agent 可见性门 —— `DeleteChatSession` 与
    /// draft-restore 两条路径刻意用这个较弱的门（「用户自己的草稿不能因为丢了 agent 权限
    /// 就永久滞留服务端」）。
    pub(crate) async fn load_session_for_user(
        &self,
        raw_session_id: &str,
    ) -> Result<ChatSessionRow, Error> {
        let id = parse_uuid_field(raw_session_id, "chat session id")?;
        let session = self
            .sessions
            .get_in_workspace(id, self.workspace_id().0)
            .await
            .map_err(|e| repo_err(e, "chat session"))?
            .ok_or_else(|| not_found("chat session"))?;
        if !session.is_creator(self.user_id()) {
            return Err(forbidden("not your chat session"));
        }
        Ok(session)
    }

    /// 上游 `gateChatSessionForUser`（`chat.go:281`）：归属校验 + 私有 agent 的**读**门。
    ///
    /// `GetAgent`（按主键，**不带** workspace 过滤）失败 → 404 `agent not found`；
    /// `canAccessPrivateAgent` 失败 → 403 `you do not have access to this agent`。
    /// 用 [`mc_repos::agent::AgentRepo::get`]（= 上游 `GetAgent`），不是
    /// `get_in_workspace` —— 后者多一层 `kind = 'user'`，会让 system agent（agent-builder
    /// 会话）的会话在读到它之前就先 404。
    pub(crate) async fn gate_session_for_user(
        &self,
        raw_session_id: &str,
    ) -> Result<ChatSessionRow, Error> {
        let session = self.load_session_for_user(raw_session_id).await?;
        let agent = self
            .agent
            .repo
            .get(session.agent_id())
            .await
            .map_err(|e| repo_err(e, "agent"))?;
        let targets = self.agent.targets_of(agent.id()).await?;
        if !self.agent.can_access_private(&agent, &targets) {
            return Err(forbidden("you do not have access to this agent"));
        }
        Ok(session)
    }

    /// 上游 `gatePublicChatSessionForUser`（`chat.go:306`）：在上面两道门之后再加
    /// **成员可见投影**边界（`GetPublicChatSessionInWorkspace`）—— 只含渠道控制记录
    /// （`channel_command`）的会话不算公开会话，缓存住一个隐藏会话 id 也复活不了它。
    ///
    /// 只读面（list messages / update / pin / read）走这道门；清理面（archive / delete /
    /// draft-restore）用较弱的门，这样空的渠道会话也还能被归档或删除。
    pub(crate) async fn gate_public_session_for_user(
        &self,
        raw_session_id: &str,
    ) -> Result<ChatSessionRow, Error> {
        let session = self.gate_session_for_user(raw_session_id).await?;
        let visible = self
            .sessions
            .is_public_in_workspace(session.id, session.workspace_id)
            .await
            .map_err(|e| repo_err(e, "chat session"))?;
        if !visible {
            return Err(not_found("chat session"));
        }
        Ok(session)
    }
}
