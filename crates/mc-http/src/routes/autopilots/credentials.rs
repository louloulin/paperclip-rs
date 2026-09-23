//! M5-3：trigger **凭据**路由（rotate webhook token / set signing secret）。
//!
//! - **写者**：M5-3（`docs/44` §3.2；§6.3 明确从 `trigger.rs` 拆出，约 168 行）。
//! - **路由**（`router.go` L2113–L2114）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 13 | POST | `/api/autopilots/:id/triggers/:triggerId/rotate-webhook-token` | `RotateAutopilotTriggerWebhookToken` | 69 |
//! | 14 | PUT | `/api/autopilots/:id/triggers/:triggerId/signing-secret` | `SetAutopilotTriggerSigningSecret` | 65 |
//!
//! - **两条都是单形态**（plain 子路由）⇒ 不要加尾斜杠别名。
//! - **凭据只出 hint**：响应与日志都用 `signingSecretHint`15 / `redactWebhookSecrets`19 的语义
//!   （真值在 `mc_autopilot::credential`），**绝不出明文**；日志通道走 `mc-telemetry` 的
//!   redaction（`docs/33` §12.2）。
//! - **写敏感值的两条路由**（⑨/安全面）：需要 403/404 判负与「只写不回显」的断言。
//!
//! # M5-3 落地的四条纪律（`docs/51-M5-3-TRIGGER-WRITE.md`）
//!
//! 1. **`signing_secret` 只进不出**：响应里只有 `has_signing_secret` + `signing_secret_hint`
//!    （末 4 位），明文只出现在**请求体**里 —— 这也是上游把这条从 `UpdateAutopilotTrigger`
//!    单独拆出来的原因（免得和通用 PATCH 体一起被日志/审计抄走）。
//! 2. **`rotate-webhook-token` 是唯一回显 token 的写面**（与 `#10` 的 webhook 创建同侧）：
//!    调用者已过 [`resolve_write_scope`] 的写权门槛，且**必须**能读到新 token，否则轮换后
//!    没有任何渠道拿到它。读面（M5-1 `GET /api/autopilots/:id`）对非写者一律
//!    `redact_webhook_secrets`，两面的差异是**刻意**的，不是漏了一处脱敏。
//! 3. **本文件的唯一日志点是 [`log_credential_write`]**：日志行里只有 `action` / 两个 id /
//!    `signing_configured` 布尔 —— **不含**任何凭据值。`redact_log_line` 只是兜底通道
//!    （它只认 `key=value` 形状，裸值不会被抹；`mc_autopilot::credential` 的测试把这条已知限制
//!    钉住了），所以「不把凭据放进这一行」仍是本文件的责任。
//! 4. **重试只换 token、不改行**：唯一索引冲突（[`RepoError::Conflict`]）是「换一个 token 再
//!    INSERT/UPDATE」的信号；耗尽后 500。旧 token 在 UPDATE 成功前一直有效（`WHERE id = $1`
//!    命中即替换，不存在「先清空再写」的中间态）。
//!
//! # 与 `trigger.rs` 的关系
//!
//! 两条 handler 复用同片的 [`resolve_write_scope`] / [`load_bound_trigger_row`]：权限链与
//! 「trigger 必须属于本 autopilot」的绑定校验只有一份实现（跨文件复用在**同片内**是允许的，
//! 且这正是 §6.3 拆文件的前提 —— 拆的是行数，不是契约）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{post, put};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use mc_autopilot::credential::{
    normalize_signing_secret, redact_log_line, CredentialError, SIGNING_SECRET_TOO_SHORT_MESSAGE,
};
use mc_autopilot::dto::AutopilotTriggerResponse;
use mc_autopilot::trigger::TRIGGER_KIND_WEBHOOK;
use mc_errors::Error;
use mc_repos::autopilot::trigger::{
    generate_webhook_token, AutopilotTriggerRepo, WEBHOOK_TOKEN_ATTEMPTS,
};
use mc_repos::autopilot::AutopilotTriggerRow;
use mc_repos::RepoError;

