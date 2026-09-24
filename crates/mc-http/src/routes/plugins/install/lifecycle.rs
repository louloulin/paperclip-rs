//! 安装生命周期：列表 / 预览 / 安装（含原地升级）/ 卸载（**5 条路由**）。
//!
//! 从 `install.rs` 拆出（门 ⑩ 的单文件 800 行硬上限，见那里的「文件布局」）。
//! 共享管路（错误信封 / 门 / DTO 投影 / 事务薄封装）一律用 `super::` 取，**这里不重复定义**。
//!
//! 上游：`internal/handler/plugin.go` 的 `ListPlugins` / `PreviewPlugin` / `InstallPlugin` /
//! `UninstallPlugin` + `internal/service/plugin.go` 的 `Preview` / `Install` / `Uninstall` /
//! `InstallSkillResources`。

use super::*;

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// `GET /api/workspaces/:id/plugins` —— **成员可见**（成员要能看到挂了什么、拿了哪些 scope）。
pub(super) async fn list_plugins(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(workspace): Path<String>,
) -> Response {
    match list_plugins_inner(&state, auth.id(), &workspace).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn list_plugins_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
) -> PluginResult<Value> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_member(state, raw_workspace, user_id).await?;
    let rows = installation_repo(state)
        .list(workspace_id)
        .await
        .map_err(|_| PluginError::unavailable("failed to list Plugins"))?;
    let mut plugins = Vec::with_capacity(rows.len());
    for row in &rows {
        plugins.push(
            installation_payload(state, row)
                .await
                .map_err(|_| PluginError::unavailable("failed to list Plugins"))?,
        );
    }
    Ok(json!({ "plugins": plugins }))
}

#[derive(Debug, Default, Deserialize)]
struct PreviewPluginRequest {
    #[serde(default)]
    version_id: String,
}

/// `POST /api/workspaces/:id/plugins/preview` —— 两步安装的第一步：**什么都不写**。
pub(super) async fn preview_plugin(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(workspace): Path<String>,
    body: Bytes,
) -> Response {
    match preview_plugin_inner(&state, auth.id(), &workspace, &body).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn preview_plugin_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    body: &Bytes,
) -> PluginResult<Value> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let request: PreviewPluginRequest = decode(body)?;
    let version = version_for_workspace(state, workspace_id, &request.version_id).await?;
    let manifest = manifest_of_version(&version)?;
    require_supported(&manifest)?;

    let mut out = Map::new();
    out.insert("manifest".into(), json!(manifest));
    out.insert("scopes".into(), json!(manifest.scopes));
    out.insert("config_schema".into(), json!(config_fields(&manifest)));
    out.insert("version_id".into(), json!(version.id.to_string()));
    out.insert("version".into(), json!(version.version));
    out.insert("digest".into(), json!(version.digest));

    let existing = installation_repo(state)
        .find_by_key(workspace_id, &manifest.key)
        .await
        .map_err(|_| PluginError::unavailable("load existing installation"))?;
    match existing {
        Some(row) => {
            out.insert("installed".into(), json!(true));
            out.insert("installed_version".into(), json!(row.version));
            let granted = row.granted_scopes();
            let added: Vec<&String> = manifest
                .scopes
                .iter()
                .filter(|scope| !granted.contains(scope))
                .collect();
            if !added.is_empty() {
                out.insert("added_scopes".into(), json!(added));
            }
        }
        None => {
            out.insert("installed".into(), json!(false));
        }
    }
    Ok(Value::Object(out))
}

#[derive(Debug, Default, Deserialize)]
struct InstallPluginRequest {
    #[serde(default)]
    version_id: String,
    #[serde(default)]
    granted_scopes: Vec<String>,
}

