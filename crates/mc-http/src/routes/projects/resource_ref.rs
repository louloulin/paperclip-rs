//! `resource_type` 的载荷校验与归一化 + `local_directory` 的 worktree 能力门。
//!
//! 逐字移植上游 `handler/project_resource.go`：
//! `validateAndNormalizeResourceRef` / `validateGithubRepoRef` / `validateLocalDirectoryRef` /
//! `isValidGitRepoURL` / `validateGitRef` / `isAbsoluteLocalPath` /
//! `requireWorktreeCapableDaemon` / `daemonAdvertisesWorktree` / `runtimeSeenAfter` /
//! `latestDaemonCLIVersion` / `localDirectoryRefLabel` / `localDirectoryRefDiffersOnlyByLabel` /
//! `withLocalDirectoryRefLabel`。
//!
//! 两处实现细节上的差异（结论等价）：
//! - 上游用 `net/url.Parse`；本仓无 `url` 依赖且本片不得新增依赖（`docs/42` §6.3），改为
//!   手写 scheme/host 判定（见 [`is_valid_git_repo_url`]），并把边界写成单元测试。
//! - 上游 `localDirectoryRefDiffersOnlyByLabel` 用 `reflect.DeepEqual`；本仓用
//!   `serde_json::Value` 的 `==`（对象比较与键序无关，两边都保留未知键）。

use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::runtime::AgentRuntimeRow;
use serde_json::{json, Map, Value as JsonValue};

use crate::state::AppState;

use super::dto::{
    GithubRepoRef, LocalDirectoryRef, LOCAL_DIRECTORY_MODE_IN_PLACE, LOCAL_DIRECTORY_MODE_WORKTREE,
};
use super::helpers::{validation, workspace_runtimes};

/// `gitRefMaxLength`（上游同名常量）：宽松 ref 名要能塞进文件系统中的一个文件名。
const GIT_REF_MAX_LENGTH: usize = 255;

/// 本机客户端支持 `local_directory` worktree 模式的最低版本
/// （上游 `pkg/agent.MinLocalWorktreeCLIVersion`，`pkg/agent/version.go:68`）。
pub(crate) const MIN_LOCAL_WORKTREE_CLI_VERSION: &str = "0.4.24";

/// `local_directory` 的 worktree 能力在 daemon metadata 里的 capability 名
/// （上游 `protocol.DaemonCapabilityLocalWorktreeV1`）。
const DAEMON_CAPABILITY_LOCAL_WORKTREE_V1: &str =
    mc_daemon_proto::DAEMON_CAPABILITY_LOCAL_WORKTREE_V1;

// ---------------------------------------------------------------------------
// 归一化 / 校验
// ---------------------------------------------------------------------------

/// 上游 `validateAndNormalizeResourceRef`：未知 `resource_type` 与非法载荷都在 API 边界
/// 拦下，返回**归一化后**的 JSONB（未知键被丢弃，与 Go 的 re-marshal 行为一致）。
///
/// `ref` 是 `None` 对应 Go 的零长 `json.RawMessage`（key 缺失）⇒ `resource_ref is required`。
pub(crate) fn validate_and_normalize_resource_ref(
    resource_type: &str,
    raw: Option<&JsonValue>,
) -> Result<JsonValue, Error> {
    let Some(raw) = raw else {
        return Err(validation("resource_ref is required"));
    };
    match resource_type {
        "github_repo" => validate_github_repo_ref(raw),
        "local_directory" => validate_local_directory_ref(raw),
        other => Err(validation(format!("unknown resource_type \"{other}\""))),
    }
}

/// 上游 `validateGithubRepoRef`。
fn validate_github_repo_ref(raw: &JsonValue) -> Result<JsonValue, Error> {
    let payload: GithubRepoRef = serde_json::from_value(raw.clone())
        .map_err(|err| validation(format!("invalid github_repo payload: {err}")))?;
    let url = payload.url.trim().to_string();
    if url.is_empty() {
        return Err(validation("github_repo: url is required"));
    }
    if !is_valid_git_repo_url(&url) {
        return Err(validation(
            "github_repo: url must be a valid http(s) or ssh git URL",
        ));
    }
    let default_branch_hint = payload.default_branch_hint.trim().to_string();
    let git_ref = payload.ref_.trim().to_string();
    validate_git_ref(&git_ref).map_err(|err| validation(format!("github_repo: {err}")))?;
    serde_json::to_value(GithubRepoRef {
        url,
        default_branch_hint,
        ref_: git_ref,
    })
    .map_err(|err| Error::Internal(format!("failed to encode github_repo: {err}")))
}

