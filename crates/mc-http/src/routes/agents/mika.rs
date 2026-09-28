//! `POST /api/agents/mika` —— 内置 agent（Mika）的供给 + onboarding 会话
//! （**写者 M9-7**）。
//!
//! 上游对照：`server/internal/handler/mika_agent.go:82-252`
//! （`CreateMikaAgent` / `resolveMikaAgent` / `writeMikaAgentResponse`，
//! `f41fae6b`）。一条路由，建档面是 `docs/fixtures/m9-declared-routes.tsv:118`
//! 声明的**单形态** `POST /api/agents/mika`（**没有**尾斜杠别名，见
//! `docs/62-M9-PLAN.md` §3.3 与本片 `docs/32` §58）。
//!
//! # 为什么 agent + 会话**一起**返回
//!
//! 上游 `mikaAgentResponse` 的注释（`mika_agent.go:61-68`）：两者必须原子地产出。
//! 客户端过去是「先列会话、没匹配再建」—— 那是一次**无保护**的 check-then-insert：
//! `LockWorkspaceForChatSessionCreate` 取的正是 `FOR KEY SHARE`（**只**与 workspace
//! 删除的 `FOR UPDATE` 互斥，**不**在 creator 之间互斥），所以两个标签页各建一个会话、
//! 各自拿到一次所谓"幂等"的 kickoff。
//!
//! # 上游的两段事务、两次锁（逐字照搬，别合并）
//!
//! - **供给**（`resolveMikaAgent`）：per-workspace `pg_advisory_xact_lock("mika:<ws>")`
//!   → 锁内复查 → INSERT → 写 invoke 允许列表 → COMMIT；
//! - **会话**（`getOrCreateMikaSession`）：**另开**自己的事务与
//!   `pg_advisory_xact_lock("mika-session:<ws>:<user>")`。
//!
//! 上游 `mika_agent.go:99-103` 点名了为什么不能合：会话的 get-or-create 自带一把锁，
//! 在 workspace 供给锁还持着时跑它，会把所有成员的会话排在**一把与会话无关**的锁后面。
//! 本片在 `MikaRepo` 里也是两个独立方法、两次独立事务。
//!
//! # 三处**有意偏离**（登记在 `docs/32-M3-DAEMON-FACE.md` §9.28）
//!
//! 1. **WS 广播不播**：`created` 时上游发 `protocol.EventAgentCreated`（+ actor
//!    解析）。本仓 agent 面整体不广播 WS 事件（M3-7 的活，见
//!    `routes/agents/crud.rs:12`）。上游还特意把它排在会话那步**之前**（"agent 已经
//!    提交了，别人的列表该知道"）——本片没有这一步。
//! 2. **`ReconcileAgentStatus` 不调**：上游在 `runtime.status == "online"` 时重算一次
//!    agent 状态。本仓的 `AgentRepo::runtime_binding` 只投影 5 个绑定相关列，**不含
//!    `status`**（`routes/agents.rs:224-236`），在线探测属 M3-4 的面 ⇒ 本片建出来的
//!    agent 保持列默认值 `offline`。
//! 3. **成员校验提前**：`AgentScope::resolve` 一上来就做 workspace 成员判定（非成员 →
//!    404 `workspace`），而上游的 `h.workspaceMember` 在「已供给」的快速路径**之后**才跑
//!    —— 意味着上游能把 Mika 发给一个非本 workspace 的成员。本仓一律 fail-closed，与
//!    本面其余 16 条路由同立场（`routes/agents.rs:41-46`）。
//!
//! 另外上游 `systemInstructionsFor`（`mika_agent.go:320-328`）产出的
//! `system_instructions` 字段本仓**不产出** —— 那是 `AgentDto` 全域的既有偏离
//! （`dto.rs:17`：产品内置 prompt 表不在本仓）。
//!
//! # 不可铸造（DoD 第 2 条）
//!
//! 请求体只有 `runtime_id` / `language` / `model` / `session_title` 四个字段，
//! **serde 默认忽略未知键** ⇒ 客户端多传 `kind` / `system_key`（或 `name` /
//! `avatar_url` / `visibility` / `permission_mode` / `max_concurrent_tasks`）
//! **一律被忽略**，服务端落的是 [`MIKA_*`] 常量。**不要**为此加一层显式过滤：
//! 机制在入参结构体与 `MikaProvision` 的形状上，多加一层只会引入上游没有的 400。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use mc_errors::Error;
use mc_repos::agent::mika::{MikaProvision, MikaRepo};
use mc_repos::chat_session::ChatSessionRow;