use super::trigger::{decode_body, load_bound_trigger_row, resolve_write_scope};
use crate::error::ApiResult;
use crate::routes::agents::{bad_request, parse_uuid, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::dto::trigger_to_response;
use crate::state::AppState;

/// 凭据两条路由的 router（2 条路由 / 2 个注册键，**都只有单形态**）。
pub fn router() -> Router<Arc<AppState>> {
    // 上游是 plain 子路由（`r.Post("/rotate-webhook-token", …)` / `r.Put("/signing-secret", …)`），
    // chi 只服务一个形态 ⇒ 加 `…/` 会被门 ⑦ 判 `EXTRA_ALIAS`。
    Router::new()
        .route(
            "/api/autopilots/:id/triggers/:triggerId/rotate-webhook-token",
            post(rotate_webhook_token),
        )
        .route(
            "/api/autopilots/:id/triggers/:triggerId/signing-secret",
            put(set_signing_secret),
        )
}

/// 上游 `SetSigningSecretRequest`（`handler/autopilot.go:408`）。
///
/// 这条请求体**只有** `signing_secret` 一个字段（上游注释：免得 secret 和别的字段共用一份
/// 会被「记录 PATCH 体」的通用日志抄走）。空串/全空白 = **清除**（退回只验 bearer token），
/// 不是 400 —— 见 [`normalize_signing_secret`] 的三态表。
#[derive(Debug, Default, Deserialize)]
struct SetSigningSecretRequest {
    /// 明文签名密钥（只在请求体里出现，永不回显）。
    #[serde(default)]
    signing_secret: Option<String>,
}

/// 本片写凭据路由的**唯一**日志点（见模块文档第 3 条）。
///
/// 两个**刻意**的字段命名/取值细节：
///
/// - `signing_configured` 而不是 `has_signing_secret`：`redact_log_line` 的 `is_sensitive` 是
///   「键名**包含** `secret` 即敏感」，于是 `has_signing_secret=true` 的 `true` 会被抹成
///   `[REDACTED]`（无害，但把唯一的信号弄丢了）。换个不含敏感词的键名，值才留得下来。
/// - 动作名写 `rotate` / `set-signing-secret`，**不**写完整路由名（`…rotate-webhook-token`
///   里含 `token`，一样会被当成敏感键做一次无用的 lookahead）。
fn log_credential_write(
    action: &str,
    autopilot_id: Uuid,
    trigger_id: Uuid,
    signing_configured: bool,
) {
    let line = redact_log_line(&format!(
        "autopilot trigger credential write action={action} autopilot_id={autopilot_id} \
         trigger_id={trigger_id} signing_configured={signing_configured}"
    ));
    tracing::info!(line = %line, "autopilot trigger credential write");
}

/// 「这一行现在配了签名密钥吗」—— 只看列（不看 token 是否存在）：日志要的是配置状态，
/// 而 DTO 的 `has_signing_secret` 只在「webhook 且 token 非空」时才计算。
fn signing_configured(row: &AutopilotTriggerRow) -> bool {
    row.signing_secret
        .as_deref()
        .is_some_and(|secret| !secret.is_empty())
}

/// `POST /api/autopilots/:id/triggers/:triggerId/rotate-webhook-token`（上游 69 行，**200**）。
///
/// 判负顺序（上游逐字）：写权门槛 → 非 UUID trigger id（400）→ 加载（404）→
/// **非 webhook 触发器 → 400 `trigger is not a webhook trigger`**（先于任何铸造动作）。
///
/// 铸造失败本地**不可达**（[`generate_webhook_token`] 是纯 CPU 的 `rand`，没有上游
/// `generateWebhookToken` 的 error 路径）⇒ 上游的 `failed to generate webhook token` 分支
/// 在本仓不存在；唯一 500 是「3 次都撞唯一索引」。
async fn rotate_webhook_token(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((autopilot_id, trigger_id)): Path<(String, String)>,
) -> ApiResult<Json<AutopilotTriggerResponse>> {
    let scope = resolve_write_scope(&state, user, &headers, &query, &autopilot_id).await?;
    let trigger_uuid = parse_uuid(&trigger_id, "trigger id")?;
    let prev = load_bound_trigger_row(&state, scope.autopilot.id, trigger_uuid).await?;
    if prev.kind != TRIGGER_KIND_WEBHOOK {
        return Err(bad_request("trigger is not a webhook trigger").into());
    }

    let repo = AutopilotTriggerRepo::new(state.db.clone());
    let mut rotated = None;
    for _ in 0..WEBHOOK_TOKEN_ATTEMPTS {
        let token = generate_webhook_token();
        match repo.rotate_webhook_token(trigger_uuid, &token).await {
            Ok(row) => {
                rotated = Some(row);
                break;
            }
            // 唯一索引冲突 ⇒ 换一个 token 再来（旧 token 在这条 SQL 成功前一直有效）。
            Err(RepoError::Conflict) => {}
            Err(err) => return Err(repo_err(err, "trigger").into()),
        }
    }
    let Some(row) = rotated else {
        return Err(Error::Internal(format!(
            "failed to rotate webhook token: could not mint a unique token in {WEBHOOK_TOKEN_ATTEMPTS} attempts"
        ))
        .into());
    };
    log_credential_write(
        "rotate",
        scope.autopilot.id,
        trigger_uuid,
        signing_configured(&row),
    );
    Ok(Json(trigger_to_response(&row)))
}

/// `PUT /api/autopilots/:id/triggers/:triggerId/signing-secret`（上游 65 行，**200**）。
///
/// 空 body / 形状不符 → 400 `invalid request body`（Go 的 `json.Decode` 在空体上报 EOF，
/// **即使字段本身可选**）；`signing_secret` 非空但短于 16 字节 → 400（文案逐字见
/// [`SIGNING_SECRET_TOO_SHORT_MESSAGE`]）；空串/全空白 → **清除**（落 NULL）。
async fn set_signing_secret(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((autopilot_id, trigger_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<AutopilotTriggerResponse>> {
    let scope = resolve_write_scope(&state, user, &headers, &query, &autopilot_id).await?;
    let trigger_uuid = parse_uuid(&trigger_id, "trigger id")?;
    let prev = load_bound_trigger_row(&state, scope.autopilot.id, trigger_uuid).await?;
    if prev.kind != TRIGGER_KIND_WEBHOOK {
        return Err(bad_request("trigger is not a webhook trigger").into());
    }
    let req = decode_body::<SetSigningSecretRequest>(&body)?;
    // 三态归一化：`""`/空白 ⇒ `None`（清除）；< 16 字节 ⇒ 400；否则 `Some(trimmed)`。
    let secret = match normalize_signing_secret(req.signing_secret.as_deref().unwrap_or_default()) {
        Ok(secret) => secret,
        Err(CredentialError::SigningSecretTooShort) => {
            return Err(bad_request(SIGNING_SECRET_TOO_SHORT_MESSAGE).into());
        }
    };

    let repo = AutopilotTriggerRepo::new(state.db.clone());
    let row = repo
        .set_signing_secret(trigger_uuid, secret)
        .await
        .map_err(|err| repo_err(err, "trigger"))?;
    log_credential_write(
        "set-signing-secret",
        scope.autopilot.id,
        trigger_uuid,
        signing_configured(&row),
    );
    // 响应里**没有** secret 本身，只有 `has_signing_secret` + `signing_secret_hint`。
    Ok(Json(trigger_to_response(&row)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空 body 在 Go 里是 `json.Decode` 的 EOF 错误 ⇒ 400（哪怕 `signing_secret` 可缺省）。
    #[test]
    fn empty_body_is_rejected_even_though_the_field_is_optional() {
        let err = decode_body::<SetSigningSecretRequest>(&Bytes::from_static(b""))
            .expect_err("empty body must fail");
        assert!(matches!(err, Error::Validation { .. }), "{err:?}");
    }

    /// 清除路径：字段缺省、显式 `null`、空串都归一到「清除」。
    #[test]
    fn clearing_forms_all_normalize_to_none() {
        for raw in [&br"{}"[..], &br#"{"signing_secret":null}"#[..]] {
            let req = decode_body::<SetSigningSecretRequest>(&Bytes::copy_from_slice(raw))
                .expect("decode");
            assert_eq!(
                normalize_signing_secret(req.signing_secret.as_deref().unwrap_or_default()),
                Ok(None)
            );
        }
        let req = decode_body::<SetSigningSecretRequest>(&Bytes::from_static(
            br#"{"signing_secret":"   "}"#,
        ))
        .expect("decode");
        assert_eq!(
            normalize_signing_secret(req.signing_secret.as_deref().unwrap_or_default()),
            Ok(None)
        );
    }

    /// 日志行**不含**凭据值，且 `signing_configured` 的值不会被 redaction 抹掉。
    #[test]
    fn log_line_carries_no_credential() {
        let autopilot_id = Uuid::nil();
        let trigger_id = Uuid::nil();
        let line = redact_log_line(&format!(
            "autopilot trigger credential write action=rotate autopilot_id={autopilot_id} \
             trigger_id={trigger_id} signing_configured=true"
        ));
        // 值还在（键名不含 `secret`）—— 这是「值可见」的一半。
        assert!(line.contains("signing_configured=true"), "{line}");
        // 换成含敏感词的键名，值会被抹（说明通道真的在工作，而不是空转）。
        let masked = redact_log_line("has_signing_secret=true");
        assert!(masked.contains("[REDACTED]"), "{masked}");
    }
}