/// 上游 `validateLocalDirectoryRef`。
fn validate_local_directory_ref(raw: &JsonValue) -> Result<JsonValue, Error> {
    let payload: LocalDirectoryRef = serde_json::from_value(raw.clone())
        .map_err(|err| validation(format!("invalid local_directory payload: {err}")))?;
    let local_path = payload.local_path.trim().to_string();
    if local_path.is_empty() {
        return Err(validation("local_directory: local_path is required"));
    }
    if !is_absolute_local_path(&local_path) {
        return Err(validation(
            "local_directory: local_path must be an absolute path",
        ));
    }
    let daemon_id = payload.daemon_id.trim().to_string();
    if daemon_id.is_empty() {
        return Err(validation("local_directory: daemon_id is required"));
    }
    let label = payload.label.trim().to_string();
    let execution_mode = payload.execution_mode.trim().to_string();
    match execution_mode.as_str() {
        "" | LOCAL_DIRECTORY_MODE_IN_PLACE | LOCAL_DIRECTORY_MODE_WORKTREE => {}
        other => {
            return Err(validation(format!(
                "local_directory: execution_mode must be \"{LOCAL_DIRECTORY_MODE_IN_PLACE}\" or \"{LOCAL_DIRECTORY_MODE_WORKTREE}\", got \"{other}\""
            )))
        }
    }
    serde_json::to_value(LocalDirectoryRef {
        local_path,
        daemon_id,
        label,
        execution_mode,
    })
    .map_err(|err| Error::Internal(format!("failed to encode local_directory: {err}")))
}

/// 上游 `validateGitRef`：只查形状（空 ref 合法 = 用仓库默认分支）。
pub(crate) fn validate_git_ref(git_ref: &str) -> Result<(), Error> {
    if git_ref.is_empty() {
        return Ok(());
    }
    if git_ref.chars().count() > GIT_REF_MAX_LENGTH {
        return Err(validation(format!(
            "ref must be at most {GIT_REF_MAX_LENGTH} characters"
        )));
    }
    for ch in git_ref.chars() {
        // 控制字符、DEL、空格，以及 git 修订语法自留的字符。
        if ch <= ' ' || ch == '\u{7f}' || "~^:?*[\\".contains(ch) {
            return Err(validation(
                "ref must not contain spaces, control characters, or any of ~ ^ : ? * [ \\",
            ));
        }
    }
    if git_ref.contains("..")
        || git_ref.contains("@{")
        || git_ref == "@"
        || git_ref.starts_with('/')
        || git_ref.ends_with('/')
        || git_ref.contains("//")
        || git_ref.ends_with('.')
    {
        return Err(validation("ref is not a valid branch, tag, or commit"));
    }
    for segment in git_ref.split('/') {
        if segment.starts_with('.') || segment.ends_with(".lock") {
            return Err(validation("ref is not a valid branch, tag, or commit"));
        }
    }
    Ok(())
}

/// 上游 `isValidGitRepoURL`：接受 GitHub "Code" 菜单能粘贴出的三种形态
/// （`https://` / 带 scheme 的 `ssh://`、`git://` / scp 简写 `git@host:owner/repo.git`）。
pub(crate) fn is_valid_git_repo_url(raw: &str) -> bool {
    if let Some((scheme, rest)) = split_scheme(raw) {
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("");
        if !host.is_empty() && matches!(scheme, "http" | "https" | "ssh" | "git") {
            return true;
        }
    }
    // scp 简写：[user@]host:path，host 与 path 都非空、且不含空格。
    // 带 `://` 的一律已被上面的 scheme 分支判过（Go 侧同样直接 false）。
    if raw.contains(' ') || raw.contains("://") {
        return false;
    }
    let Some(colon) = raw.find(':') else {
        return false;
    };
    if colon == 0 || colon == raw.len() - 1 {
        return false;
    }
    // `[user@]host:path` 里的 `@` 只有出现在第一个 `:` 之前才是用户名分隔符。
    let at = raw.find('@');
    if at.is_some_and(|at| at >= colon) {
        return false;
    }
    let host_start = at.map_or(0, |at| at + 1);
    let host = &raw[host_start..colon];
    let path = &raw[colon + 1..];
    !host.is_empty() && !path.is_empty()
}

/// `scheme://rest` 拆分；无合法 scheme（RFC 3986：字母开头 + `[A-Za-z0-9+.-]*`）→ `None`。
fn split_scheme(raw: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = raw.split_once("://")?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some((scheme, rest))
}

