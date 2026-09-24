//! 出站（上游 `callHookEndpoint`）：目的地校验、签名、发出去、读回一个有界响应。
//!
//! 从 `hooks_job.rs` 拆出来是门 ⑩ 的 800 行硬上限（与 `routes/plugins/install/` 同款）。

use super::{
    hook_signing_key, net_domains, parse_installation_manifest, policy, resolve_endpoint,
    sign_hook_payload, wire, CallbackRequest, DateTime, DeploymentKey, Digest, Duration,
    EndpointPolicy, HookError, HookInvocation, HookRuntime, Id, InstallationRow, Map, Sha256, Utc,
    Uuid, Value, CONFIG_SECRET, DEV_CA_ENV, DEV_ORIGINS_ENV, HOOK_CONTENT_TYPE,
    HOOK_DEFAULT_TIMEOUT, HOOK_INSTALLATION_HEADER, HOOK_MAX_RESPONSE_BYTES, HOOK_SIGNATURE_HEADER,
    HOOK_SIGNATURE_VERSION, HOOK_TIMESTAMP_HEADER, HOOK_USER_AGENT,
};
use wire::{invocation_trigger, HookBody, HookBodyActor, HookBodySchedule};

// ---------------------------------------------------------------------------
// 出站
// ---------------------------------------------------------------------------

/// 上游 `callHookEndpoint`：校验目的地、签名、发出去、读回一个有界响应。
///
/// 目的地判据与 M6-6 的 MCP 面**同一套**（`mc-mcp` 的 `EndpointPolicy` + `resolve_endpoint`）：
/// `net:` scope 的精确 host 白名单 + 「解析出来的每个地址都是公网」，dev origin 只跳过公网
/// 那一条、**不**跳过白名单。
pub(super) async fn call_hook_endpoint(
    runtime: &HookRuntime,
    invocation: &HookInvocation,
) -> Result<Option<Value>, HookError> {
    let domains = net_domains(&invocation.installation.granted_scopes());
    if domains.is_empty() {
        return Err(HookError::forbidden(
            "this Plugin was granted no net: scope, so it cannot call out",
        ));
    }
    let body = build_hook_body(invocation)?;
    let encoded = serde_json::to_vec(&body)
        .map_err(|error| HookError::invalid(format!("encode hook request: {error}")))?;
    let timestamp = Utc::now().timestamp().to_string();
    let key = runtime.deployment_key().ok_or_else(HookError::disabled)?;
    let headers = build_hook_headers(
        Some(&key),
        invocation.installation.id(),
        &timestamp,
        &encoded,
    )?;

    // 回调令牌恰好活到这次调用结束（上游 `defer s.Callbacks.Revoke(...)`）。
    let callback_token = match &body.callback_token {
        Some(token) if !token.is_empty() => Some(token.clone()),
        _ => None,
    };
    let result = send_hook_request(invocation, &domains, &encoded, &headers).await;
    if let Some(token) = callback_token {
        policy::callback_tokens().revoke(&token);
    }
    result
}

/// 真正发出去的那一段（拆开是为了让 `defer` 的等价物在 [`call_hook_endpoint`] 里更清楚）。
async fn send_hook_request(
    invocation: &HookInvocation,
    domains: &[String],
    encoded: &[u8],
    headers: &[(String, String)],
) -> Result<Option<Value>, HookError> {
    let policy = endpoint_policy(domains);
    let endpoint = resolve_endpoint(&invocation.hook.transport.url, &policy)
        .await
        .map_err(|error| {
            tracing::warn!(hook = %invocation.hook.key, error = %error, "hook endpoint rejected");
            HookError::forbidden("hook endpoint is not allowed")
        })?;

    let timeout = if invocation.hook.timeout_ms > 0 {
        Duration::from_millis(u64::try_from(invocation.hook.timeout_ms).unwrap_or(10_000))
    } else {
        HOOK_DEFAULT_TIMEOUT
    };
    // 复用 M6-6 的安全客户端（dial 时重新解析、拒内网地址、禁重定向、禁代理）。
    let client = mc_mcp::client::secure_client(&endpoint, &policy).map_err(|error| {
        tracing::warn!(hook = %invocation.hook.key, error = %error, "hook client");
        HookError::unavailable("hook endpoint did not answer")
    })?;

    let mut request = client
        .post(endpoint.as_str())
        .body(encoded.to_vec())
        .timeout(timeout);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Err(HookError::timed_out()),
        Err(error) => {
            tracing::warn!(hook = %invocation.hook.key, error = %error, "hook endpoint did not answer");
            return Err(HookError::unavailable("hook endpoint did not answer"));
        }
    };
    let status = response.status();
    let payload = response
        .bytes()
        .await
        .map_err(|error| HookError::unavailable(format!("read hook response: {error}")))?;
    if payload.len() > HOOK_MAX_RESPONSE_BYTES {
        return Err(HookError::unavailable(
            "hook endpoint returned too much data",
        ));
    }
    if !status.is_success() {
        return Err(HookError::unavailable(format!(
            "hook endpoint returned {}",
            status.as_u16()
        )));
    }
    let trimmed: Vec<u8> = payload
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|_| HookError::unavailable("hook endpoint returned a non-JSON body"))
}

