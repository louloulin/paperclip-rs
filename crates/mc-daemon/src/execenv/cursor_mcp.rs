//! Cursor 的托管 MCP sidecar：`.cursor/mcp.json`、Cursor 数据目录里的 approvals 与信任标记、
//! 可选的 `mcp-auth.json` 播种。
//!
//! - **上游**：`execenv/cursor_mcp.go`（361 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 为什么要在 Cursor 的数据目录里「伪造」这份状态
//!
//! Cursor 只运行**已被批准**的 MCP server，而批准状态是以
//! `<dataDir>/projects/<slug>/mcp-approvals.json` 里的一串标识符数组表达的。标识符的算法是
//! Cursor 自己的：`<name>-<sha256({"path":<projectRoot>,"server":<规范化后的 server>})[:16]>`。
//! 我们不生成这些标识符，用户每起一次任务就要在 UI 里再点一次「批准」—— 于是 daemon 直接
//! 把它算出来写进去（连同 `.workspace-trusted`），并**绝不覆盖**用户已有的文件。
//!
//! ## 字节级契约（算错就是「每次都要重新批准」）
//!
//! `server` 这一段的字节必须与 Cursor 的 JSON.stringify 一致：
//! - stdio 服务器**只保留** `type` / `command` / `args` / `env` / `cwd`，且按这个顺序；
//! - remote 服务器只保留 `type` / `url` / `headers`，同样按顺序；
//! - 两者都不认识的形状（既没有 `command` 也没有 `url`）**原样紧凑输出**；
//! - 不转义 HTML（`&`、`<`、`>` 保持原字符），与 JS 的 `JSON.stringify` 一致。
//!
//! 本仓的 `serde_json` 默认就不转义 HTML，字段序由下面几个 `#[derive(Serialize)]` 的结构体
//! 固定。已知残余差异（登记在 `docs/32` §9.9）：Go 侧 `json.Encoder` 会把 U+2028/U+2029
//! 转义成 `\u2028`，而本实现原样写出。
//!
//! ## 与上游的差异
//!
//! - `recordMkdirAll` / `recordWriteFile`（`sidecar_manifest.go`）→
//!   [`crate::execenv::sidecar`] 的「拒绝覆盖」版（详见该模块文档）。
//! - `sidecarManifest` 记账不做（本 slice 不落账本）；`cursor-data/` 随 env root 一起被 GC。

use std::fs;
use std::path::{Path, PathBuf};

use mc_core::hash::ContentHash;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::execenv::sidecar::{
    create_dir_all, remove_file_if_present, write_new_file, SidecarError,
};
use crate::skill::{non_empty_env, user_home};

/// agent 自定义 env 键：显式指定 `mcp-auth.json`（或含它的 Cursor 项目数据目录）。
///
/// 故意**不**叫 `MULTICA_*`：`custom_env` 会挡住用户设置 `MULTICA_` 前缀的键。
pub const CURSOR_MCP_AUTH_SOURCE_ENV: &str = "CURSOR_MCP_AUTH_SOURCE";

/// Cursor 的「工作区已信任」标记文件名。
pub const CURSOR_WORKSPACE_TRUSTED_FILE: &str = ".workspace-trusted";

/// Cursor 的 MCP 授权文件名。
pub const CURSOR_MCP_AUTH_FILE: &str = "mcp-auth.json";

/// 这个 `mcp_config` 是不是「显式的托管配置」。
///
/// `null` / 缺省 ⇒ `false`（「让 Cursor 照常行事」），于是不建 `.cursor/mcp.json`、也不设
/// `CURSOR_DATA_DIR`。空对象在本函数里**算托管**（上游只判 `len(trimmed) > 0`），因为它
/// 明确表达了「这个 agent 管着 MCP」，与「没配」不同。
#[must_use]
pub fn has_managed_cursor_mcp_config(mcp_config: Option<&Value>) -> bool {
    match mcp_config {
        None | Some(Value::Null) => false,
        Some(Value::Object(_) | _) => true,
    }
}