/// 上游 `isAbsoluteLocalPath`：POSIX 前导 `/`、UNC 前缀 `\\`、或盘符 `C:\` / `C:/`。
pub(crate) fn is_absolute_local_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') {
        return true;
    }
    if path.starts_with(r"\\") {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// 上游 `localDirectoryRefLabel`：读 ref 内嵌 label（trim），读不出按无标签。
pub(crate) fn local_directory_ref_label(raw: &JsonValue) -> String {
    serde_json::from_value::<LocalDirectoryRef>(raw.clone())
        .map(|payload| payload.label.trim().to_string())
        .unwrap_or_default()
}

/// 上游 `localDirectoryRefDiffersOnlyByLabel`：两边都成功解成对象、抹掉 `label` 后相等。
///
/// **未知键参与比较**：本二进制看不懂的差异也是差异，把这种请求当纯重命名是猜测。
pub(crate) fn local_directory_ref_differs_only_by_label(a: &JsonValue, b: &JsonValue) -> bool {
    fn strip(raw: &JsonValue) -> Option<JsonValue> {
        let obj = raw.as_object()?;
        let mut map = Map::new();
        for (key, value) in obj {
            if key != "label" {
                map.insert(key.clone(), value.clone());
            }
        }
        Some(JsonValue::Object(map))
    }
    match (strip(a), strip(b)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// 上游 `withLocalDirectoryRefLabel`：只改写/删除 `label` 键，其余键**逐字保留**
/// （绝不 re-marshal 成 [`LocalDirectoryRef`]，否则会丢掉更新版本写入的字段）。
pub(crate) fn with_local_directory_ref_label(
    raw: &JsonValue,
    label: Option<&str>,
) -> Result<JsonValue, Error> {
    let Some(obj) = raw.as_object() else {
        return Err(Error::Internal(
            "local_directory ref is not a JSON object".into(),
        ));
    };
    let mut map = obj.clone();
    match label {
        Some(label) => {
            map.insert("label".into(), JsonValue::String(label.to_string()));
        }
        None => {
            map.remove("label");
        }
    }
    Ok(JsonValue::Object(map))
}

// ---------------------------------------------------------------------------
// worktree 能力门
// ---------------------------------------------------------------------------

/// `local_directory` 是否要求 worktree 模式。
pub(crate) fn wants_worktree(resource_type: &str, normalized_ref: &JsonValue) -> bool {
    if resource_type != "local_directory" {
        return false;
    }
    serde_json::from_value::<LocalDirectoryRef>(normalized_ref.clone())
        .is_ok_and(|ref_| ref_.execution_mode == LOCAL_DIRECTORY_MODE_WORKTREE)
}

/// 上游 `requireWorktreeCapableDaemon`：`worktree` 模式要求对应 daemon 的最新运行时行
/// **实际宣告**了 worktree 能力；否则 422 扁平体（`code: daemon_version_unsupported`）。
///
/// 返回 `Ok(())` 放行；`Err(Response)` 是已经写好的 422（客户端按 `code` 分支）。
/// 版本号本身不能回答这个问题：自构建的 daemon 报的是 git-describe 串，被版本下限
/// 故意豁免（MUL-5707）。
pub(crate) async fn require_worktree_capable_daemon(
    state: &AppState,
    workspace_id: Id,
    resource_type: &str,
    normalized_ref: &JsonValue,
) -> Result<(), Box<Response>> {
    if !wants_worktree(resource_type, normalized_ref) {
        return Ok(());
    }
    let Ok(ref_) = serde_json::from_value::<LocalDirectoryRef>(normalized_ref.clone()) else {
        return Ok(());
    };
    let runtimes = match workspace_runtimes(state, workspace_id).await {
        Ok(runtimes) => runtimes,
        Err(err) => {
            return Err(Box::new(
                crate::error::ApiError(err)
                    .respond_with(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
            ))
        }
    };
    if daemon_advertises_worktree(&runtimes, &ref_.daemon_id) {
        return Ok(());
    }
    Err(Box::new(
        (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": format!(
                    "local_directory: \"{}\" is set to parallel (worktree) mode, but the Multica runtime on that machine does not support it. Update the Multica app on that machine to the latest version, or keep the resource on in_place.",
                    ref_.local_path
                ),
                "code": "daemon_version_unsupported",
                "current_version": latest_daemon_cli_version(&runtimes, &ref_.daemon_id),
                "min_version": MIN_LOCAL_WORKTREE_CLI_VERSION,
                "daemon_id": ref_.daemon_id,
            })),
        )
            .into_response(),
    ))
}

