//! `/api/agents/:id/env` 两条路由（上游 `agent_env.go` L141 / L190）。
//!
//! | method | path | 上游 |
//! |---|---|---|
//! | GET | `/api/agents/:id/env` | `GetAgentEnv` L141 |
//! | PUT | `/api/agents/:id/env` | `UpdateAgentEnv` L190 |
//!
//! 形状（**与 issue 正文里的括注不同**，这里是按上游源码逐行核过的）：
//! `GET` 返回**明文** `{"agent_id", "custom_env"}`，并且**先落审计行再出明文**
//! （fail-closed：写不进 `activity_log` 就 500，绝不无痕地交付密钥）；
//! `PUT` 才是脱敏语义——值等于 `****`（`envSentinel`）表示「该键保留库里原值」，
//! 「键 + `****` + 库里不存在」则整键丢弃（永不把字面 `****` 写进库）。
//!
//! 鉴权：上游 `authorizeAgentEnv` = 先拒 agent actor，再要求 workspace
//! owner/admin 或 agent 的**人类 owner**。本片不解析 agent actor（见
//! `agents.rs` 顶部说明），因此等价于「admin 或 agent owner」的 fail-closed 版本。
//!
//! 偏离：`agent:status` 广播已接（M3-7-fu / LUM-1506）。`PUT` 在提交并重新读出 skills 后
//! 发一条**脱敏**的 `agent:status`（载荷是 `AgentDto`，从不带 env 值），投递面是
//! **该 workspace 的用户连接**（`Hub::notify_agent_status`）。
//!
//! 与上游的两处差异（登记在 `docs/44`）：① 本地 `AgentDto.skills` 恒为空（没有上游
//! `attachAgentSkills` 的等价物），上游为此专门重读过 skills；② 上游的
//! `invocation_targets` 会一并带上，本片不额外查它（省一次 DB 往返，客户端仍以 HTTP 面为准）。

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value as JsonValue};

use mc_repos::agent::{ACTIVITY_ENV_REVEALED, ACTIVITY_ENV_UPDATED};