/// `EndpointPolicy` 的构造点：与 `plugins/mcp.rs` 同款（`net:` 白名单 + 两个 dev env）。
fn endpoint_policy(allowed_hosts: &[String]) -> EndpointPolicy {
    let origins = std::env::var(DEV_ORIGINS_ENV).unwrap_or_default();
    let ca = std::env::var(DEV_CA_ENV)
        .ok()
        .and_then(|path| std::fs::read(path).ok());
    EndpointPolicy::from_values(allowed_hosts, &origins, ca)
}

/// 出站请求体（上游 `hookRequestBody`）。字段名与省略规则**逐字对齐** ——
/// 一个按 v1 写的 handler 不该关心宿主后来又学会了发什么。
fn build_hook_body(invocation: &HookInvocation) -> Result<HookBody, HookError> {
    let mut body = HookBody {
        version: 1,
        invocation_id: Uuid::new_v4().to_string(),
        delivery_id: invocation.delivery_id.clone(),
        attempt: invocation.attempt.max(1),
        occurred_at: Utc::now(),
        hook_key: invocation.hook.key.clone(),
        trigger: invocation.trigger.as_str().to_owned(),
        event_type: invocation.event_type.clone(),
        workspace_id: invocation.installation.workspace_id().to_string(),
        installation_id: invocation.installation.id().to_string(),
        issue_id: invocation.issue_id.map(|id| id.to_string()),
        actor: HookBodyActor {
            kind: invocation.actor.kind.as_str().to_owned(),
            id: invocation.actor.id.to_string(),
        },
        input: invocation.input.clone(),
        config: non_secret_config(&invocation.installation),
        callback_token: None,
        callback_url: None,
        schedule: invocation.planned_at.map(|planned_at| HookBodySchedule {
            planned_at: planned_at.to_utc(),
        }),
    };
    // 回调令牌：**跨片契约**（`docs/32` §9.9 的 M6-7-D4）—— 表的唯一入口是
    // `routes/v1/policy.rs` 的 `callback_tokens()`，这里**不要**另建一张。
    let token = policy::callback_tokens()
        .issue(&CallbackRequest {
            installation_id: invocation.installation.id(),
            workspace_id: invocation.installation.workspace_id(),
            hook_key: &invocation.hook.key,
            trigger: invocation_trigger(invocation.trigger),
            actor: invocation.actor,
            issue_id: invocation.issue_id,
        })
        .map_err(|error| HookError::unavailable(error.to_string()))?;
    body.callback_token = Some(token);
    // 上游 `CallbackBaseURL` 缺省为空 ⇒ 不出现。本地没有这条配置（登记在 `docs/32` §9.10）。
    Ok(body)
}

/// 上游 `nonSecretConfig`：读安装配置，**按 manifest 剪掉** `secret` 字段。
///
/// 按 **manifest** 剪而不是按存储形态剪：secret 根本不该出现在 `config` 列里，所以这一步
/// 同时是「万一出现也没发出去」的那道保险。
fn non_secret_config(installation: &InstallationRow) -> Option<Map<String, Value>> {
    let Ok(manifest) = parse_installation_manifest(&installation.manifest.0) else {
        return None;
    };
    let mut values = installation.config_object();
    values.retain(|key, _| {
        manifest
            .config
            .field(key)
            .is_some_and(|field| field.kind != CONFIG_SECRET)
    });
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// 出站四个头（+ `Content-Type`）的**唯一**构造点。
///
/// `sign_and_build` 与真请求共用它，所以「测到的头」与「发出去的头」不可能漂移。
///
/// # Errors
///
/// 部署密钥缺失 ⇒ [`HookError::disabled`]（**绝不**发未签名的请求）。
pub fn build_hook_headers(
    key: Option<&DeploymentKey>,
    installation_id: Id,
    timestamp: &str,
    body: &[u8],
) -> Result<Vec<(String, String)>, HookError> {
    if key.is_none() {
        return Err(HookError::disabled());
    }
    // 派生密钥本身在这里也算一遍：未配置时它才是真正拦住出站的那道门
    // （`sign_hook_payload` 内部同样会失败，这里提前一步让错误码统一）。
    hook_signing_key(key, installation_id).map_err(|_| HookError::disabled())?;
    let signature = sign_hook_payload(key, installation_id, timestamp, body)
        .map_err(|_| HookError::disabled())?;
    Ok(vec![
        ("Content-Type".to_owned(), HOOK_CONTENT_TYPE.to_owned()),
        (HOOK_TIMESTAMP_HEADER.to_owned(), timestamp.to_owned()),
        (
            HOOK_SIGNATURE_HEADER.to_owned(),
            format!("{HOOK_SIGNATURE_VERSION}={signature}"),
        ),
        (
            HOOK_INSTALLATION_HEADER.to_owned(),
            installation_id.to_string(),
        ),
        ("User-Agent".to_owned(), HOOK_USER_AGENT.to_owned()),
    ])
}

/// `plugin_hook_schedule` 一格投递的幂等键（上游 `pluginHookScheduleDeliveryID`）：
/// `psd_` + `sha256(installation_id ‖ hook_key ‖ generation ‖ plan_time)` 的 hex。
///
/// 跨重试**稳定**：接收方据此把「同一次计划投递的若干次尝试」认成一个投递。
#[must_use]
pub fn schedule_delivery_id(
    installation_id: Id,
    hook_key: &str,
    generation: Id,
    plan_time: DateTime<Utc>,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        installation_id.to_string(),
        hook_key.to_owned(),
        generation.to_string(),
        plan_time.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\x00");
    }
    format!("psd_{}", hex::encode(hasher.finalize()))
}
