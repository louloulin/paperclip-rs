//! 安装设置面：配置（含密钥）/ 启停 / 安装令牌的轮换与吊销（**5 条路由**）。
//!
//! 从 `install.rs` 拆出（门 ⑩ 的单文件 800 行硬上限，见那里的「文件布局」）。
//! 共享管路一律用 `super::` 取，**这里不重复定义**。
//!
//! 上游：`internal/handler/plugin.go` 的 `ConfigurePlugin` / `EnablePlugin` / `DisablePlugin` /
//! `RotatePluginToken` / `DeletePluginToken` + `internal/service/plugin.go` 的 `SetConfig` /
//! `SetEnabled` / `RotateInstallCredentials`。

use super::*;

#[derive(Debug, Default, Deserialize)]
struct ConfigurePluginRequest {
    #[serde(default)]
    values: Map<String, Value>,
}

/// `PUT /api/workspaces/:id/plugins/:installationId/config`。
pub(super) async fn configure_plugin(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    match configure_inner(&state, auth.id(), &workspace, &installation, &body).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn configure_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    raw_installation: &str,
    body: &Bytes,
) -> PluginResult<Value> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let installation = installation_for_workspace(state, workspace_id, raw_installation).await?;
    let request: ConfigurePluginRequest = decode(body)?;
    let manifest = installation_manifest(&installation)?;

    // 按目的地分流：非 secret 值留在安装行，secret 值进加密表。secret **永不**落 `config`。
    let mut plain = Map::new();
    let mut secrets: Vec<(String, String)> = Vec::new();
    for (key, value) in &request.values {
        let Some(field) = manifest.config.field(key) else {
            return Err(PluginError::invalid(format!(
                "unknown config field {key:?}"
            )));
        };
        if field.kind == SECRET_KIND {
            let Some(text) = value.as_str() else {
                return Err(PluginError::invalid(format!(
                    "config field {key:?} must be a string"
                )));
            };
            if text.len() > MAX_PLUGIN_SECRET_BYTES {
                return Err(PluginError::invalid(format!(
                    "config field {key:?} exceeds {MAX_PLUGIN_SECRET_BYTES} bytes"
                )));
            }
            secrets.push((key.clone(), text.to_string()));
            continue;
        }
        plain.insert(key.clone(), normalize_config_value(field, value)?);
    }

    // 合并到已存值之上：部分提交不能把表单没交上来的字段静默清掉。
    let mut merged = installation.config_object();
    for (key, value) in plain {
        merged.insert(key, value);
    }
    let encoded = Value::Object(merged);

    // 缺部署密钥时**拒绝持久化**（绝不落明文、也不用零密钥兜底）。
    let key = deployment_key(state);
    if !secrets.is_empty() && key.is_none() {
        return Err(PluginError::unavailable(credential_message(
            &CredentialError::PluginSecretsDisabled,
        )));
    }

    let mut tx = begin(state, "begin configure").await?;
    for (name, text) in &secrets {
        // 空提交 = 清除，而不是存一个 ""。
        if text.is_empty() {
            installations::delete_secret_tx(&mut tx, installation.id(), name)
                .await
                .map_err(|_| PluginError::unavailable("clear plugin secret"))?;
            continue;
        }
        let sealed = seal_secret(key.as_ref(), text)?;
        installations::upsert_secret_tx(&mut tx, installation.id(), name, &sealed)
            .await
            .map_err(|_| PluginError::unavailable("store plugin secret"))?;
    }
    let updated = installations::set_config_tx(&mut tx, installation.id(), &encoded)
        .await
        .map_err(|_| PluginError::unavailable("store plugin config"))?;
    commit(tx, "commit configure").await?;
    installation_payload(state, &updated).await
}

/// `POST /api/workspaces/:id/plugins/:installationId/enable`。
pub(super) async fn enable_plugin(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
) -> Response {
    match set_enabled_inner(&state, auth.id(), &workspace, &installation, true).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// `POST /api/workspaces/:id/plugins/:installationId/disable`。
pub(super) async fn disable_plugin(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
) -> Response {
    match set_enabled_inner(&state, auth.id(), &workspace, &installation, false).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// 停用会**立刻**隐藏全部贡献，但保留存储与密钥 —— 所以重新启用不是重装。
async fn set_enabled_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    raw_installation: &str,
    enabled: bool,
) -> PluginResult<Value> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let installation = installation_for_workspace(state, workspace_id, raw_installation).await?;
    if installation.enabled == enabled {
        // 上游同判：省掉一次无谓的写（幂等）。
        return installation_payload(state, &installation).await;
    }
    let mut tx = begin(state, "begin plugin state update").await?;
    let updated = installations::set_enabled_tx(&mut tx, installation.id(), enabled)
        .await
        .map_err(|_| PluginError::unavailable("update plugin state"))?;
    commit(tx, "commit plugin state update").await?;
    installation_payload(state, &updated).await
}

/// `POST /api/workspaces/:id/plugins/:installationId/token` —— 轮换（明文只此一次）。
pub(super) async fn rotate_plugin_token(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
) -> Response {
    match rotate_token_inner(&state, auth.id(), &workspace, &installation).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn rotate_token_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    raw_installation: &str,
) -> PluginResult<Value> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let installation = installation_for_workspace(state, workspace_id, raw_installation).await?;

    // 先把响应里每个值都备好，再替换已存的哈希：可选的 hook 签名配置失败不能反过来
    // 让上一个令牌失效（上游 `RotateInstallCredentials` 的顺序就是为此）。
    let key = deployment_key(state);
    let signing_secret = match key.as_ref() {
        Some(_) => Some(
            hook_signing_secret(key.as_ref(), installation.id())
                .map_err(|err| PluginError::unavailable(credential_message(&err)))?,
        ),
        None => None,
    };

    let token = new_install_token()?;

    let mut tx = begin(state, "begin token rotation").await?;
    installations::set_token_hash_tx(&mut tx, installation.id(), Some(&hash_token(&token)))
        .await
        .map_err(|_| PluginError::unavailable("store install token"))?;
    commit(tx, "commit token rotation").await?;

    let mut out = Map::new();
    out.insert("token".into(), json!(token));
    if let Some(secret) = signing_secret {
        out.insert("signing_secret".into(), json!(secret));
    }
    Ok(Value::Object(out))
}

/// `DELETE /api/workspaces/:id/plugins/:installationId/token` —— 吊销（**幂等** 204）。
///
/// 库里只丢哈希，所以「已吊销」与「从未签发」是同一个状态：重复调用不得报错。
pub(super) async fn revoke_plugin_token(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
) -> Response {
    match revoke_token_inner(&state, auth.id(), &workspace, &installation).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => err.into_response(),
    }
}

async fn revoke_token_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    raw_installation: &str,
) -> PluginResult<()> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let installation = installation_for_workspace(state, workspace_id, raw_installation).await?;
    let mut tx = begin(state, "begin token revoke").await?;
    installations::set_token_hash_tx(&mut tx, installation.id(), None)
        .await
        .map_err(|_| PluginError::unavailable("revoke install token"))?;
    commit(tx, "commit token revoke").await
}