/// 解析 `mcp_config` 里的 `mcpServers`（上游 `parseCursorManagedMcpServers`）。
///
/// 每个 server 必须是一个 JSON **对象**（`null`、字符串、数组都拒）；名字不能是空白。
pub fn parse_cursor_managed_mcp_servers(
    mcp_config: &Value,
) -> Result<Map<String, Value>, SidecarError> {
    let Value::Object(config) = mcp_config else {
        return Err(SidecarError::Invalid(
            "parse mcp_config json: not a JSON object".to_string(),
        ));
    };
    let Some(servers) = config.get("mcpServers") else {
        return Ok(Map::new());
    };
    let Value::Object(servers) = servers else {
        return Err(SidecarError::Invalid(
            "parse mcp_config json: mcpServers must be an object".to_string(),
        ));
    };
    for (name, server) in servers {
        if name.trim().is_empty() {
            return Err(SidecarError::Invalid(
                "mcp server name must not be empty".to_string(),
            ));
        }
        if !server.is_object() {
            return Err(SidecarError::Invalid(format!(
                "mcp_servers.{name} must be a JSON object"
            )));
        }
    }
    Ok(servers.clone())
}

/// `.cursor/mcp.json` 的文档形状。
#[derive(Debug, Serialize)]
struct CursorMcpConfigFile<'a> {
    #[serde(rename = "mcpServers")]
    mcp_servers: &'a Map<String, Value>,
}

/// 序列化 `.cursor/mcp.json`（上游 `marshalCursorMcpConfig`：两空格缩进 + 末尾换行）。
pub fn marshal_cursor_mcp_config(servers: &Map<String, Value>) -> Result<String, SidecarError> {
    let document = CursorMcpConfigFile {
        mcp_servers: servers,
    };
    let mut raw = serde_json::to_string_pretty(&document)
        .map_err(|err| SidecarError::Invalid(format!("marshal cursor mcp config: {err}")))?;
    raw.push('\n');
    Ok(raw)
}

/// stdio server 在算批准键前的**规范化形状**（字段序 = Cursor 的）。
#[derive(Debug, Serialize)]
struct CursorStdioApprovalServer<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<&'a Value>,
    command: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a Value>,
}

/// remote server 的规范化形状。
#[derive(Debug, Serialize)]
struct CursorRemoteApprovalServer<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<&'a Value>,
    url: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<&'a Value>,
}

/// 把 server 规范化成参与哈希的那段字节（上游 `marshalCursorMcpApprovalServer`）。
pub fn marshal_cursor_mcp_approval_server(server: &Value) -> Result<String, SidecarError> {
    let Value::Object(fields) = server else {
        return Err(SidecarError::Invalid(
            "cursor approval server must be a JSON object".to_string(),
        ));
    };
    if let Some(command) = fields.get("command") {
        return serde_json::to_string(&CursorStdioApprovalServer {
            r#type: fields.get("type"),
            command,
            args: fields.get("args"),
            env: fields.get("env"),
            cwd: fields.get("cwd"),
        })
        .map_err(|err| SidecarError::Invalid(format!("marshal cursor approval server: {err}")));
    }
    if let Some(url) = fields.get("url") {
        return serde_json::to_string(&CursorRemoteApprovalServer {
            r#type: fields.get("type"),
            url,
            headers: fields.get("headers"),
        })
        .map_err(|err| SidecarError::Invalid(format!("marshal cursor approval server: {err}")));
    }
    serde_json::to_string(server)
        .map_err(|err| SidecarError::Invalid(format!("marshal cursor approval server: {err}")))
}