use super::dto::AgentDto;
use super::{bad_request, parse_uuid, AgentScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 上游 `mikaAgentDescriptions`（`mika_agent.go:40-45`）。
///
/// **用户可见文案**，所以本地化（`instructions` 不是：它随二进制走，不落这一列）。
/// 与 `instructions` 不同，`description` 是 owner 可改字段，供给之后产品也不收回。
/// 键集合必须与 `mc-chat` 的 `onboarding::LANGUAGES` **逐字相同**（`en`/`zh`/`ko`/`ja`），
/// 否则会出现「语言校验过了但查不到文案」的空档 —— 单测
/// `every_whitelisted_language_has_a_description` 钉住这一点。
const MIKA_DESCRIPTIONS: [(&str, &str); 4] = [
    (
        "en",
        "Your workspace Chief of Staff. Mika turns goals into issues, coordinates agents, \
         and helps build reusable workflows.",
    ),
    (
        "zh",
        "你的工作区 Chief of Staff。Mika 会把目标转化为任务、协调智能体，并帮你建立可复用的工作流。",
    ),
    (
        "ko",
        "워크스페이스의 Chief of Staff입니다. Mika가 목표를 태스크로 구체화하고 에이전트를 조율하며 \
         재사용 가능한 워크플로 구성을 돕습니다.",
    ),
    (
        "ja",
        "ワークスペースの Chief of Staff。Mika は目標をタスクに落とし込み、エージェントを調整し、\
         再利用できるワークフローづくりを支援します。",
    ),
];

/// 上游 `mika_agent.go:110-113` 的 400 文案（逐字）。
const LANGUAGE_ERROR: &str = "language must be en, zh, ko, or ja";

/// 上游 `mika_agent.go:138-140` 的 400 文案（逐字）。
const RUNTIME_NOT_FOUND: &str = "runtime not found in this workspace";

/// 上游 `mika_agent.go:141-145` 的 403 文案（逐字）。
const RUNTIME_FORBIDDEN: &str = "you cannot bind an agent to this runtime";

/// 上游 `mika_agent.go:123` / `:170` 的 500 文案（逐字）。
const LOOKUP_FAILED: &str = "failed to look up the workspace agent";

/// 上游 `mika_agent.go:196-198` 的 500 文案（逐字）。
const CREATE_FAILED: &str = "failed to create the workspace agent";

/// 上游 `mika_agent.go:241-242` 的 500 文案（逐字）。
const SESSION_FAILED: &str = "failed to open the Mika conversation";

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 上游 `createMikaAgentRequest`（`mika_agent.go:47-59`）。
///
/// Go 的 `encoding/json` 对**未知键**不报错（没有 `DisallowUnknownFields`）⇒ serde 的
/// 默认行为（忽略未知键）就是逐字等价。`kind` / `system_key` 就这样被**忽略**。
///
/// `Model` 语义：空 = 「用 runtime 的默认模型」，也就是每个没有 per-agent 模型支持的
/// 部署本来就拿到的结果。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CreateMikaAgentRequest {
    /// 目标 runtime（必填；不是 uuid ⇒ 400）。
    #[serde(default)]
    runtime_id: Option<String>,
    /// `en` / `zh` / `ko` / `ja`（白名单外 ⇒ 400）。
    #[serde(default)]
    language: Option<String>,
    /// Mika 该跑的 runtime 模型（可空）。
    #[serde(default)]
    model: Option<String>,
    /// **只是**一个标签：会话的身份是 (workspace, member, Mika)，所以后来传一个不同的
    /// 标题会**复用**既有会话，而不是开出第二条。
    #[serde(default)]
    session_title: Option<String>,
}

