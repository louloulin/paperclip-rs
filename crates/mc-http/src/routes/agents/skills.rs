//! `/api/agents/{id}/skills*` 与 `/api/agents/{id}/runtime-skills/enabled`（M6-4，6 条注册键）。
//!
//! 上游对照：
//! - `internal/handler/skill.go` L2586–2812（`ListAgentSkills` / `SetAgentSkills` /
//!   `AddAgentSkills` / `SetAgentSkillEnabled` / `RemoveAgentSkill` / `writeUpdatedAgentSkills`）
//! - `internal/handler/agent_runtime_skills.go:202`（`SetAgentRuntimeSkillEnabled`）
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/agents/:id/skills` | GET, PUT | `router.go:2193-2194` |
//! | `/api/agents/:id/skills/add` | POST | `router.go:2195` |
//! | `/api/agents/:id/skills/:skill_id/enabled` | PUT | `router.go:2199` |
//! | `/api/agents/:id/skills/:skill_id` | DELETE | `router.go:2201` |
//! | `/api/agents/:id/runtime-skills/enabled` | PUT | `router.go:2200` |
//!
//! ## 三处刻意与上游同形 / 不同的地方
//!
//! 1. **读面没有可见性门**：上游这 5 条 skill 路由只用 `loadAgentForUser`（workspace 成员 +
//!    `kind='user'`），**不**调 `canAccessPrivateAgent` —— 私有 agent 的绑定列表对任何成员可读。
//!    本文件照抄（`AgentScope::load_agent` 就是这个语义），**不**加 `require_can_access_private`。
//!    写面 4 条一律 `require_can_manage`（owner 或 workspace owner/admin），失败 403。
//! 2. **坏 uuid → 400 的话术**沿本仓 M3-5 约定（`parse_uuid` 产 `<field> must be a valid uuid`），
//!    上游是 `invalid <field>`；状态码一致，只有文案不同（同 `docs/40` §5 的既有偏离）。
//! 3. **写后响应**：上游 `writeUpdatedAgentSkills` 顺带 `publish(agent:status)`。本仓 WS 面在
//!    M3 之外，本片只落 HTTP 200 + 最新列表（列表与上游同一句 SQL ⇒ 响应体逐字同形）。
//!
//! 哈希/清单口径不在这里 —— bundle digest 的唯一实现点是 `mc_core::skill`
//! （见 `crates/mc-core/src/skill.rs`），本文件只读 `agent_skill` 与 `agent.disabled_runtime_skills`。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::AgentRow;
use mc_repos::skill::binding::{AgentSkillSummaryRow, SkillBindingRepo};
use mc_repos::skill::read::SkillRepo;
use mc_repos::RepoError;

use super::dto::{decode_disabled_runtime_skills, DisabledRuntimeSkillDto};
use super::{bad_request, not_found, parse_uuid, repo_err, AgentScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::skills::helpers::SkillSummaryDto;
use crate::state::AppState;

/// 上游 `maxRuntimeSkillKeyLength`（`agent_runtime_skills.go:17`）。
const MAX_RUNTIME_SKILL_KEY_LENGTH: usize = 512;

/// 上游 `writeError(w, http.StatusConflict, …)`。
fn conflict(message: &str) -> Error {
    Error::Conflict {
        message: message.to_string(),
    }
}

fn binding_repo(state: &AppState) -> SkillBindingRepo {
    SkillBindingRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// 响应投影
// ---------------------------------------------------------------------------

/// `AgentSkillSummaryRow` → `SkillSummaryResponse`（上游 `skillSummaryToResponse` +
/// `resp[i].Enabled = &s.Enabled`）。
///
/// 复用 M6-2 的 [`SkillSummaryDto`] 而不新建 DTO：`/api/skills` 与 `/api/agents/{id}/skills`
/// 在上游是同**一个** `SkillSummaryResponse`，两侧同 DTO 才能保证线格式不漂移。
/// 这里手写构造（不调 `SkillSummaryDto::from_summary_row`）是因为手上的行类型不同
/// （agent 面多一列 `ask.enabled`、少一次 `labels` 批量关联）；`config` 的
/// `null ⇒ {}` 规范化与上行 `decodeSkillConfig` 同款。
fn skill_summary(row: &AgentSkillSummaryRow) -> SkillSummaryDto {
    SkillSummaryDto {
        id: row.id.to_string(),
        workspace_id: row.workspace_id.to_string(),
        name: row.name.clone(),
        description: row.description.clone(),
        config: if row.config.is_null() {
            serde_json::json!({})
        } else {
            row.config.clone()
        },
        created_by: row.created_by.map(|v| v.to_string()),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        enabled: Some(row.enabled),
        labels: None,
    }
}

/// 上游 `writeUpdatedAgentSkills` 的响应半段（列表 + 200）。
async fn updated_agent_skills(
    state: &AppState,
    agent_id: Id,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let rows = binding_repo(state)
        .list_agent_skill_summaries(agent_id)
        .await
        .map_err(|e| repo_err(e, "agent skill"))?;
    Ok(Json(rows.iter().map(skill_summary).collect()))
}

// ---------------------------------------------------------------------------
// GET /api/agents/{id}/skills
// ---------------------------------------------------------------------------

/// 上游 `ListAgentSkills`（含被停用的绑定；`content` 不出库）。
pub(super) async fn list_agent_skills(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    updated_agent_skills(&state, agent.id()).await
}

// ---------------------------------------------------------------------------
// PUT /api/agents/{id}/skills + POST /api/agents/{id}/skills/add
// ---------------------------------------------------------------------------

/// 上游 `SetAgentSkillsRequest` / `AddAgentSkillsRequest`（两个结构体逐字相同）。
#[derive(Debug, Default, Deserialize)]
struct SkillIdsRequest {
    /// 缺字段与 `null` 都退化成空列表（Go 侧仍是 nil slice，不报错）。
    #[serde(default)]
    skill_ids: Option<Vec<String>>,
}

/// 上游 `parseUUIDSliceOrBadRequest(w, req.SkillIDs, "skill_ids")`。
fn decode_skill_ids(body: &Bytes) -> Result<Vec<Uuid>, Error> {
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    if value.is_null() {
        // Go 对 `null` body 解出零值结构体（`skill_ids` 为空）而不是报错。
        return Ok(Vec::new());
    }
    let req: SkillIdsRequest =
        serde_json::from_value(value).map_err(|_| bad_request("invalid request body"))?;
    req.skill_ids
        .unwrap_or_default()
        .iter()
        .map(|raw| parse_uuid(raw, "skill_ids"))
        .collect()
}

/// 上游 `validateAgentSkillIDsInWorkspace`：去重后逐个查本 workspace，任一缺失 → 404 `skill`。
///
/// 上游把**任何**错误（含 DB 故障）都折成 404 `skill not found`，这里照抄 —— 顺带避免
/// 「存在但属于别的 workspace」与「不存在」给出可区分的答案。
async fn validate_ids_in_workspace(
    state: &AppState,
    workspace_id: Id,
    ids: &[Uuid],
) -> Result<(), Error> {
    let repo = SkillRepo::new(state.db.clone());
    let mut seen: HashSet<Uuid> = HashSet::new();
    for id in ids {
        if !seen.insert(*id) {
            continue;
        }
        repo.get_in_workspace(workspace_id, Id::from(*id))
            .await
            .map_err(|_| not_found("skill"))?;
    }
    Ok(())
}

/// 上游 `SetAgentSkills`：整体替换绑定（`enabled` 回到默认 `TRUE`）。
pub(super) async fn set_agent_skills(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let ids = decode_skill_ids(&body)?;
    validate_ids_in_workspace(&state, scope.workspace_id, &ids).await?;
    binding_repo(&state)
        .replace_agent_skills(agent.id(), &ids)
        .await
        .map_err(|e| repo_err(e, "agent skill"))?;
    updated_agent_skills(&state, agent.id()).await
}

/// 上游 `AddAgentSkills`：只追加（已停用的绑定**不**被重新启用 ⇒ `ON CONFLICT DO NOTHING`）。
pub(super) async fn add_agent_skills(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let ids = decode_skill_ids(&body)?;
    validate_ids_in_workspace(&state, scope.workspace_id, &ids).await?;
    binding_repo(&state)
        .add_agent_skills(agent.id(), &ids)
        .await
        .map_err(|e| repo_err(e, "agent skill"))?;
    updated_agent_skills(&state, agent.id()).await
}

// ---------------------------------------------------------------------------
// PUT /api/agents/{id}/skills/{skill_id}/enabled + DELETE /api/agents/{id}/skills/{skill_id}
// ---------------------------------------------------------------------------

/// 上游那段匿名 `struct { Enabled *bool }`：解析失败与缺 `enabled` 报同一句话。
fn decode_enabled(body: &Bytes) -> Result<bool, Error> {
    let value: JsonValue = serde_json::from_slice(body).unwrap_or(JsonValue::Null);
    value
        .get("enabled")
        .and_then(JsonValue::as_bool)
        .ok_or_else(|| bad_request("enabled is required"))
}

/// 上游 `SetAgentSkillEnabled`（绑定不存在 → 404；**不** upsert）。
pub(super) async fn set_agent_skill_enabled(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((id, skill_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let skill_id = parse_uuid(&skill_id, "skill_id")?;
    let enabled = decode_enabled(&body)?;
    let rows = binding_repo(&state)
        .set_agent_skill_enabled(agent.id(), skill_id, enabled)
        .await
        .map_err(|e| repo_err(e, "agent skill"))?;
    if rows == 0 {
        return Err(not_found("agent skill").into());
    }
    updated_agent_skills(&state, agent.id()).await
}

/// 上游 `RemoveAgentSkill`（幂等：删不到也回 200 + 最新列表）。
pub(super) async fn remove_agent_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((id, skill_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let skill_id = parse_uuid(&skill_id, "skill_id")?;
    binding_repo(&state)
        .remove_agent_skill(agent.id(), skill_id)
        .await
        .map_err(|e| repo_err(e, "agent skill"))?;
    updated_agent_skills(&state, agent.id()).await
}

// ---------------------------------------------------------------------------
// PUT /api/agents/{id}/runtime-skills/enabled
// ---------------------------------------------------------------------------

/// 上游那段匿名请求体（6 个字段，`enabled` 是唯一必填）。
#[derive(Debug, Default, Deserialize)]
struct RuntimeSkillEnabledRequest {
    #[serde(default)]
    runtime_id: Option<String>,
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    plugin: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// 上游 `normalizeRuntimeSkillIdentity`（`agent_runtime_skills.go:37`）。
///
/// 返回 `(root, cleaned_key, plugin)`；`None` ⇒ 400 `invalid runtime skill identity`。
/// - `provider` / `universal` 两类的 `plugin` 被清空；
/// - `plugin` 类必须带 `plugin`；
/// - 其他 root 一律拒绝。
fn normalize_runtime_skill_identity(
    root: &str,
    key: &str,
    plugin: &str,
) -> Option<(String, String, String)> {
    let root = root.trim();
    let key = key.trim();
    let mut plugin = plugin.trim();
    if key.is_empty() || key.len() > MAX_RUNTIME_SKILL_KEY_LENGTH {
        return None;
    }
    let cleaned = clean_slash_path(key)?;
    match root {
        "provider" | "universal" => plugin = "",
        "plugin" if !plugin.is_empty() => {}
        _ => return None,
    }
    Some((root.to_string(), cleaned, plugin.to_string()))
}

/// Go `filepath.ToSlash(filepath.Clean(filepath.FromSlash(key)))` 的 Linux 语义等价物。
///
/// 上游随后拒绝「绝对路径 / `.` / `..` / `../` 前缀」。Linux 上 `FromSlash` 不转 `\`
/// （它只把 `/` 换成 `os.PathSeparator`，Linux 下即 `/`），所以 `\` 是普通字符，本函数同样处理。
fn clean_slash_path(path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            // 消不掉的 `..` ⇒ Clean 结果以 `../` 开头 → 上游拒绝。
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        // Clean 结果是 "."（或被清空）→ 上游拒绝。
        return None;
    }
    Some(parts.join("/"))
}

/// `Option<String>` 的「上游空串」等价视图（Go 侧这两个字段是 `string` + `omitempty`）。
fn opt_str(value: Option<&String>) -> &str {
    value.map_or("", String::as_str)
}

/// 上游 `sameDisabledRuntimeSkill`：身份四要素 + `plugin`（**不**比 `name`）。
fn same_runtime_skill_identity(a: &DisabledRuntimeSkillDto, b: &DisabledRuntimeSkillDto) -> bool {
    a.runtime_id == b.runtime_id
        && a.provider == b.provider
        && a.root == b.root
        && a.key == b.key
        && opt_str(a.plugin.as_ref()) == opt_str(b.plugin.as_ref())
}

/// 上游读改写的那 12 行：先摘掉同身份的旧条目，`enabled=false` 时再追加新条目。
fn merge_disabled_runtime_skills(
    current: &[DisabledRuntimeSkillDto],
    target: &DisabledRuntimeSkillDto,
    enabled: bool,
) -> Vec<DisabledRuntimeSkillDto> {
    let mut next: Vec<DisabledRuntimeSkillDto> = current
        .iter()
        .filter(|skill| !same_runtime_skill_identity(skill, target))
        .cloned()
        .collect();
    if !enabled {
        next.push(target.clone());
    }
    next
}

fn non_empty(raw: &str) -> Option<String> {
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

/// 上游 `SetAgentRuntimeSkillEnabled`：持久化「该 agent 在这台 runtime 上禁用某个本地 skill」。
///
/// 读改写必须在**一个事务**里并先 `SELECT … FOR UPDATE` 锁 agent 行（上游 `GetAgentForUpdate`
/// 里的二次比 `runtime_id`）：并发两个开关不能互相覆盖，且换机器后旧覆盖立刻作废（409）。
/// 本文件只落 HTTP 204（上游额外广播 `agent:status`，WS 面不在 M6-4 范围）。
pub(super) async fn set_agent_runtime_skill_enabled(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage(&agent)?;
    let (runtime_id, target, enabled) = resolve_runtime_skill_target(&scope, &agent, &body).await?;
    persist_runtime_skill_override(&state, &agent, runtime_id, &target, enabled).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 校验链的读前半段（400/404/409 都出在这里）⇒ 要写入的覆盖条目 + `enabled`。
async fn resolve_runtime_skill_target(
    scope: &AgentScope,
    agent: &AgentRow,
    body: &Bytes,
) -> Result<(Uuid, DisabledRuntimeSkillDto, bool), Error> {
    let value: JsonValue = serde_json::from_slice(body).unwrap_or(JsonValue::Null);
    let req: RuntimeSkillEnabledRequest = if value.is_null() {
        RuntimeSkillEnabledRequest::default()
    } else {
        serde_json::from_value(value)
            .map_err(|_| bad_request("runtime_id, root, key, and enabled are required"))?
    };
    let Some(enabled) = req.enabled else {
        return Err(bad_request(
            "runtime_id, root, key, and enabled are required",
        ));
    };
    let runtime_id = parse_uuid(req.runtime_id.as_deref().unwrap_or_default(), "runtime_id")?;
    if agent.runtime_id != Some(runtime_id) {
        return Err(conflict("agent is no longer assigned to this runtime"));
    }
    let runtime = scope
        .repo
        .runtime_binding(scope.workspace_id, runtime_id)
        .await
        .map_err(|_| not_found("runtime"))?;
    if runtime.runtime_mode != "local" || !matches!(runtime.provider.as_str(), "codex" | "claude") {
        return Err(bad_request(
            "runtime skill controls are only supported for codex and claude",
        ));
    }
    let Some((root, key, plugin)) = normalize_runtime_skill_identity(
        req.root.as_deref().unwrap_or_default(),
        req.key.as_deref().unwrap_or_default(),
        req.plugin.as_deref().unwrap_or_default(),
    ) else {
        return Err(bad_request("invalid runtime skill identity"));
    };
    if root == "plugin" && runtime.provider != "claude" {
        return Err(bad_request("invalid runtime skill identity"));
    }
    let name = req.name.as_deref().unwrap_or_default().trim();
    if name.len() > MAX_RUNTIME_SKILL_KEY_LENGTH {
        return Err(bad_request("invalid runtime skill name"));
    }
    let target = DisabledRuntimeSkillDto {
        runtime_id: runtime_id.to_string(),
        provider: runtime.provider,
        root,
        key,
        name: non_empty(name),
        plugin: non_empty(&plugin),
    };
    Ok((runtime_id, target, enabled))
}

/// 事务内的读改写（锁 agent 行 → 重比 runtime → 过滤 + 追加 → 写回）。
async fn persist_runtime_skill_override(
    state: &AppState,
    agent: &AgentRow,
    runtime_id: Uuid,
    target: &DisabledRuntimeSkillDto,
    enabled: bool,
) -> Result<(), Error> {
    let repo = binding_repo(state);
    let mut tx = repo.begin().await.map_err(|e| repo_err(e, "agent"))?;
    let locked = repo
        .lock_agent_runtime_skills(&mut tx, agent.id())
        .await
        .map_err(|e| repo_err(e, "agent"))?
        .ok_or_else(|| Error::Internal("failed to load agent".to_string()))?;
    if locked.runtime_id != Some(runtime_id) {
        return Err(conflict("agent is no longer assigned to this runtime"));
    }
    let current = decode_disabled_runtime_skills(&locked.disabled_runtime_skills);
    let next = merge_disabled_runtime_skills(&current, target, enabled);
    let payload = serde_json::to_value(&next)?;
    repo.update_disabled_runtime_skills(&mut tx, agent.id(), &payload)
        .await
        .map_err(|e| repo_err(e, "agent"))?;
    tx.commit()
        .await
        .map_err(|e| repo_err(RepoError::Db(e.to_string()), "agent"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dto(root: &str, key: &str, plugin: Option<&str>) -> DisabledRuntimeSkillDto {
        DisabledRuntimeSkillDto {
            runtime_id: "rt-1".to_string(),
            provider: "claude".to_string(),
            root: root.to_string(),
            key: key.to_string(),
            name: None,
            plugin: plugin.map(str::to_string),
        }
    }

    #[test]
    fn identity_normalises_root_and_key() {
        assert_eq!(
            normalize_runtime_skill_identity(" provider ", " a/./b ", "x"),
            Some(("provider".to_string(), "a/b".to_string(), String::new()))
        );
        assert_eq!(
            normalize_runtime_skill_identity("universal", "k", ""),
            Some(("universal".to_string(), "k".to_string(), String::new()))
        );
        assert_eq!(
            normalize_runtime_skill_identity("plugin", "p/k", " claude-foo "),
            Some((
                "plugin".to_string(),
                "p/k".to_string(),
                "claude-foo".to_string()
            ))
        );
    }

    #[test]
    fn identity_rejects_escaping_and_bad_roots() {
        for (root, key, plugin) in [
            ("provider", "", ""),
            ("provider", "../x", ""),
            ("provider", "/abs", ""),
            ("provider", "a/../..", ""),
            ("provider", ".", ""),
            ("plugin", "k", ""),
            ("other", "k", ""),
            (
                "provider",
                &"k".repeat(MAX_RUNTIME_SKILL_KEY_LENGTH + 1),
                "",
            ),
        ] {
            assert!(
                normalize_runtime_skill_identity(root, key, plugin).is_none(),
                "should reject root={root} key={key}"
            );
        }
    }

    #[test]
    fn merge_disables_then_re_enables() {
        // 存量里同身份重复两条也一并摘掉（上游是按身份 filter，不是跳过一次）。
        let current = vec![
            dto("provider", "a", None),
            dto("provider", "b", None),
            dto("provider", "a", None),
        ];
        let target = dto("provider", "a", None);

        let disabled = merge_disabled_runtime_skills(&current, &target, false);
        assert_eq!(disabled.len(), 2);
        assert_eq!(disabled[0].key, "b");
        assert_eq!(disabled[1].key, "a");

        // 再启用：只摘掉同身份的一条，`b` 原样保留（幂等）。
        let enabled = merge_disabled_runtime_skills(&disabled, &target, true);
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].key, "b");
        assert_eq!(
            merge_disabled_runtime_skills(&enabled, &target, true),
            enabled
        );
    }

    #[test]
    fn identity_ignores_name_but_not_plugin() {
        let mut a = dto("plugin", "k", Some("p"));
        a.name = Some("n".to_string());
        let b = dto("plugin", "k", Some("p"));
        assert!(same_runtime_skill_identity(&a, &b));

        // 上游存的空串与请求里的缺省是同一个值。
        let mut c = dto("provider", "k", None);
        c.plugin = Some(String::new());
        assert!(same_runtime_skill_identity(&dto("provider", "k", None), &c));
        assert!(!same_runtime_skill_identity(
            &a,
            &dto("plugin", "k", Some("q"))
        ));
    }

    #[test]
    fn enabled_requires_a_boolean() {
        assert!(decode_enabled(&Bytes::from_static(b"{\"enabled\":true}")).unwrap());
        assert!(!decode_enabled(&Bytes::from_static(b"{\"enabled\":false}")).unwrap());
        for bad in [&b"{}"[..], b"null", b"{\"enabled\":\"true\"}", b"not json"] {
            assert!(decode_enabled(&Bytes::from(bad.to_vec())).is_err());
        }
    }

    #[test]
    fn skill_ids_accept_null_and_reject_bad_uuids() {
        assert!(decode_skill_ids(&Bytes::from_static(b"null"))
            .unwrap()
            .is_empty());
        assert!(decode_skill_ids(&Bytes::from_static(b"{}"))
            .unwrap()
            .is_empty());
        assert!(
            decode_skill_ids(&Bytes::from_static(b"{\"skill_ids\":null}"))
                .unwrap()
                .is_empty()
        );
        assert!(
            decode_skill_ids(&Bytes::from_static(b"{\"skill_ids\":[\"not-a-uuid\"]}")).is_err()
        );
        assert!(decode_skill_ids(&Bytes::from_static(b"{\"skill_ids\":[1]}")).is_err());
        let ok = decode_skill_ids(&Bytes::from_static(
            b"{\"skill_ids\":[\"1c331d0b-94fd-412a-a7cc-6a209add00a1\"]}",
        ))
        .unwrap();
        assert_eq!(ok.len(), 1);
    }
}