/// 算出 Cursor 会认的批准标识符（上游 `cursorMcpApprovalKeys`）。
///
/// 按 server 名**升序**；每个键 = `<name>-<sha256(payload)[:16]>`，payload 是
/// `{"path":<json(projectRoot)>,"server":<规范化 server>}` 这段**手工拼出来的**字节
/// （顺序与空白都固定，不能改成「等价的对象」）。
pub fn cursor_mcp_approval_keys(
    project_root: &Path,
    servers: &Map<String, Value>,
) -> Result<Vec<String>, SidecarError> {
    let project_root_slash = project_root.to_string_lossy().replace('\\', "/");
    let root_json = serde_json::to_string(&Value::String(project_root_slash))
        .map_err(|err| SidecarError::Invalid(format!("marshal cursor project root: {err}")))?;

    let mut approvals = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        let server_json = marshal_cursor_mcp_approval_server(server)?;
        let payload = format!("{{\"path\":{root_json},\"server\":{server_json}}}");
        let digest = ContentHash::sha256(payload.as_bytes());
        let hex = digest.as_str();
        // `[:16]`：前 8 字节（hex 密文截断后是 16 个字符）。
        let short = hex.get(..16).unwrap_or(hex);
        approvals.push(format!("{name}-{short}"));
    }
    Ok(approvals)
}

/// Cursor 的项目根（上游 `cursorProjectRoot`）：解析符号链接 → 绝对化 → 向上找 `.git`。
///
/// 找不到 `.git`（一直到根都没有）⇒ 回落成**解析 + 绝对化之后**的那个目录。
#[must_use]
pub fn cursor_project_root(work_dir: &Path) -> PathBuf {
    if work_dir.as_os_str().is_empty() {
        return work_dir.to_path_buf();
    }
    let resolved = fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let absolute = if resolved.is_absolute() {
        resolved.clone()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&resolved))
            .unwrap_or(resolved)
    };
    let fallback = absolute.clone();
    let mut current = absolute;
    loop {
        if current.join(".git").exists() {
            return current;
        }
        match current.parent() {
            Some(parent) => {
                if parent == current {
                    return fallback;
                }
                current = parent.to_path_buf();
            }
            None => return fallback,
        }
    }
}

/// Cursor 数据目录里用的项目 slug（上游 `cursorSlugifyPath`）。
///
/// 字母数字保留，其余**连续**非字母数字折成一个 `-`，最后去掉首尾 `-`。
#[must_use]
pub fn cursor_slugify_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut last_dash = false;
    for ch in path.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
            continue;
        }
        if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// 解析 `CURSOR_MCP_AUTH_SOURCE`（上游 `resolveCursorMcpAuthSource`）。
///
/// 允许「`~` 展开」「目录（自动接 `mcp-auth.json`）」，但最终**必须**是一个名为
/// `mcp-auth.json` 的**文件**，且必须是绝对路径。
pub fn resolve_cursor_mcp_auth_source(source: &str) -> Result<PathBuf, SidecarError> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(SidecarError::Invalid(format!(
            "{CURSOR_MCP_AUTH_SOURCE_ENV} is empty"
        )));
    }
    let mut candidate = trimmed.to_string();
    if candidate == "~" || candidate.starts_with("~/") {
        let home = user_home().map_err(|_| {
            SidecarError::Invalid(format!(
                "resolve {CURSOR_MCP_AUTH_SOURCE_ENV} home directory"
            ))
        })?;
        candidate = if candidate == "~" {
            home.to_string_lossy().to_string()
        } else {
            home.join(&candidate[2..]).to_string_lossy().to_string()
        };
    }
    if !Path::new(&candidate).is_absolute() {
        return Err(SidecarError::Invalid(format!(
            "{CURSOR_MCP_AUTH_SOURCE_ENV} must be an absolute path to {CURSOR_MCP_AUTH_FILE} or its containing Cursor project directory"
        )));
    }
    let mut path = PathBuf::from(candidate);
    let metadata = fs::metadata(&path).map_err(|err| SidecarError::Io {
        op: "stat CURSOR_MCP_AUTH_SOURCE",
        path: path.clone(),
        source: err,
    })?;
    if metadata.is_dir() {
        path = path.join(CURSOR_MCP_AUTH_FILE);
        let metadata = fs::metadata(&path).map_err(|err| SidecarError::Io {
            op: "stat CURSOR_MCP_AUTH_SOURCE mcp-auth.json",
            path: path.clone(),
            source: err,
        })?;
        if metadata.is_dir() {
            return Err(SidecarError::Invalid(format!(
                "{CURSOR_MCP_AUTH_SOURCE_ENV} must resolve to a file, got directory {}",
                path.display()
            )));
        }
    }
    if path
        .file_name()
        .is_none_or(|name| name != CURSOR_MCP_AUTH_FILE)
    {
        return Err(SidecarError::Invalid(format!(
            "{CURSOR_MCP_AUTH_SOURCE_ENV} must point at {CURSOR_MCP_AUTH_FILE}, got {}",
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        )));
    }
    Ok(path)
}