// ---------------------------------------------------------------------------
// 响应
// ---------------------------------------------------------------------------

/// 上游 `mikaAgentResponse`（`mika_agent.go:69-72`）：agent 内联（Go 的嵌入 ⇒
/// `flatten`），外加 `onboarding_session`。
///
/// `onboarding_session` 的 `omitempty` 在上游实际是**恒有**的：拿不到会话时上游回 500
/// 而不是「成功但没有会话」（`mika_agent.go:234-242`：报成功而省掉会话，比失败更糟 ——
/// 调用方分不清「bootstrap 做了一半」和「做完了」，而那些引导用户补完的面是按 agent
/// 触发的，那个 agent 现在已经存在了）。所以本仓同样**没有** `skip_serializing_if`。
#[derive(Debug, Serialize)]
pub(crate) struct MikaAgentDto {
    /// 上游的内嵌 `AgentResponse`（字段与 `AgentDto` 逐字相同，平铺到同一层）。
    #[serde(flatten)]
    pub agent: AgentDto,
    /// 调用方的 onboarding 会话。
    pub onboarding_session: MikaChatSessionDto,
}

/// `ChatSessionResponse` 的**单会话**形态（上游 `chatSessionToResponse`），
/// 与 `routes/chat/session.rs` 的 `ChatSessionDto::from_row` 逐字同形。
///
/// 这里**重新声明**而不复用 `chat::session::ChatSessionDto`：那个类型是
/// `pub(super)`，而本片唯一的越界写授权覆盖不到 `routes/chat/session.rs`
/// （见本片 `docs/32` §58）。字段顺序 / `has_unread` / `unread_count` /
/// `last_message` 三个派生位与它保持一致。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MikaChatSessionDto {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: String,
    pub creator_id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub status: String,
    pub has_unread: bool,
    pub unread_count: i32,
    pub last_message: Option<JsonValue>,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl MikaChatSessionDto {
    /// 单会话形态：`has_unread=false` / `unread_count=0` / `last_message=null`。
    fn from_row(row: &ChatSessionRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            agent_id: row.agent_id.to_string(),
            creator_id: row.creator_id.to_string(),
            project_id: row.project_id.map(|id| id.to_string()),
            title: row.title.clone(),
            status: row.status.clone(),
            has_unread: false,
            unread_count: 0,
            last_message: None,
            pinned: row.is_pinned(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `POST /api/agents/mika`（上游 `CreateMikaAgent`）。
///
/// 供给好了就 **201**，拿到既有的就 **200**（上游 `mika_agent.go:247-251`）。
pub(crate) async fn create_mika_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<MikaAgentDto>)> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let req: CreateMikaAgentRequest = decode_body(&body)?;

    // 语言先判（上游 `mika_agent.go:110-114`）：它在任何 DB 动作之前，且是
    // 400 里唯一一条纯用户输入的报错。**白名单只读复用** `mc-chat` 的
    // `onboarding::language_name`（M4-4），不复制一份。
    let language = req.language.as_deref().unwrap_or("");
    if mc_chat::onboarding::language_name(language).is_none() {
        return Err(bad_request(LANGUAGE_ERROR).into());
    }
    let description = description_for(language);

    // 快速路径（上游 `mika_agent.go:121-131`）：**已供给**就直接交回同一个 agent ——
    // 重试、第二个标签页、重跑 onboarding 都不可能造出重复品。
    //
    // 🔴 幂等**按 `system_key` 判，不按名字**（`mika_agent.go:76-81`）：名字是 owner
    // 可改字段，而迁移 172 的唯一索引盖的是
    // `(workspace_id, owner_id, runtime_id, system_key)`，换个 runtime 或换个 owner
    // 仍能再插一个。真正成立「一个 workspace 一个 Mika」的是下面那次查询。
    let mika = MikaRepo::new(state.db.clone());
    if let Some(existing) = mika
        .find_by_system_key(scope.workspace_id)
        .await
        .map_err(|_| server_error(LOOKUP_FAILED))?
    {
        return Ok(finish(&scope, &mika, existing, &req, false).await?);
    }

    // 还没供给 ⇒ 这才是本次调用该干的事，走 runtime 绑定校验（上游 `:133-145`）。
    //
    // 🔴 逐字走 `AgentRepo::runtime_binding`（不包 `AgentScope::runtime_binding`）：
    // 后者把失败翻成 `invalid runtime_id`（那是 `POST /api/agents/` 的文案，
    // `routes/agents.rs:411-417`），本条的**上游原文**是
    // `runtime not found in this workspace`（`mika_agent.go:139`）。复用同一处
    // 校验查询，只换文案。
    let runtime_id = req.runtime_id.as_deref().unwrap_or("");
    let runtime_uuid = parse_uuid(runtime_id, "runtime_id")?;
    let runtime = scope
        .repo
        .runtime_binding(scope.workspace_id, runtime_uuid)
        .await
        .map_err(|_| bad_request(RUNTIME_NOT_FOUND))?;
    if !scope.can_use_runtime(&runtime) {
        return Err(Error::Forbidden {
            message: RUNTIME_FORBIDDEN.to_string(),
        }
        .into());
    }

    let provision = MikaProvision {
        workspace_id: scope.workspace_id,
        owner_id: scope.user_id.0,
        runtime_id: runtime.id,
        runtime_mode: runtime.runtime_mode.clone(),
        description,
        // 上游 `strings.TrimSpace(req.Model)`：全空白 ⇒ `NULL`。
        model: req
            .model
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
    };
    let provisioned = mika
        .provision(&provision)
        .await
        .map_err(|_| server_error(CREATE_FAILED))?;

    Ok(finish(&scope, &mika, provisioned.agent, &req, provisioned.created).await?)
}

/// 上游 `writeMikaAgentResponse`：响应组装 + onboarding 会话的 get-or-create。
///
/// 上游把广播事件夹在「组装响应」与「取会话」之间；本片没有广播（见文件头偏离 1），
/// 所以这里是「组装 → 取会话 → 出参」两步。
async fn finish(
    scope: &AgentScope,
    mika: &MikaRepo,
    agent: mc_repos::agent::AgentRow,
    req: &CreateMikaAgentRequest,
    created: bool,
) -> Result<(StatusCode, Json<MikaAgentDto>), Error> {
    // 上游 `enrichAgentResponseWithTargets` 失败只 `log.Warn`、不失败响应
    // （`mika_agent.go:221-223`）。本仓的白名单读失败是真失败（`repo_err`），
    // 与 `routes/agents` 其余端点同立场 —— agent 刚供给出来，它的允许列表不可能读不到。
    let targets = scope.targets_of(agent.id()).await?;

    let session = mika
        .get_or_create_onboarding_session(
            scope.workspace_id,
            scope.user_id.0,
            agent.id,
            req.session_title.as_deref().unwrap_or(""),
        )
        .await
        .map_err(|_| server_error(SESSION_FAILED))?;

    let dto = MikaAgentDto {
        agent: AgentDto::from_row(&agent, scope, &targets),
        onboarding_session: MikaChatSessionDto::from_row(&session),
    };
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(dto),
    ))
}

