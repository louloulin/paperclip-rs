//! 插件包管理路由：已发布包列表 / 发布 / 本地开发目录发布 / 删包（**4 个注册键**）。
//!
//! - **写者**：M6-5（`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_package.go`(151) + `internal/service/plugin_package.go`(524)。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins/packages` | GET, POST | `router.go:1724-1725` |
//! | `/api/workspaces/:id/plugins/packages/local` | POST | `router.go:1726` |
//! | `/api/workspaces/:id/plugins/packages/:packageId` | DELETE | `router.go:1727` |
//!
//! - **两条硬纪律**：
//!   1. **版本不可变**（`392` 的语义）：发布落库后不得改行，改版本 = 发新版本；
//!   2. `digest` / `sha256` 是**纯 hex**（`char_length = 64` 的 CHECK）—— `sha256:` 前缀只属于
//!      bundle 的**线上**形态（见 `mc_core::skill` 的 hash 口径），别写进列里。
//! - **包体校验**：zip 条目白名单 + 体积上限归 `mc_plugin_host::bundle`（纯逻辑）；本文件只做
//!   路由、多部分体解析（axum 的 `multipart` 特征已在 M6-0 打开）与落库。
//! - **不做什么**：不做安装（`install.rs` 的 `POST /plugins` 走 `package_version_id`）。
//!
//! # 上传面的体量上限为什么是 `to_bytes(.., max+1)` 而不是 `DefaultBodyLimit`
//!
//! 上游是 `http.MaxBytesReader(w, r.Body, MaxBundleSize+64KiB)` + `ParseMultipartForm(同值)`，
//! 两者**都**失败 ⇒ 413「too large or malformed」。本仓 `axum::body::to_bytes` 的 `axum::Error`
//! 不暴露内部类型，判不出「超限」与「其它读错」的差别（同 `routes/skills/import.rs` 的取舍），
//! 但上游这两条本来就**合并成同一个 413**，所以这里整段折成一个 413 反而与上游逐字相同；
//! 真正需要区分的「包本体超 `MaxBundleSize`」在读完字段后再判一次（413「the Plugin package
//! is too large」）。限流/全局 `RequestBodyLimitLayer` 与本路由无关（`docs/54`）。
//!
//! # 本地开发通道（`POST /packages/local`）
//!
//! 上游把 `MULTICA_PLUGIN_DIR` 读进 `PluginService.LocalDir`（构造时一次）；本仓**没有这个装配点**
//! ——`AppState` 是 M6-0 冻结面、`apps/mc-server/src/main.rs` 不在 M6-5 写集，因此这里**按请求
//! 读同名 env**（未设置 ⇒ 400「local plugin sources require MULTICA_PLUGIN_DIR」，与上游同一句），
//! 装配点登记给 M6-10（`docs/32` §9）。目录读取用**同步** `std::fs`：`parse_bundle_from_dir` 的
//! 读回调是同步签名（无法 `await`），且整包上限 4MiB / 512 条目，仅开发通道使用。
//!
//! 与 zip 通道共用 `build_bundle`（M6-1），所以「本地目录」不会比「上传的包」宽一寸：
//! surface 的 JS 词法扫描、skill 的 UTF-8/空白、条目白名单完全同源。
//!
//! # 两条偏离（`docs/32` §9 已登记）
//!
//! 1. **`digest` 算在 manifest 的**原样字节**上**：上游 `BundleDigest` 哈希 `bundle.Canonical`
//!    （`json.Marshal` 归一化后的 manifest），本仓 `mc_plugin_host` 明确不做 canonical 化
//!    （`bundle.rs` 头注），落库的也是原样字节 ⇒ 摘要跟着落库的那一份走。同一个 manifest 的
//!    两种字节形态（键序/空白不同）在本仓会得到不同 digest —— 它只是展示值，没有跨系统比对；
//! 2. `PluginErrorQuota`（507）在本文件**不产生**（本地无配额账本），但错误码集里保留它
//!    （`PluginError::quota`），好让配额片接上时不必改状态码表。
//!
//! **状态：M6-5 已落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩，800 行/文件）：本文件只放**生产代码**，单元测试在 `packages/tests.rs`
//! （`#[cfg(test)] mod tests;`）—— 与 `install.rs` 的 `install/{lifecycle,settings}.rs` 同一处理：
//! 门 ⑩ 对**新文件不豁免**，而 `packages.rs` 是文件模块，子模块落到 `packages/` 目录即可。

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use mc_core::Id;
use mc_plugin_host::bundle::{
    parse_bundle, parse_bundle_from_dir, Bundle, BundleError, MAX_BUNDLE_SIZE,
};
use mc_plugin_host::manifest::MAX_VERSION_LENGTH;
use mc_repos::plugin::installation::{self as installations, InstallationRepo, InstallationRow};
use mc_repos::plugin::package::{
    self as packages, NewVersion, PackageRepo, PackageRow, PackageVersionRow,
};
use mc_repos::RepoError;