/// 把用户那份 `mcp-auth.json` 播种到任务的数据目录（上游 `seedCursorMcpAuthFile`）：
/// 先试符号链接，失败再拷贝（拷贝用 `O_EXCL`，绝不覆盖既有文件）。
pub fn seed_cursor_mcp_auth_file(
    project_data_dir: &Path,
    source: &str,
) -> Result<(), SidecarError> {
    let source_path = resolve_cursor_mcp_auth_source(source)?;
    let target = project_data_dir.join(CURSOR_MCP_AUTH_FILE);
    if create_file_link(&source_path, &target).is_ok() {
        return Ok(());
    }
    copy_new_file(&source_path, &target).map_err(|err| match err {
        SidecarError::Io { source, .. } => {
            SidecarError::Invalid(format!("seed cursor mcp auth file: {source}"))
        }
        other => other,
    })
}

#[cfg(unix)]
fn create_file_link(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, destination)
}

#[cfg(not(unix))]
fn create_file_link(source: &Path, destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "file links are not implemented on this platform: {} -> {}",
            destination.display(),
            source.display()
        ),
    ))
}

/// 拷一份新文件（不覆盖）。
fn copy_new_file(source: &Path, target: &Path) -> Result<(), SidecarError> {
    let data = fs::read(source).map_err(|err| SidecarError::Io {
        op: "read cursor mcp auth source",
        path: source.to_path_buf(),
        source: err,
    })?;
    write_new_file(target, &data)
}

/// 删掉数据目录里的 `mcp-auth.json`（上游 `removeCursorMcpAuthFile`）。
pub fn remove_cursor_mcp_auth_file(project_data_dir: &Path) -> Result<(), SidecarError> {
    remove_file_if_present(&project_data_dir.join(CURSOR_MCP_AUTH_FILE)).map_err(|err| match err {
        SidecarError::Io { path, source, .. } => SidecarError::Io {
            op: "remove prior cursor mcp auth file",
            path,
            source,
        },
        other => other,
    })
}