/// 上游 `daemonAdvertisesWorktree`：**最新一帧**运行时行说了算（newest-wins）。
///
/// 故意不是「任意一行宣告过」：注销运行时只是把行翻成 offline、metadata 还在，
/// 一台降级过的机器会同时留着旧的能力行 ⇒ any-match 会永远说 yes。
fn daemon_advertises_worktree(runtimes: &[AgentRuntimeRow], daemon_id: &str) -> bool {
    if daemon_id.trim().is_empty() {
        return false;
    }
    let mut newest: Option<&AgentRuntimeRow> = None;
    for row in runtimes {
        if row.daemon_id.as_deref() != Some(daemon_id) {
            continue;
        }
        match newest {
            // 从未有行时当前行即最新。
            None => newest = Some(row),
            Some(current) => {
                if runtime_seen_after(row, current) {
                    newest = Some(row);
                }
            }
        }
    }
    let Some(newest) = newest else {
        return false;
    };
    let metadata = serde_json::to_vec(&newest.metadata).ok();
    mc_daemon_proto::runtime_has_capability(
        metadata.as_deref(),
        DAEMON_CAPABILITY_LOCAL_WORKTREE_V1,
    )
}

/// 上游 `runtimeSeenAfter`：从未上报（NULL）的行排序最旧。
fn runtime_seen_after(candidate: &AgentRuntimeRow, current: &AgentRuntimeRow) -> bool {
    match (candidate.last_seen_at, current.last_seen_at) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(candidate), Some(current)) => candidate > current,
    }
}

/// 上游 `latestDaemonCLIVersion`：最新一帧**带版本号**的运行时行的 `cli_version`。
fn latest_daemon_cli_version(runtimes: &[AgentRuntimeRow], daemon_id: &str) -> String {
    let mut current = String::new();
    let mut current_seen: Option<chrono::DateTime<chrono::Utc>> = None;
    for row in runtimes {
        if row.daemon_id.as_deref() != Some(daemon_id) {
            continue;
        }
        let version = read_runtime_cli_version(&row.metadata);
        if version.is_empty() {
            continue;
        }
        let newer = current.is_empty()
            || row
                .last_seen_at
                .zip(current_seen)
                .is_some_and(|(seen, previous)| seen > previous);
        if newer {
            current = version;
            current_seen = row.last_seen_at;
        }
    }
    current
}

/// 上游 `readRuntimeCLIVersion`：读 `metadata.cli_version` 字符串。
fn read_runtime_cli_version(metadata: &JsonValue) -> String {
    metadata
        .get("cli_version")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_url_accepts_https_ssh_and_scp_forms() {
        for ok in [
            "https://github.com/louloulin/paperclip-rs",
            "https://github.com/louloulin/paperclip-rs.git",
            "http://example.com/x",
            "ssh://git@github.com/louloulin/paperclip-rs.git",
            "git://github.com/x/y.git",
            "git@github.com:louloulin/paperclip-rs.git",
            "github.com:owner/repo.git",
        ] {
            assert!(is_valid_git_repo_url(ok), "should accept {ok}");
        }
    }

    #[test]
    fn git_url_rejects_garbage() {
        for bad in [
            "not-a-url",
            "",
            "https://",
            "ftp://github.com/x/y",
            "git@github.com",
            ":owner/repo",
            "git@github.com:",
            "git@github.com:owner/repo:extra@x",
            "https://github.com/a b",
        ] {
            assert!(!is_valid_git_repo_url(bad), "should reject {bad}");
        }
    }

    #[test]
    fn git_ref_shape_rules() {
        assert!(validate_git_ref("").is_ok());
        assert!(validate_git_ref("main").is_ok());
        assert!(validate_git_ref("feature/x-1").is_ok());
        assert!(validate_git_ref("v1.2.3").is_ok());
        for bad in [
            " a",
            "a b",
            "a..b",
            "@",
            "a@{0}",
            "/a",
            "a/",
            "a//b",
            "a.",
            ".a",
            "a.lock/x.lock",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
        ] {
            assert!(validate_git_ref(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn absolute_path_forms() {
        assert!(is_absolute_local_path("/home/devbox/repo"));
        assert!(is_absolute_local_path(r"\\server\share"));
        assert!(is_absolute_local_path(r"C:\src"));
        assert!(is_absolute_local_path("C:/src"));
        assert!(!is_absolute_local_path(""));
        assert!(!is_absolute_local_path("src/repo"));
        assert!(!is_absolute_local_path("C:src"));
    }
}