use super::install::{
    begin, commit, decode, package_repo, require_plugins_v1, require_supported, timestamp,
    workspace_admin, PluginError, PluginResult,
};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 多部分体里那个字段（上游 `r.FormFile("bundle")`）。
const BUNDLE_FIELD: &str = "bundle";

/// 表单自身的边界/头部余量（上游 `multipartOverheadBytes`）。
const MULTIPART_OVERHEAD_BYTES: usize = 64 * 1024;

/// 上游 `writeError(413, …)` 的逐字文案（读包阶段）。
const UPLOAD_TOO_LARGE: &str = "the Plugin package upload is too large or malformed";

/// 上游 `writeError(400, …)` 的逐字文案（读包阶段）。
const FAILED_TO_READ_PACKAGE: &str = "failed to read the Plugin package";

/// 本地开发通道的根目录（上游 `PluginService.LocalDir` 的来源）。
const LOCAL_DIR_ENV: &str = "MULTICA_PLUGIN_DIR";

/// `/api/workspaces/:id/plugins/packages*`（M6-5 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/plugins/packages",
            get(list_packages).post(publish_package),
        )
        .route(
            "/api/workspaces/:id/plugins/packages/local",
            post(publish_local_package),
        )
        .route(
            "/api/workspaces/:id/plugins/packages/:packageId",
            delete(delete_package),
        )
}

// ---------------------------------------------------------------------------
// 响应体
// ---------------------------------------------------------------------------

/// 上游 `PluginPackageSummary`（键名与上游 JSON tag 一致：全 snake_case，无 `rename_all`）。
#[derive(Debug, Serialize)]
struct PackageSummaryDto {
    id: String,
    plugin_key: String,
    name: String,
    versions: Vec<PackageVersionSummaryDto>,
    created_at: String,
}

/// 上游 `PluginPackageVersionSummary`。
#[derive(Debug, Serialize)]
struct PackageVersionSummaryDto {
    id: String,
    version: String,
    digest: String,
    size_bytes: i64,
    published_at: String,
    /// 该 workspace 当前**装的是不是这一版**（从 `plugin_installation` 读，不是「最新的一版」）。
    installed: bool,
}

/// 上游 `publishLocalPluginRequest`。
#[derive(Debug, Deserialize)]
struct LocalPackageRequest {
    #[serde(default)]
    name: String,
}

// ---------------------------------------------------------------------------
// 路由入口
// ---------------------------------------------------------------------------