/// 写 Cursor 的 MCP sidecar（上游 `prepareCursorMcpConfig`）。
///
/// 返回要设给 `CURSOR_DATA_DIR` 的目录；`mcp_config` 为 `null`/缺省 ⇒ `Ok(None)`。
pub fn prepare_cursor_mcp_config(
    env_root: &Path,
    work_dir: &Path,
    mcp_config: Option<&Value>,
    mcp_auth_source: &str,
) -> Result<Option<PathBuf>, SidecarError> {
    if !has_managed_cursor_mcp_config(mcp_config) {
        return Ok(None);
    }
    if env_root.as_os_str().is_empty() {
        return Err(SidecarError::Invalid(
            "env root is required for managed cursor mcp_config".to_string(),
        ));
    }
    let mcp_config = mcp_config.unwrap_or(&Value::Null);
    let project_root = cursor_project_root(work_dir);
    let servers = parse_cursor_managed_mcp_servers(mcp_config)?;

    let cursor_dir = project_root.join(".cursor");
    create_dir_all(&cursor_dir)?;
    let config_data = marshal_cursor_mcp_config(&servers)?;
    match write_new_file(&cursor_dir.join("mcp.json"), config_data.as_bytes()) {
        Ok(()) => {}
        Err(err) if err.is_pre_existing() => {
            return Err(SidecarError::Invalid(
                "managed cursor mcp_config would overwrite existing .cursor/mcp.json".to_string(),
            ));
        }
        Err(err) => return Err(err),
    }

    let cursor_data_dir = env_root.join("cursor-data");
    let project_data_dir = cursor_data_dir
        .join("projects")
        .join(cursor_slugify_path(&project_root.to_string_lossy()));
    create_dir_all(&project_data_dir)?;
    remove_cursor_mcp_auth_file(&project_data_dir)?;

    let approvals = cursor_mcp_approval_keys(&project_root, &servers)?;
    let approval_data = serde_json::to_vec_pretty(&approvals)
        .map_err(|err| SidecarError::Invalid(format!("marshal cursor mcp approvals: {err}")))?;
    // approvals 是**我们自己上一轮**写的内容，整体重写（上游同：`os.WriteFile`）。
    let approvals_path = project_data_dir.join("mcp-approvals.json");
    fs::write(&approvals_path, approval_data).map_err(|err| SidecarError::Io {
        op: "write cursor mcp approvals",
        path: approvals_path.clone(),
        source: err,
    })?;

    let trust_data = serde_json::to_vec_pretty(&serde_json::json!({
        "trustedAt": "1970-01-01T00:00:00Z",
        "workspacePath": project_root.to_string_lossy(),
        "trustMethod": "multica-managed",
    }))
    .map_err(|err| SidecarError::Invalid(format!("marshal cursor workspace trust: {err}")))?;
    let trust_path = project_data_dir.join(CURSOR_WORKSPACE_TRUSTED_FILE);
    fs::write(&trust_path, trust_data).map_err(|err| SidecarError::Io {
        op: "write cursor workspace trust",
        path: trust_path.clone(),
        source: err,
    })?;

    if !mcp_auth_source.trim().is_empty() {
        seed_cursor_mcp_auth_file(&project_data_dir, mcp_auth_source)?;
    }

    Ok(Some(cursor_data_dir))
}