/// 上游 `mika_agent.go:110-113` 的文案查表；键集合由
/// `every_whitelisted_language_has_a_description` 钉住与 `mc-chat` 白名单一致。
fn description_for(language: &str) -> String {
    MIKA_DESCRIPTIONS
        .iter()
        .find(|(code, _)| *code == language)
        .map_or_else(String::new, |(_, text)| (*text).to_string())
}

/// 上游 `json.NewDecoder(r.Body).Decode(&req)` 的 400 分支：body 不是 JSON **对象**
/// （标量 / 数组）⇒ 400 `invalid request body`。
///
/// ⚠️ 必须显式要求「是对象」：serde 派生的结构体同时实现了 `visit_seq`，所以
/// `[]` / `["a","b"]` 会被当成**位置化**字段填进 `Option` 位而**不报错**（缺位补
/// `None`）—— 而 Go 的 `json.Decode` 对数组是硬报错的。
/// `null` / 空 body 则按 Go 的 no-op 退化成「所有字段缺失」（于是 `runtime_id` 为空
/// ⇒ `parse_uuid("")` ⇒ 400 `runtime_id must be a valid uuid`，与上游
/// `parseUUIDOrBadRequest` 的空串分支同）。
fn decode_body(body: &Bytes) -> Result<CreateMikaAgentRequest, Error> {
    if body.is_empty() {
        return Ok(CreateMikaAgentRequest::default());
    }
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    if value.is_null() {
        return Ok(CreateMikaAgentRequest::default());
    }
    if !value.is_object() {
        return Err(bad_request("invalid request body"));
    }
    serde_json::from_value(value).map_err(|_| bad_request("invalid request body"))
}