use super::dto::{AgentDto, AgentEnvDto, CustomEnv, UpdateAgentEnvRequest, ENV_SENTINEL};
use super::{bad_request, repo_err, AgentScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 上游 `unmarshalCustomEnv`：坏形状 → 空 map（永不 panic、永不返回 `None`）。
fn decode_custom_env(stored: &JsonValue) -> CustomEnv {
    stored
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// 上游 `mergeAgentEnv` 的审计摘要（全部按字典序，保证可复现）。
#[derive(Debug, Default, PartialEq, Eq)]
struct EnvAudit {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
    preserved: Vec<String>,
}

/// 上游 `mergeAgentEnv`：`****` 哨兵规则 + 对称差审计。
fn merge_agent_env(existing: &CustomEnv, request: &CustomEnv) -> (CustomEnv, EnvAudit) {
    let mut merged = CustomEnv::new();
    let mut audit = EnvAudit::default();
    for (key, value) in request {
        if value == ENV_SENTINEL {
            if let Some(old) = existing.get(key) {
                merged.insert(key.clone(), old.clone());
                audit.preserved.push(key.clone());
            }
            // 库里没有这个键 → 丢弃，绝不持久化字面 `****`。
            continue;
        }
        match existing.get(key) {
            Some(old) if old == value => {
                merged.insert(key.clone(), value.clone());
            }
            Some(_) => {
                merged.insert(key.clone(), value.clone());
                audit.changed.push(key.clone());
            }
            None => {
                merged.insert(key.clone(), value.clone());
                audit.added.push(key.clone());
            }
        }
    }
    for key in existing.keys() {
        if !request.contains_key(key) {
            audit.removed.push(key.clone());
        }
    }
    for list in [
        &mut audit.added,
        &mut audit.removed,
        &mut audit.changed,
        &mut audit.preserved,
    ] {
        list.sort();
    }
    (merged, audit)
}

/// `custom_env` 的键列表（上游 `sortedKeys`）。
fn sorted_keys(env: &CustomEnv) -> Vec<String> {
    env.keys().cloned().collect()
}

fn env_to_json(env: &CustomEnv) -> JsonValue {
    serde_json::to_value(env).unwrap_or_else(|_| JsonValue::Object(serde_json::Map::new()))
}

/// `GET /api/agents/:id/env`（上游 `GetAgentEnv`）。
pub(super) async fn get_agent_env(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<AgentEnvDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage_env(&agent)?;

    let custom_env = decode_custom_env(&agent.custom_env);
    let revealed_keys = sorted_keys(&custom_env);
    let details = json!({
        "agent_id": agent.id.to_string(),
        "agent_name": agent.name,
        "revealed_keys": revealed_keys,
        "key_count": revealed_keys.len(),
    });
    // fail-closed：审计行写不下去就不给明文（上游同一分支同一文案）。
    scope
        .repo
        .record_env_activity(
            scope.workspace_id,
            scope.user_id.0,
            ACTIVITY_ENV_REVEALED,
            &details,
        )
        .await
        .map_err(|_| {
            mc_errors::Error::Database(
                "audit log write failed; refusing to serve env without a recorded reveal".into(),
            )
        })?;

    Ok(Json(AgentEnvDto {
        agent_id: agent.id.to_string(),
        custom_env,
    }))
}

/// `PUT /api/agents/:id/env`（上游 `UpdateAgentEnv`，整表替换 + 同事务审计）。
pub(super) async fn update_agent_env(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<AgentEnvDto>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let agent = scope.load_agent(&id).await?;
    scope.require_can_manage_env(&agent)?;

    let req: UpdateAgentEnvRequest =
        serde_json::from_slice(&body).map_err(|_| bad_request("invalid request body"))?;
    let request: CustomEnv = req.custom_env.unwrap_or_default();

    let existing = decode_custom_env(&agent.custom_env);
    let (merged, audit) = merge_agent_env(&existing, &request);
    let details = json!({
        "agent_id": agent.id.to_string(),
        "agent_name": agent.name,
        "added_keys": audit.added,
        "removed_keys": audit.removed,
        "changed_keys": audit.changed,
        "preserved_keys": audit.preserved,
    });

    let updated = scope
        .repo
        .update_custom_env_audited(
            agent.id(),
            &env_to_json(&merged),
            scope.user_id.0,
            ACTIVITY_ENV_UPDATED,
            &details,
        )
        .await
        .map_err(|e| repo_err(e, "agent"))?;

    // 上游 `agent_env.go:266-272`：提交后广播 `agent:status`，连着的客户端据此重取该行、
    // 刷新「已配置 N 个变量」的指示器。载荷是**脱敏**的 agent 响应（`AgentDto` 从不带
    // env 明文）。
    //
    // 广播失败**不影响**响应：它是「尽力而为的唤醒通道」（`hub.rs` 模块文档），客户端
    // 仍以 HTTP 面为准。`updated` 已经是提交后重新读出的行（`update_custom_env_audited`
    // 的返回值），所以 `has_custom_env` / `custom_env_key_count` 是本次写入后的真值。
    if let Ok(targets) = scope.targets_by_agent(&[updated.id]).await {
        let dto = AgentDto::from_row(
            &updated,
            &scope,
            targets.get(&updated.id).map_or(&[], Vec::as_slice),
        );
        if let Ok(agent) = serde_json::to_value(&dto) {
            state.daemon_hub.notify_agent_status(
                &scope.workspace_id.to_string(),
                &mc_ws::frames::AgentStatusPayload { agent },
            );
        }
    }

    Ok(Json(AgentEnvDto {
        agent_id: updated.id.to_string(),
        custom_env: merged,
    }))
}

/// 供测试断言 `BTreeMap` 的字符串键序与上游 Go 的 map 序列化一致。
#[cfg(test)]
fn env_from(pairs: &[(&str, &str)]) -> CustomEnv {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect::<BTreeMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_preserves_existing_values_and_drops_unknown_keys() {
        let existing = env_from(&[("A", "1"), ("B", "2")]);
        let request = env_from(&[("A", ENV_SENTINEL), ("C", ENV_SENTINEL)]);
        let (merged, audit) = merge_agent_env(&existing, &request);
        assert_eq!(merged.get("A").map(String::as_str), Some("1"));
        assert!(!merged.contains_key("C"));
        assert_eq!(audit.preserved, vec!["A"]);
        assert_eq!(audit.removed, vec!["B"]);
        assert!(audit.added.is_empty() && audit.changed.is_empty());
    }

    #[test]
    fn diff_buckets_are_sorted_and_never_count_no_ops() {
        let existing = env_from(&[("A", "1"), ("B", "2"), ("D", "4")]);
        let request = env_from(&[("A", "1"), ("B", "9"), ("C", "3")]);
        let (merged, audit) = merge_agent_env(&existing, &request);
        assert_eq!(merged.get("B").map(String::as_str), Some("9"));
        assert_eq!(audit.added, vec!["C"]);
        assert_eq!(audit.changed, vec!["B"]);
        assert_eq!(audit.removed, vec!["D"]);
        assert!(audit.preserved.is_empty());
    }

    #[test]
    fn empty_request_removes_everything() {
        let existing = env_from(&[("A", "1")]);
        let (merged, audit) = merge_agent_env(&existing, &CustomEnv::new());
        assert!(merged.is_empty());
        assert_eq!(audit.removed, vec!["A"]);
    }

    #[test]
    fn bad_shaped_stored_env_degrades_to_empty_map() {
        assert!(
            decode_custom_env(&JsonValue::Array(vec![JsonValue::String("nope".into())])).is_empty()
        );
        assert!(decode_custom_env(&JsonValue::Null).is_empty());
        let env = decode_custom_env(&json!({"A": 1, "B": "two"}));
        assert_eq!(env.get("B").map(String::as_str), Some("two"));
        assert!(!env.contains_key("A"));
    }

    #[test]
    fn sorted_keys_is_lexicographic() {
        let env = env_from(&[("b", "1"), ("A", "2"), ("a", "3")]);
        assert_eq!(sorted_keys(&env), vec!["A", "a", "b"]);
    }
}