/// `CURSOR_MCP_AUTH_SOURCE` 的现值（trim 过；空 ⇒ `None`）。
#[must_use]
pub fn cursor_mcp_auth_source_from_env() -> Option<String> {
    non_empty_env(CURSOR_MCP_AUTH_SOURCE_ENV)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mc-daemon-cursor-mcp-{name}-{nanos:x}"));
            fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            self.0.as_path()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn managed_config_detection_matches_upstream() {
        assert!(!has_managed_cursor_mcp_config(None));
        assert!(!has_managed_cursor_mcp_config(Some(&Value::Null)));
        assert!(has_managed_cursor_mcp_config(Some(&json!({}))));
        assert!(has_managed_cursor_mcp_config(Some(
            &json!({"mcpServers": {}})
        )));
    }

    #[test]
    fn managed_servers_must_be_objects_and_names_must_not_be_blank() {
        let servers = parse_cursor_managed_mcp_servers(&json!({
            "mcpServers": {"ok": {"command": "run"}}
        }))
        .expect("parse");
        assert_eq!(servers.len(), 1);

        // 没有 `mcpServers` ⇒ 空表（不是错误）。
        assert!(parse_cursor_managed_mcp_servers(&json!({}))
            .expect("parse")
            .is_empty());

        for bad in [
            json!({"mcpServers": {"": {"command": "x"}}}),
            json!({"mcpServers": {"a": null}}),
            json!({"mcpServers": {"a": "string"}}),
            json!({"mcpServers": []}),
        ] {
            assert!(
                parse_cursor_managed_mcp_servers(&bad).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn approval_payload_uses_the_normalized_field_order() {
        // stdio：只保留 type/command/args/env/cwd，且按这个顺序。
        let stdio = json!({
            "command": "run", "cwd": "/w", "env": {"A": "1"}, "args": ["--x"],
            "type": "stdio", "ignored": "drop me"
        });
        assert_eq!(
            marshal_cursor_mcp_approval_server(&stdio).expect("marshal"),
            r#"{"type":"stdio","command":"run","args":["--x"],"env":{"A":"1"},"cwd":"/w"}"#
        );

        // remote：只保留 type/url/headers。
        let remote = json!({"url": "https://x", "headers": {"H": "1"}, "extra": true});
        assert_eq!(
            marshal_cursor_mcp_approval_server(&remote).expect("marshal"),
            r#"{"url":"https://x","headers":{"H":"1"}}"#
        );

        // 两者都没有 ⇒ 原样紧凑输出。
        let other = json!({"whatever": 1});
        assert_eq!(
            marshal_cursor_mcp_approval_server(&other).expect("marshal"),
            r#"{"whatever":1}"#
        );
    }

    #[test]
    fn approval_keys_are_name_plus_sixteen_hex_characters_in_name_order() {
        let mut servers = Map::new();
        servers.insert("zeta".to_string(), json!({"command": "z"}));
        servers.insert("alpha".to_string(), json!({"url": "https://a"}));
        let keys = cursor_mcp_approval_keys(Path::new("/work"), &servers).expect("keys");
        assert_eq!(keys.len(), 2);
        assert!(keys[0].starts_with("alpha-"), "{keys:?}");
        assert!(keys[1].starts_with("zeta-"), "{keys:?}");
        for key in &keys {
            let (_, hash) = key.split_once('-').expect("separator");
            assert_eq!(hash.len(), 16);
            assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn approval_keys_are_stable_and_path_sensitive() {
        let mut servers = Map::new();
        servers.insert("a".to_string(), json!({"command": "run", "args": []}));
        let one = cursor_mcp_approval_keys(Path::new("/work"), &servers).expect("keys");
        let again = cursor_mcp_approval_keys(Path::new("/work"), &servers).expect("keys");
        assert_eq!(one, again);
        let other = cursor_mcp_approval_keys(Path::new("/elsewhere"), &servers).expect("keys");
        assert_ne!(one, other, "项目根参与哈希");
    }

    #[test]
    fn slugify_collapses_separator_runs() {
        assert_eq!(cursor_slugify_path("/home/dev/project"), "home-dev-project");
        assert_eq!(cursor_slugify_path("/a//b__c"), "a-b-c");
        assert_eq!(cursor_slugify_path("---"), "");
        assert_eq!(cursor_slugify_path("C:\\Users\\x"), "C-Users-x");
    }

    #[test]
    fn project_root_walks_up_to_the_git_directory() {
        let dir = TestDir::new("root");
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        fs::create_dir_all(repo.join("nested").join("deeper")).expect("mkdir nested");
        assert_eq!(
            cursor_project_root(&repo.join("nested").join("deeper")),
            fs::canonicalize(&repo).expect("canonical")
        );
        // 没有 `.git` ⇒ 回落成解析后的目录本身。
        let loose = dir.path().join("loose");
        fs::create_dir_all(&loose).expect("mkdir loose");
        assert_eq!(
            cursor_project_root(&loose),
            fs::canonicalize(&loose).expect("canonical")
        );
    }

    #[test]
    fn auth_source_must_be_an_absolute_mcp_auth_json_file() {
        let dir = TestDir::new("auth-source");
        let file = dir.path().join(CURSOR_MCP_AUTH_FILE);
        fs::write(&file, "{}").expect("write");
        assert_eq!(
            resolve_cursor_mcp_auth_source(file.to_str().expect("utf8")).expect("resolve"),
            file
        );
        // 传目录 ⇒ 自动接 `mcp-auth.json`。
        assert_eq!(
            resolve_cursor_mcp_auth_source(dir.path().to_str().expect("utf8")).expect("resolve"),
            file
        );

        for bad in ["", "  ", "relative/mcp-auth.json"] {
            assert!(resolve_cursor_mcp_auth_source(bad).is_err(), "{bad:?}");
        }
        // 目录里没有 `mcp-auth.json`。
        let empty = TestDir::new("auth-empty");
        assert!(resolve_cursor_mcp_auth_source(empty.path().to_str().expect("utf8")).is_err());
        // 文件名不对。
        let wrong = dir.path().join("other.json");
        fs::write(&wrong, "{}").expect("write");
        assert!(resolve_cursor_mcp_auth_source(wrong.to_str().expect("utf8")).is_err());
    }

    #[test]
    fn prepare_writes_the_sidecars_and_refuses_to_clobber_the_config() {
        let env_root = TestDir::new("prepare-env");
        let work = TestDir::new("prepare-work");
        fs::create_dir_all(work.path().join(".git")).expect("mkdir .git");
        let project_root = fs::canonicalize(work.path()).expect("canonical");
        let config = json!({"mcpServers": {"a": {"command": "run"}}});

        let data_dir = prepare_cursor_mcp_config(env_root.path(), work.path(), Some(&config), "")
            .expect("prepare")
            .expect("some");
        assert_eq!(data_dir, env_root.path().join("cursor-data"));

        let cursor_config: Value = serde_json::from_str(
            &fs::read_to_string(project_root.join(".cursor").join("mcp.json")).expect("read"),
        )
        .expect("parse");
        assert_eq!(cursor_config["mcpServers"]["a"]["command"], "run");

        let project_data_dir = data_dir
            .join("projects")
            .join(cursor_slugify_path(&project_root.to_string_lossy()));
        let approvals: Vec<String> = serde_json::from_str(
            &fs::read_to_string(project_data_dir.join("mcp-approvals.json")).expect("read"),
        )
        .expect("parse");
        assert_eq!(approvals.len(), 1);
        assert!(project_data_dir
            .join(CURSOR_WORKSPACE_TRUSTED_FILE)
            .is_file());

        // 第二遍：`.cursor/mcp.json` 已存在 ⇒ 拒绝覆盖（用户字节优先）。
        let err = prepare_cursor_mcp_config(env_root.path(), work.path(), Some(&config), "")
            .expect_err("must refuse");
        assert!(
            err.to_string().contains("would overwrite existing"),
            "{err}"
        );
    }

    #[test]
    fn a_null_config_writes_nothing() {
        let env_root = TestDir::new("null-env");
        let work = TestDir::new("null-work");
        assert!(
            prepare_cursor_mcp_config(env_root.path(), work.path(), None, "")
                .expect("no-op")
                .is_none()
        );
        assert!(
            prepare_cursor_mcp_config(env_root.path(), work.path(), Some(&Value::Null), "")
                .expect("no-op")
                .is_none()
        );
        assert!(!env_root.path().join("cursor-data").exists());
    }

    #[cfg(unix)]
    #[test]
    fn auth_file_is_seeded_as_a_link_to_the_users_copy() {
        let env_root = TestDir::new("auth-env");
        let work = TestDir::new("auth-work");
        let source_dir = TestDir::new("auth-src");
        let source = source_dir.path().join(CURSOR_MCP_AUTH_FILE);
        fs::write(&source, "{\"token\":\"t\"}").expect("write source");

        let data_dir = prepare_cursor_mcp_config(
            env_root.path(),
            work.path(),
            Some(&json!({"mcpServers": {"a": {"command": "x"}}})),
            source.to_str().expect("utf8"),
        )
        .expect("prepare")
        .expect("some");

        let project_root = fs::canonicalize(work.path()).expect("canonical");
        let project_data_dir = data_dir
            .join("projects")
            .join(cursor_slugify_path(&project_root.to_string_lossy()));
        let target = project_data_dir.join(CURSOR_MCP_AUTH_FILE);
        assert_eq!(
            fs::canonicalize(&target).expect("canonical"),
            fs::canonicalize(&source).expect("canonical")
        );
        // 播种是链接 ⇒ 不复制用户的 token 明文。
        assert!(fs::symlink_metadata(&target)
            .expect("metadata")
            .file_type()
            .is_symlink());
    }
}