/// `POST /api/workspaces/:id/plugins` —— 两步安装的第二步；同一插件已装 ⇒ 原地升级。
pub(super) async fn install_plugin(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(workspace): Path<String>,
    body: Bytes,
) -> Response {
    match install_plugin_inner(&state, auth.id(), &workspace, &body).await {
        Ok(body) => (StatusCode::CREATED, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn install_plugin_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    body: &Bytes,
) -> PluginResult<Value> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let request: InstallPluginRequest = decode(body)?;
    let version = version_for_workspace(state, workspace_id, &request.version_id).await?;
    let manifest = manifest_of_version(&version)?;
    require_supported(&manifest)?;
    require_exact_scopes(&manifest.scopes, &request.granted_scopes)?;

    let granted = Value::Array(manifest.scopes.iter().cloned().map(Value::String).collect());
    let existing = installation_repo(state)
        .find_by_key(workspace_id, &manifest.key)
        .await
        .map_err(|_| PluginError::unavailable("load existing installation"))?;

    let row = match existing {
        None => {
            create_installation(state, workspace_id, user_id, &version, &manifest, &granted).await?
        }
        Some(row) => {
            upgrade_installation(state, user_id, &version, &manifest, &granted, &row).await?
        }
    };
    installation_payload(state, &row).await
}

/// 首装：安装行与它贡献的 skill 必须在**同一个提交**里 —— 只到一半的安装比失败更糟，
/// 因为缺的那一半是看不见的。
async fn create_installation(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
    version: &PackageVersionRow,
    manifest: &Manifest,
    granted: &Value,
) -> PluginResult<InstallationRow> {
    let mut tx = begin(state, "begin install").await?;
    installations::lock_plugin_key_tx(&mut tx, workspace_id, &manifest.key)
        .await
        .map_err(|_| PluginError::unavailable("lock plugin package"))?;
    require_version_still_published(&mut tx, workspace_id, version.id()).await?;

    let row = installations::insert_tx(
        &mut tx,
        &NewInstallation {
            workspace_id,
            plugin_key: &manifest.key,
            package_version_id: version.id(),
            version: &version.version,
            manifest: &version.manifest.0,
            granted_scopes: granted,
            installed_by: user_id,
        },
    )
    .await
    .map_err(|err| match err {
        // 两个管理员同时装同一个插件：一人输给唯一索引。这是可重试的冲突，不是后端坏了。
        RepoError::Conflict => {
            PluginError::conflict("this plugin is already installed in this workspace")
        }
        _ => PluginError::unavailable("install plugin"),
    })?;

    sync_skill_resources(&mut tx, workspace_id, user_id, &row, version, manifest).await?;
    commit(tx, "commit install").await?;
    Ok(row)
}

/// 升级：换绑版本 + 快照 + 同意过的 scope，剪掉新 manifest 不再声明的配置与密钥。
///
/// 密钥剪枝与快照在同一个事务里 —— 否则一次快照可能带着上一版的密文落地。
async fn upgrade_installation(
    state: &AppState,
    user_id: Id,
    version: &PackageVersionRow,
    manifest: &Manifest,
    granted: &Value,
    existing: &InstallationRow,
) -> PluginResult<InstallationRow> {
    let pruned = Value::Object(prune_config(&existing.config_object(), manifest));
    let stored = installation_repo(state)
        .secret_keys(existing.id())
        .await
        .map_err(|_| PluginError::unavailable("list plugin secrets"))?;
    let orphans: Vec<String> = stored
        .into_iter()
        .filter(|key| !is_secret_field(manifest, key))
        .collect();

    let mut tx = begin(state, "begin upgrade").await?;
    installations::lock_plugin_key_tx(&mut tx, existing.workspace_id(), &manifest.key)
        .await
        .map_err(|_| PluginError::unavailable("lock plugin package"))?;
    require_version_still_published(&mut tx, existing.workspace_id(), version.id()).await?;

    for key in &orphans {
        installations::delete_secret_tx(&mut tx, existing.id(), key)
            .await
            .map_err(|_| PluginError::unavailable("prune plugin secret"))?;
    }
    let updated = installations::upgrade_tx(
        &mut tx,
        existing.id(),
        &UpgradeInstallation {
            package_version_id: version.id(),
            version: &version.version,
            manifest: &version.manifest.0,
            granted_scopes: granted,
            config: &pruned,
        },
    )
    .await
    .map_err(|_| PluginError::unavailable("upgrade plugin"))?;

    // 升级也重跑：改过的 SKILL.md 要生效、被拿掉的要剪掉。与快照同一个事务。
    sync_skill_resources(
        &mut tx,
        updated.workspace_id(),
        user_id,
        &updated,
        version,
        manifest,
    )
    .await?;
    commit(tx, "commit upgrade").await?;
    Ok(updated)
}

/// 上游 `requireVersionStillPublished`：锁**内**再确认一次版本还在；不在 ⇒ 409。
async fn require_version_still_published(
    tx: &mut installations::Tx<'_>,
    workspace_id: Id,
    version_id: Id,
) -> PluginResult<()> {
    match packages::get_version_tx(tx, workspace_id, version_id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(PluginError::conflict(
            "this version was deleted while the install was being confirmed; publish or pick another version",
        )),
        Err(_) => Err(PluginError::unavailable("re-read published plugin version")),
    }
}

/// 上游 `InstallSkillResources`：把 manifest 声明的 `skill` 资源物化成 `source='plugin'` 的行。
///
/// **只写行**：落盘路径与解析归 M6-4，本片不碰 `routes/daemon/skills.rs`。
/// 名字取 **manifest 的 resource key**（不是 frontmatter 的 name）：同意页列的是 key，
/// 工具命名空间用的也是它，文件里写了别的名字不能让它悄悄换成另一个名字装进来。
async fn sync_skill_resources(
    tx: &mut installations::Tx<'_>,
    workspace_id: Id,
    user_id: Id,
    installation: &InstallationRow,
    version: &PackageVersionRow,
    manifest: &Manifest,
) -> PluginResult<()> {
    let resources: Vec<_> = manifest
        .contributes
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceType::Skill.as_str())
        .collect();

    let mut inputs = Vec::with_capacity(resources.len());
    for resource in resources {
        let file = packages::file_tx(tx, version.id(), &resource.entry)
            .await
            .map_err(|_| PluginError::unavailable("read plugin package file"))?
            .ok_or_else(|| {
                PluginError::unavailable(format!(
                    "plugin package file {:?} is missing",
                    resource.entry
                ))
            })?;
        let content = String::from_utf8_lossy(&file.content).into_owned();
        let frontmatter = parse_skill_frontmatter(&content);
        let description = if frontmatter.description.trim().is_empty() {
            format!("Provided by the {} Plugin.", manifest.name)
        } else {
            frontmatter.description
        };
        inputs.push(PluginSkillInput {
            name: resource.key.clone(),
            description,
            content,
        });
    }

    sync_plugin_skills(tx, workspace_id, installation.id(), user_id, &inputs)
        .await
        .map_err(|err| match err {
            // `sync_tx` 的守卫在名字属于别人时回 Conflict（上游在这一步会拿到 ErrNoRows）。
            RepoError::Conflict => PluginError::conflict(
                "a skill contributed by this plugin already exists in this workspace",
            ),
            _ => PluginError::unavailable("install plugin skill"),
        })
}

/// `DELETE /api/workspaces/:id/plugins/:installationId` —— 204，无体。
pub(super) async fn uninstall_plugin(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation)): Path<(String, String)>,
) -> Response {
    match uninstall_inner(&state, auth.id(), &workspace, &installation).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => err.into_response(),
    }
}

async fn uninstall_inner(
    state: &AppState,
    user_id: Id,
    raw_workspace: &str,
    raw_installation: &str,
) -> PluginResult<()> {
    require_plugins_v1(state)?;
    let workspace_id = workspace_admin(state, raw_workspace, user_id).await?;
    let installation = installation_for_workspace(state, workspace_id, raw_installation).await?;

    // 没有外键/级联（仓库策略），所以这些 delete 必须同事务：半截卸载会留下谁也够不着的行。
    let mut tx = begin(state, "begin uninstall").await?;
    installations::delete_cascade_tx(&mut tx, installation.id())
        .await
        .map_err(|_| PluginError::unavailable("uninstall the Plugin"))?;
    commit(tx, "commit uninstall").await
}
