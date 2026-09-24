//! omp（oh-my-pi）的托管 MCP 配置注入口。
//!
//! - **上游**：`execenv/omp_mcp.go`（33 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! 只有 `provider == "omp"` 且 agent 存了**显式**的托管 `mcp_config` 时才动手：把它写到
//! `<workdir>/.omp/mcp.json`。`null` / 缺省 ⇒ 什么都不做（让 omp 走自己的发现链）。
//!
//! 覆盖已有文件是**拒绝**的（上游 `errPathPreExists` 那条路）：用户自己的 `.omp/mcp.json`
//! 必须逐字节留着，写坏一次就找不回来了。

use std::path::Path;

use serde_json::Value;

use crate::execenv::sidecar::{create_dir_all, write_new_file, SidecarError};

/// omp 的托管配置目录名（上游 `"." + provider`）。
pub const OMP_CONFIG_DIR: &str = ".omp";
/// omp 的 MCP 配置文件名（上游 `mcp.json`）。
pub const OMP_MCP_FILE: &str = "mcp.json";

/// 这个 `mcp_config` 是不是「显式的托管配置」。
///
/// ⚠️ 上游 `prepareOmpMcpConfig` 只判「非空、非 `null`」——**空对象也算托管**（它写出一份
/// 空的 `mcp.json` 就表达「这个 agent 管着 MCP」）。
#[must_use]
pub fn has_managed_omp_mcp_config(mcp_config: Option<&Value>) -> bool {
    match mcp_config {
        None | Some(Value::Null) => false,
        Some(_) => true,
    }
}

/// 写 omp 的 `.omp/mcp.json`（上游 `prepareOmpMcpConfig`）。
///
/// 返回写出的路径；不该写时返回 `Ok(None)`。
pub fn prepare_omp_mcp_config(
    work_dir: &Path,
    provider: &str,
    mcp_config: Option<&Value>,
) -> Result<Option<std::path::PathBuf>, SidecarError> {
    if provider != "omp" {
        return Ok(None);
    }
    if !has_managed_omp_mcp_config(mcp_config) {
        return Ok(None);
    }
    if work_dir.as_os_str().is_empty() {
        return Err(SidecarError::Invalid(
            "managed mcp_config requires a working directory".to_string(),
        ));
    }

    // ⚠️ 先解析工作目录：`workDir` 是相对路径时，`.omp/mcp.json` 会落到 daemon 的 cwd 上，
    // 而不是用户的任务目录（上游此处直接 `filepath.Join`，本 slice 收紧为「必须是绝对路径
    // 或可解析」，登记在 `docs/32` §9.9）。
    if !work_dir.is_absolute() {
        return Err(SidecarError::Invalid(format!(
            "managed mcp_config requires an absolute working directory, got {}",
            work_dir.display()
        )));
    }

    let config_dir = work_dir.join(OMP_CONFIG_DIR);
    create_dir_all(&config_dir)?;
    let path = config_dir.join(OMP_MCP_FILE);

    let payload = mcp_config.unwrap_or(&Value::Null);
    let mut data = serde_json::to_vec_pretty(payload)
        .map_err(|err| SidecarError::Invalid(format!("marshal omp mcp config: {err}")))?;
    data.push(b'\n');

    match write_new_file(&path, &data) {
        Ok(()) => Ok(Some(path)),
        Err(err) if err.is_pre_existing() => Err(SidecarError::Invalid(format!(
            "managed mcp_config would overwrite existing {}",
            path.display()
        ))),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mc-daemon-omp-mcp-{name}-{nanos:x}"));
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
    fn only_omp_with_an_explicit_config_writes_anything() {
        let dir = TestDir::new("guard");
        assert!(
            prepare_omp_mcp_config(dir.path(), "claude", Some(&json!({"mcpServers": {}})))
                .expect("no-op")
                .is_none()
        );
        assert!(prepare_omp_mcp_config(dir.path(), "omp", None)
            .expect("no-op")
            .is_none());
        assert!(
            prepare_omp_mcp_config(dir.path(), "omp", Some(&Value::Null))
                .expect("no-op")
                .is_none()
        );
        assert!(!dir.path().join(OMP_CONFIG_DIR).exists());
    }

    #[test]
    fn has_managed_config_matches_the_documented_shapes() {
        assert!(!has_managed_omp_mcp_config(None));
        assert!(!has_managed_omp_mcp_config(Some(&Value::Null)));
        assert!(has_managed_omp_mcp_config(Some(&json!({}))));
        assert!(has_managed_omp_mcp_config(Some(
            &json!({"mcpServers": {"a": {}}})
        )));
    }

    #[test]
    fn writes_the_config_with_owner_only_conventions() {
        let dir = TestDir::new("write");
        let config = json!({"mcpServers": {"a": {"type": "http", "url": "https://x"}}});
        let path = prepare_omp_mcp_config(dir.path(), "omp", Some(&config))
            .expect("write")
            .expect("some");
        assert_eq!(path, dir.path().join(".omp").join("mcp.json"));
        let body: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse");
        assert_eq!(body["mcpServers"]["a"]["url"], "https://x");
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.ends_with('\n'), "pretty output ends with a newline");
    }

    #[test]
    fn an_existing_user_config_is_never_overwritten() {
        let dir = TestDir::new("refuse");
        let config_dir = dir.path().join(".omp");
        fs::create_dir_all(&config_dir).expect("mkdir");
        let path = config_dir.join("mcp.json");
        fs::write(&path, "user bytes").expect("seed");

        let err = prepare_omp_mcp_config(dir.path(), "omp", Some(&json!({"mcpServers": {}})))
            .expect_err("must refuse");
        assert!(
            err.to_string().contains("would overwrite existing"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(&path).expect("read"), "user bytes");
    }

    #[test]
    fn a_relative_work_dir_is_rejected() {
        let err = prepare_omp_mcp_config(Path::new("relative/dir"), "omp", Some(&json!({})))
            .expect_err("must reject");
        assert!(
            err.to_string().contains("absolute working directory"),
            "{err}"
        );
    }
}