/// 上游 `ListPluginPackages` ⇒ 200 `{"packages": [...]}`。
async fn list_packages(
    State(state): State<Arc<AppState>>,
    Path(workspace): Path<String>,
    auth: AuthUser,
) -> Response {
    // 上游每个 handler 的第一句都是 `requirePluginsV1` + 「先解析 workspace 再收体」；
    // 所以门在 handler 层，`*_inner` 拿到的是**已解析**的 workspace。
    let workspace_id = match admin_scope(&state, &workspace, auth.id()).await {
        Ok(workspace_id) => workspace_id,
        Err(err) => return err.into_response(),
    };
    match list_packages_inner(&state, workspace_id).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// 上游 `PublishPluginPackage` ⇒ 201 + 包摘要（`multipart/form-data` 的 `bundle` 文件）。
async fn publish_package(
    State(state): State<Arc<AppState>>,
    Path(workspace): Path<String>,
    auth: AuthUser,
    request: Request,
) -> Response {
    let user_id = auth.id();
    // 发布在**上游的 admin 组**里（`cmd/server/router.go` 那四条 `plugins/packages*` 全在
    // `RequireWorkspaceRoleFromURL(owner, admin)` 之下），所以这里的门是 admin；handler 自己
    // 那句 `workspaceMember` 被它包含（非成员仍回 404，不是 403）。
    // 门在**读体之前**：开关关着/权限不够时应当回 403，而不是先把 2MiB 的包收进来再拒。
    let workspace_id = match admin_scope(&state, &workspace, user_id).await {
        Ok(workspace_id) => workspace_id,
        Err(err) => return err.into_response(),
    };
    let (parts, body) = request.into_parts();
    // 上限一次判到底：`to_bytes` 多留 1 字节，好把「正好等于上限」与「超了」分开
    // （虽然上游把两者合并成同一个 413，`+1` 让这里的语义仍然是「超限」）。
    let Ok(bytes) = to_bytes(body, MAX_BUNDLE_SIZE + MULTIPART_OVERHEAD_BYTES + 1).await else {
        return PluginError::too_large(UPLOAD_TOO_LARGE).into_response();
    };
    let rebuilt = Request::from_parts(parts, Body::from(bytes));
    let Ok(mut multipart) = Multipart::from_request(rebuilt, &state).await else {
        return PluginError::too_large(UPLOAD_TOO_LARGE).into_response();
    };
    let mut archive: Option<Vec<u8>> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return PluginError::too_large(UPLOAD_TOO_LARGE).into_response(),
        };
        // 先把名字拷出来：`field` 接下来要被消费，`name()` 的借用必须立刻结束。
        let name = field.name().map(str::to_string);
        if name.as_deref() == Some(BUNDLE_FIELD) {
            match field.bytes().await {
                Ok(data) => archive = Some(data.to_vec()),
                Err(_) => return PluginError::invalid(FAILED_TO_READ_PACKAGE).into_response(),
            }
        }
    }
    let Some(archive) = archive else {
        return PluginError::invalid("a Plugin package file is required").into_response();
    };
    if archive.len() > MAX_BUNDLE_SIZE {
        return PluginError::too_large("the Plugin package is too large").into_response();
    }
    match publish_upload(&state, workspace_id, user_id, &archive).await {
        Ok(summary) => (StatusCode::CREATED, Json(summary)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// 上游 `PublishLocalPluginPackage` ⇒ 201 + 包摘要（JSON `{"name": "..."}`）。
async fn publish_local_package(
    State(state): State<Arc<AppState>>,
    Path(workspace): Path<String>,
    auth: AuthUser,
    body: Bytes,
) -> Response {
    let user_id = auth.id();
    // 同 `publish_package`：admin 组（`workspaceMember` 被包含）。
    let workspace_id = match admin_scope(&state, &workspace, user_id).await {
        Ok(workspace_id) => workspace_id,
        Err(err) => return err.into_response(),
    };
    match publish_local_inner(&state, workspace_id, user_id, body).await {
        Ok(summary) => (StatusCode::CREATED, Json(summary)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// 上游 `DeletePluginPackage` ⇒ 204。
async fn delete_package(
    State(state): State<Arc<AppState>>,
    Path((workspace, package_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Response {
    let workspace_id = match admin_scope(&state, &workspace, auth.id()).await {
        Ok(workspace_id) => workspace_id,
        Err(err) => return err.into_response(),
    };
    match delete_package_inner(&state, workspace_id, &package_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// 业务
// ---------------------------------------------------------------------------

/// 上游 `ListPackages`：本 workspace 每个包 + 它的全部版本。
async fn list_packages_inner(state: &AppState, workspace_id: Id) -> PluginResult<Value> {
    let repo = package_repo(state);
    let rows = repo
        .list(workspace_id)
        .await
        .map_err(|error| failed(error, "list plugin packages"))?;
    let installations = InstallationRepo::new(state.db.clone());
    let mut summaries = Vec::with_capacity(rows.len());
    for row in &rows {
        summaries.push(package_summary(&repo, &installations, workspace_id, row).await?);
    }
    Ok(json!({ "packages": summaries }))
}

/// 上传通道：解包 → 校验 → 落库（上游 `PublishBundle`）。
async fn publish_upload(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
    archive: &[u8],
) -> PluginResult<PackageSummaryDto> {
    let bundle = parse_bundle(archive)
        .map_err(|error| PluginError::invalid(format!("plugin package is invalid: {error}")))?;
    publish(state, workspace_id, user_id, &bundle, false).await
}

/// 开发通道：读本地目录 → 校验 → 落库（上游 `PublishLocalBundle`）。
async fn publish_local_inner(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
    body: Bytes,
) -> PluginResult<PackageSummaryDto> {
    let payload: LocalPackageRequest = decode(&body)?;
    let name = payload.name.trim().to_owned();
    // 顺序与上游一致：先判 env（400「require MULTICA_PLUGIN_DIR」），再判目录名。
    let root = local_plugin_dir()
        .ok_or_else(|| PluginError::invalid("local plugin sources require MULTICA_PLUGIN_DIR"))?;
    validate_local_name(&name)?;
    let root = root.join(&name);
    let bundle =
        parse_bundle_from_dir(|entry| read_local_entry(&root, entry)).map_err(|error| {
            PluginError::invalid(format!("local plugin package is invalid: {error}"))
        })?;
    publish(state, workspace_id, user_id, &bundle, true).await
}

/// 上游 `DeletePackage`：仍在被安装的包不许删。
async fn delete_package_inner(
    state: &AppState,
    workspace_id: Id,
    raw_package: &str,
) -> PluginResult<()> {
    // 上游只为了拿到「锁哪个 key」在事务外读一次；判据全在事务内重读。
    let package = package_repo(state)
        .get(workspace_id, parse_package_id(raw_package)?)
        .await
        .map_err(|error| match error {
            RepoError::NotFound => PluginError::not_found("plugin package not found"),
            other => failed(other, "load plugin package"),
        })?;

    let mut tx = begin(state, "begin delete").await?;
    installations::lock_plugin_key_tx(&mut tx, workspace_id, &package.plugin_key)
        .await
        .map_err(|error| failed(error, "lock plugin package"))?;
    // 在锁内计数：锁外计数会让「删包之后才提交的安装」指着已经不存在的版本。
    let installed = installations::count_installations_of_versions_tx(&mut tx, package.id())
        .await
        .map_err(|error| failed(error, "count installations"))?;
    if installed > 0 {
        // 事务在这里被 drop ⇒ 回滚（上游 `defer tx.Rollback`）。
        return Err(PluginError::conflict(
            "this plugin is still installed; uninstall it before deleting the published package",
        ));
    }
    // 文件先删：它们指着一个马上不存在的版本，而本仓按仓库政策**不用**级联删除。
    packages::delete_files_by_package_tx(&mut tx, package.id())
        .await
        .map_err(|error| failed(error, "delete plugin package files"))?;
    packages::delete_versions_by_package_tx(&mut tx, package.id())
        .await
        .map_err(|error| failed(error, "delete plugin package versions"))?;
    packages::delete_package_tx(&mut tx, package.id())
        .await
        .map_err(|error| failed(error, "delete plugin package"))?;
    commit(tx, "commit delete").await
}

/// 上游 `PluginService.publish`：一个事务里写包身份 + 版本 + 版本内每个文件。
///
/// 三部分同事务的理由与上游逐字相同：任何「写了一半」都是列表看不出来的谎——
/// 版本在而文件缺 = 面板加载不出东西；名字被一次后来冲突的发布改掉 = 声称描述一个没落库的版本。
async fn publish(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
    bundle: &Bundle,
    dev_loop: bool,
) -> PluginResult<PackageSummaryDto> {
    require_supported(&bundle.manifest)?;
    // 落库的是 manifest 的**原样字节**（本仓不做 canonical 化）：再解析一次只为了拿
    // `serde_json::Value` 落 JSONB 列——能过 `parse_manifest` 就一定解析得出来。
    let manifest: Value = serde_json::from_slice(&bundle.manifest_raw).map_err(|_| {
        PluginError::invalid("plugin package is invalid: the manifest is not valid JSON")
    })?;

    let mut tx = begin(state, "begin publish").await?;
    installations::lock_plugin_key_tx(&mut tx, workspace_id, &bundle.manifest.key)
        .await
        .map_err(|error| failed(error, "lock plugin package"))?;
    let package = packages::upsert_package_tx(
        &mut tx,
        workspace_id,
        user_id,
        &bundle.manifest.key,
        &bundle.manifest.name,
    )
    .await
    .map_err(|error| match error {
        // 锁之后的兜底：另一个还没拿到锁的进程抢先建了同名 key（唯一索引）。
        RepoError::Conflict => {
            PluginError::conflict("this plugin was just published by someone else; try again")
        }
        other => failed(other, "create plugin package"),
    })?;
    let existing = packages::versions_tx(&mut tx, package.id())
        .await
        .map_err(|error| failed(error, "list published versions"))?;
    let version = resolve_publish_version(bundle.manifest.version.as_str(), &existing, dev_loop)?;

    let size_bytes = i64::try_from(bundle.total_size())
        .map_err(|_| PluginError::too_large("the Plugin package is too large"))?;
    let row = packages::insert_version_tx(
        &mut tx,
        &NewVersion {
            package_id: package.id(),
            workspace_id,
            version: &version,
            manifest: &manifest,
            digest: &bundle_digest(bundle),
            size_bytes,
            published_by: user_id,
        },
    )
    .await
    .map_err(|error| match error {
        RepoError::Conflict => PluginError::conflict(format!(
            "version {version} of this plugin is already published; published versions are \
             immutable, so publish a new version instead"
        )),
        other => failed(other, "publish plugin version"),
    })?;
    for file in &bundle.files {
        packages::insert_file_tx(
            &mut tx,
            row.id(),
            &file.path,
            &file.content,
            &sha256_hex(&file.content),
        )
        .await
        .map_err(|error| failed(error, "store plugin package file"))?;
    }
    commit(tx, "commit publish").await?;

    let repo = package_repo(state);
    let installations = InstallationRepo::new(state.db.clone());
    package_summary(&repo, &installations, workspace_id, &package).await
}

/// 上游 `PluginService.packageSummary`。
async fn package_summary(
    repo: &PackageRepo,
    installations: &InstallationRepo,
    workspace_id: Id,
    package: &PackageRow,
) -> PluginResult<PackageSummaryDto> {
    let versions = repo
        .versions(package.id())
        .await
        .map_err(|error| failed(error, "list published versions"))?;
    // 「本 workspace 跑的是哪一版」从 installation 读，不是「最新的一版」：
    // 发布之后这两个不一致才是常态，藏起来就没意义了。
    let installation: Option<InstallationRow> = installations
        .find_by_key(workspace_id, &package.plugin_key)
        .await
        .map_err(|error| failed(error, "load plugin installation"))?;
    let installed_version = installation.map(|row| row.package_version_id);
    let rendered = versions
        .into_iter()
        .map(|row| PackageVersionSummaryDto {
            installed: Some(row.id) == installed_version,
            id: row.id.to_string(),
            version: row.version,
            digest: row.digest,
            size_bytes: row.size_bytes,
            published_at: timestamp(row.created_at),
        })
        .collect();
    Ok(PackageSummaryDto {
        id: package.id().0.to_string(),
        plugin_key: package.plugin_key.clone(),
        name: package.name.clone(),
        versions: rendered,
        created_at: timestamp(package.created_at),
    })
}

// ---------------------------------------------------------------------------
// 版本号 / 摘要 / 本地目录
// ---------------------------------------------------------------------------

/// 上游 `resolvePublishVersion`：上传保留 manifest 的版本号（撞车就是不可变性的判据），
/// 本地开发发布改落 `+dev.N` —— 改一个文件再发一次是开发循环的常态。
fn resolve_publish_version(
    version: &str,
    existing: &[PackageVersionRow],
    dev_loop: bool,
) -> PluginResult<String> {
    if !dev_loop {
        return Ok(version.to_owned());
    }
    let prefix = format!("{version}+dev.");
    let mut taken = false;
    let mut highest = 0u32;
    for row in existing {
        if row.version == version {
            taken = true;
        }
        if let Some(suffix) = row.version.strip_prefix(&prefix) {
            if let Ok(number) = suffix.parse::<u32>() {
                highest = highest.max(number);
            }
        }
    }
    if !taken {
        return Ok(version.to_owned());
    }
    let candidate = format!("{version}+dev.{}", highest.saturating_add(1));
    if candidate.len() > MAX_VERSION_LENGTH {
        return Err(PluginError::invalid(format!(
            "version {version:?} leaves no room for a development suffix; shorten it below \
             {MAX_VERSION_LENGTH} bytes"
        )));
    }
    Ok(candidate)
}

/// 上游 `BundleDigest`：manifest **原样字节** + 每个文件的路径与长度与内容（`files` 已按路径排序）。
///
/// 故意不是「上传压缩包的哈希」：同一批文件的两个 zip 在时间戳/压缩率上都不同，而摘要要回答的
/// 是「我们看到的是不是同一个插件」，不是「这是不是同一次上传」。
fn bundle_digest(bundle: &Bundle) -> String {
    let mut hash = Sha256::new();
    hash.update(&bundle.manifest_raw);
    for file in &bundle.files {
        hash.update(format!("\n{}\n{}\n", file.path, file.content.len()));
        hash.update(&file.content);
    }
    hex::encode(hash.finalize())
}

/// 纯 hex 的 sha256（`plugin_package_file.sha256` 的 CHECK 是 `char_length = 64`）。
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `MULTICA_PLUGIN_DIR`：空的/纯空白视为**未设置**（上游 `strings.TrimSpace`）。
fn local_plugin_dir() -> Option<PathBuf> {
    let raw = std::env::var(LOCAL_DIR_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// 上游 `readLocalFile` 的名字校验：单个目录名、不含分隔符、不以 `.` 开头。
///
/// 以 `.` 开头这一条同时挡掉了 `..`（`strings.HasPrefix` 的语义）。
fn validate_local_name(name: &str) -> PluginResult<()> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(PluginError::invalid(
            "local plugin source must be a single directory name under MULTICA_PLUGIN_DIR",
        ));
    }
    Ok(())
}

/// 上游 `readLocalFile`：条目名由契约层校验过是相对路径，但**这一层碰文件系统**，
/// 所以拼出来的路径要再按插件目录清洗一遍再比前缀。
///
/// `Ok(None)` = 条目不存在（上游 `os.ErrNotExist` ⇒ `(nil, false, nil)`）。
fn read_local_entry(root: &FsPath, entry: &str) -> Result<Option<Vec<u8>>, BundleError> {
    let root = clean_path(root);
    let path = clean_path(&root.join(entry));
    if path != root && !path.starts_with(&root) {
        return Err(BundleError::Read {
            name: entry.to_owned(),
            reason: "escapes its directory".to_owned(),
        });
    }
    match std::fs::read(&path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BundleError::Read {
            name: entry.to_owned(),
            reason: error.to_string(),
        }),
    }
}

/// 词法清洗一个路径（等价上游 `filepath.Clean`；**不**解析符号链接，所以不要求路径存在）。
fn clean_path(path: &FsPath) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                cleaned.pop();
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

// ---------------------------------------------------------------------------
// 门 / 错误
// ---------------------------------------------------------------------------

/// `requirePluginsV1` + `workspaceMember`（发布面的两道门；上游顺序也是门在前、收体在后）。
/// `requirePluginsV1` + `workspaceAdmin`（`plugins/packages*` 四条路由都在 admin 组里）。
async fn admin_scope(state: &AppState, raw_workspace: &str, user_id: Id) -> PluginResult<Id> {
    require_plugins_v1(state)?;
    workspace_admin(state, raw_workspace, user_id).await
}

/// `DELETE /packages/{packageId}` 的路径参数：非 uuid ⇒ **404**（上游 `PluginErrorNotFound`，
/// 不是 400 —— 路径里的 id 不是「请求体里的参数」）。
fn parse_package_id(raw: &str) -> PluginResult<Id> {
    Id::parse(raw.trim()).map_err(|_| PluginError::not_found("plugin package not found"))
}

/// 兜底降级：`NotFound` 落「包不存在」，其余（含 driver 报错）落 502，文案是**操作名**
/// （上游把底层错误只写进 `Err`，不回给客户端）。调用点先分派有意义的 `Conflict`。
fn failed(error: RepoError, operation: &str) -> PluginError {
    match error {
        RepoError::NotFound => PluginError::not_found("plugin package not found"),
        RepoError::Conflict => PluginError::conflict("plugin package conflict"),
        RepoError::Db(detail) => {
            tracing::error!(error = %detail, operation, "plugin package database failure");
            PluginError::unavailable(operation)
        }
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