/// 上游那一串 `writeError(w, http.StatusInternalServerError, ...)`。
fn server_error(message: &'static str) -> Error {
    Error::Internal(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_repos::agent::mika::{
        MIKA_AVATAR_URL, MIKA_MAX_CONCURRENCY, MIKA_PERMISSION_MODE, MIKA_SYSTEM_KEY,
        MIKA_VISIBILITY,
    };

    #[test]
    fn every_whitelisted_language_has_a_description() {
        // 键集合必须与 `mc-chat` 的白名单**逐字相同**，否则会出现「语言校验过了但
        // 查不到文案」的空档（`description` 落空串 ⇒ agent 描述丢失）。
        let codes: Vec<&str> = MIKA_DESCRIPTIONS.iter().map(|(code, _)| *code).collect();
        let whitelisted: Vec<&str> = mc_chat::onboarding::LANGUAGES
            .iter()
            .map(|(code, _)| *code)
            .collect();
        assert_eq!(codes, whitelisted);
        for (_, text) in MIKA_DESCRIPTIONS {
            assert!(!text.is_empty());
        }
        assert!(description_for("fr").is_empty());
        assert!(!description_for("en").is_empty());
    }

    #[test]
    fn language_whitelist_is_read_from_mc_chat_not_duplicated() {
        assert_eq!(mc_chat::onboarding::language_name("en"), Some("English"));
        assert_eq!(mc_chat::onboarding::language_name("fr"), None);
        assert_eq!(mc_chat::onboarding::language_name(""), None);
    }

    #[test]
    fn repo_constants_agree_with_mc_chat_identity() {
        // 「按 `system_key` 判身份」这条不变式跨两个 crate：仓储层与 onboarding 层
        // 各自持有一份 `mika` 字面量，这里钉住它们不会漂。
        assert_eq!(MIKA_SYSTEM_KEY, mc_chat::onboarding::SYSTEM_KEY);
        assert_eq!(MIKA_AVATAR_URL, "emoji:\u{1F984}");
        assert_eq!(MIKA_MAX_CONCURRENCY, 3);
        assert_eq!(MIKA_VISIBILITY, "workspace");
        assert_eq!(MIKA_PERMISSION_MODE, "public_to");
    }

    #[test]
    fn unknown_body_fields_are_ignored_not_rejected() {
        // 「客户端不能铸造」：多传 `kind` / `system_key` / `name` / `avatar_url`
        // 既不 400 也不落库（入参结构体上就没有这些字段）。
        let body = Bytes::from(
            r#"{"runtime_id":"11111111-1111-1111-1111-111111111111","language":"en",
                "kind":"system","system_key":"mika","name":"Impostor",
                "avatar_url":"emoji:x","visibility":"private",
                "permission_mode":"private","max_concurrent_tasks":99}"#,
        );
        let req = decode_body(&body).expect("多传字段应被忽略");
        assert_eq!(req.language.as_deref(), Some("en"));
        assert_eq!(
            req.runtime_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
    }

    #[test]
    fn malformed_body_is_400() {
        assert!(decode_body(&Bytes::from_static(b"[]")).is_err());
        assert!(decode_body(&Bytes::from_static(b"\"x\"")).is_err());
        assert!(decode_body(&Bytes::from_static(b"runtime_id")).is_err());
    }
}
