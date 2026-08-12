#![forbid(unsafe_code)]
//! hot-restart 的本地文件持久化层。

use crate::pure::{
    is_observed_hot_restart_target_alive, parse_hot_restart_intent, ProcessObservation,
    ProcessIdentityError, ReplacementIdentity,
};
use crate::types::{HotRestartIntent, HotRestartReport, HotRestartIntentRun, ShutdownSignal};
use chrono::{DateTime, SecondsFormat, Utc};
use pc_config::PaperclipHomePaths;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const INTENT_FILENAME: &str = "hot-restart-intent.json";
const REPORT_FILENAME: &str = "hot-restart-report.json";
const LOCK_SUFFIX: &str = ".lock";
const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// 本地 hot-restart 文件的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotRestartPaths {
    home_dir: PathBuf,
    instance_id: String,
}

impl HotRestartPaths {
    /// 使用显式 home 和 instance id 构造路径。
    pub fn new(home_dir: impl Into<PathBuf>, instance_id: impl Into<String>) -> Result<Self, HotRestartError> {
        let instance_id = instance_id.into();
        if instance_id.is_empty() || !instance_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')) {
            return Err(HotRestartError::InvalidInstanceId(instance_id));
        }
        Ok(Self { home_dir: home_dir.into(), instance_id })
    }

    /// 按 PAPERCLIP_HOME 和 PAPERCLIP_INSTANCE_ID 构造生产路径。
    pub fn from_env() -> Result<Self, HotRestartError> {
        let paths = PaperclipHomePaths::from_env()?;
        Self::new(paths.home_dir().to_path_buf(), paths.instance_id().to_owned())
    }

    /// Paperclip home 根目录。
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// 实例 id。
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// 实例目录。
    pub fn instance_root(&self) -> PathBuf {
        self.home_dir.join("instances").join(&self.instance_id)
    }

    /// 当前实例 marker 路径。
    pub fn intent_path(&self) -> PathBuf {
        self.instance_root().join(INTENT_FILENAME)
    }

    /// 旧版共享 marker 路径。
    pub fn legacy_intent_path(&self) -> PathBuf {
        self.home_dir.join(INTENT_FILENAME)
    }

    /// 当前实例接管报告路径。
    pub fn report_path(&self) -> PathBuf {
        self.instance_root().join(REPORT_FILENAME)
    }
}

/// 写入 intent 的参数。
#[derive(Debug, Clone)]
pub struct HotRestartIntentInput {
    /// 旧 server PID。
    pub previous_server_pid: i32,
    /// 不可变的 server boot identity。
    pub previous_server_identity: Option<String>,
    /// 操作系统进程启动时间。
    pub previous_server_started_at: Option<String>,
    /// 旧版本。
    pub previous_server_version: Option<String>,
    /// 是否要求 drain。
    pub drain_required: bool,
    /// 请求来源 run。
    pub requested_by_run_id: Option<String>,
    /// 预检 active runs。
    pub preflight_active_run_ids: Vec<String>,
    /// 请求时间，None 使用当前时间。
    pub requested_at: Option<String>,
}

impl HotRestartIntentInput {
    /// 构造一个默认 intent 输入。
    pub fn new(previous_server_pid: i32) -> Self {
        Self {
            previous_server_pid,
            previous_server_identity: None,
            previous_server_started_at: None,
            previous_server_version: None,
            drain_required: false,
            requested_by_run_id: None,
            preflight_active_run_ids: Vec::new(),
            requested_at: None,
        }
    }
}

/// hot-restart 本地存储错误。
#[derive(Debug, thiserror::Error)]
pub enum HotRestartError {
    /// 文件系统错误。
    #[error("hot-restart filesystem error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON 错误。
    #[error("hot-restart JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Paperclip home 路径错误。
    #[error("hot-restart home path error: {0}")]
    HomePath(#[from] pc_config::HomePathError),
    /// instance id 不安全。
    #[error("invalid hot-restart instance id '{0}'")]
    InvalidInstanceId(String),
    /// 无法确认进程身份。
    #[error("{0}")]
    ProcessIdentity(#[from] ProcessIdentityError),
    /// 兼容锁超时。
    #[error("timed out waiting for hot-restart compatibility lock at {0}")]
    LockTimeout(PathBuf),
}

/// 写入新的 hot-restart intent，同时 claim 共享旧路径。
pub async fn write_hot_restart_intent(
    paths: &HotRestartPaths,
    input: HotRestartIntentInput,
) -> Result<HotRestartIntent, HotRestartError> {
    let previous_server_started_at = match input.previous_server_started_at {
        Some(value) => normalize_date(&value),
        None => read_process_started_at(input.previous_server_pid).await.ok(),
    };
    if input.previous_server_identity.is_none() && previous_server_started_at.is_none() {
        return Err(HotRestartError::ProcessIdentity(ProcessIdentityError::Unknown {
            pid: input.previous_server_pid,
        }));
    }
    let intent = HotRestartIntent {
        version: 1,
        requested_at: input.requested_at.unwrap_or_else(now_iso),
        previous_server_pid: input.previous_server_pid,
        previous_server_identity: non_empty(input.previous_server_identity),
        previous_server_started_at,
        previous_server_version: input.previous_server_version,
        drain_required: input.drain_required,
        requested_by_run_id: non_empty(input.requested_by_run_id),
        preflight_active_run_ids: dedupe_strings(input.preflight_active_run_ids),
        shutdown_snapshot: None,
    };
    let legacy_path = paths.legacy_intent_path();
    with_path_lock(&legacy_path, || async {
        claim_legacy_intent(&legacy_path, &intent).await
    })
    .await?;
    let instance_path = paths.intent_path();
    if let Err(error) = with_path_lock(&instance_path, || async {
        write_json_atomic(&instance_path, &intent).await
    })
    .await
    {
        let _ = with_path_lock(&legacy_path, || async {
            remove_matching(&legacy_path, Some(&intent)).await
        })
        .await;
        return Err(error);
    }
    Ok(intent)
}

/// 在旧 server graceful shutdown 前写入 active run snapshot。
pub async fn write_hot_restart_shutdown_snapshot(
    paths: &HotRestartPaths,
    intent: &HotRestartIntent,
    signal: ShutdownSignal,
    active_runs: Vec<HotRestartIntentRun>,
    captured_at: Option<String>,
) -> Result<HotRestartIntent, HotRestartError> {
    let mut updated = intent.clone();
    updated.shutdown_snapshot = Some(crate::types::ShutdownSnapshot {
        captured_at: captured_at.unwrap_or_else(now_iso),
        signal,
        active_runs,
    });
    let instance_path = paths.intent_path();
    with_path_lock(&instance_path, || async {
        write_json_atomic(&instance_path, &updated).await
    })
    .await?;
    let legacy_path = paths.legacy_intent_path();
    with_path_lock(&legacy_path, || async {
        if let Some(legacy) = read_intent_at(&legacy_path).await? {
            if same_request(&legacy, intent) {
                write_json_atomic(&legacy_path, &updated).await?;
            }
        }
        Ok(())
    })
    .await?;
    Ok(updated)
}

/// 原子写入接管报告。
pub async fn write_hot_restart_report(
    paths: &HotRestartPaths,
    report: &HotRestartReport,
) -> Result<(), HotRestartError> {
    write_json_atomic(&paths.report_path(), report).await
}

/// 读取实例 marker，并在匹配时从 legacy marker 导入 snapshot。
pub async fn read_hot_restart_intent(
    paths: &HotRestartPaths,
) -> Result<Option<HotRestartIntent>, HotRestartError> {
    let instance = read_intent_at(&paths.intent_path()).await?;
    let legacy = match read_intent_at(&paths.legacy_intent_path()).await {
        Ok(value) => value,
        Err(_error) if instance.is_some() => return Ok(instance),
        Err(error) => return Err(error),
    };
    if instance.is_none() {
        return Ok(if paths.instance_id() == "default" { legacy } else { None });
    }
    let instance = instance.expect("checked above");
    let Some(legacy) = legacy else {
        return Ok(Some(instance));
    };
    if !same_request(&instance, &legacy) {
        return Ok(Some(instance));
    }
    if let Some(snapshot) = legacy.shutdown_snapshot {
        let mut merged = instance;
        merged.shutdown_snapshot = Some(snapshot);
        return Ok(Some(merged));
    }
    Ok(Some(instance))
}

/// 删除 intent；expected 存在时只删除同一请求，避免覆盖并发 replacement。
pub async fn remove_hot_restart_intent(
    paths: &HotRestartPaths,
    expected: Option<&HotRestartIntent>,
) -> Result<(), HotRestartError> {
    let instance_path = paths.intent_path();
    with_path_lock(&instance_path, || async {
        remove_matching(&instance_path, expected).await
    })
    .await?;
    let legacy_path = paths.legacy_intent_path();
    with_path_lock(&legacy_path, || async {
        remove_matching(&legacy_path, expected).await
    })
    .await
}

/// 读取目标进程的启动时间，用于 PID 复用保护。
pub async fn read_process_started_at(pid: i32) -> Result<String, HotRestartError> {
    if pid <= 0 {
        return Err(HotRestartError::ProcessIdentity(ProcessIdentityError::Unknown { pid }));
    }
    #[cfg(target_os = "linux")]
    {
        let metadata = fs::metadata(format!("/proc/{pid}")).await?;
        let started = metadata.modified().or_else(|_| metadata.created())?;
        return Ok(chrono::DateTime::<Utc>::from(started)
            .to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "aix", target_os = "solaris"))]
    {
        let output = Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .await?;
        if !output.status.success() {
            return Err(HotRestartError::ProcessIdentity(ProcessIdentityError::Unknown { pid }));
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let parsed = chrono::NaiveDateTime::parse_from_str(&raw, "%a %b %e %H:%M:%S %Y")
            .map_err(|_| HotRestartError::ProcessIdentity(ProcessIdentityError::Unknown { pid }))?;
        return Ok(chrono::DateTime::<Utc>::from_naive_utc_and_offset(parsed, Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!("$process = Get-Process -Id {pid} -ErrorAction Stop; $process.StartTime.ToUniversalTime().ToString('o')");
        let mut output = Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .await?;
        if !output.status.success() {
            output = Command::new("pwsh.exe")
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
                .output()
                .await?;
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return normalize_date(&raw).ok_or_else(|| {
            HotRestartError::ProcessIdentity(ProcessIdentityError::Unknown { pid })
        });
    }
    #[allow(unreachable_code)]
    Err(HotRestartError::ProcessIdentity(ProcessIdentityError::Unknown { pid }))
}

async fn claim_legacy_intent(
    path: &Path,
    intent: &HotRestartIntent,
) -> Result<(), HotRestartError> {
    match write_json_exclusive(path, intent).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let Some(existing) = read_intent_at(path).await? else {
                return Err(error.into());
            };
            let alive = process_alive(existing.previous_server_pid).await?;
            let started_at = if alive {
                Some(read_process_started_at(existing.previous_server_pid).await?)
            } else {
                None
            };
            let replacement = ReplacementIdentity {
                previous_server_pid: intent.previous_server_pid,
                previous_server_identity: intent.previous_server_identity.clone(),
                previous_server_started_at: intent.previous_server_started_at.clone(),
            };
            if is_observed_hot_restart_target_alive(
                &existing,
                &ProcessObservation { alive, started_at, replacement: Some(replacement) },
            )? {
                return Err(error.into());
            }
            remove_matching(path, Some(&existing)).await?;
            write_json_exclusive(path, intent).await.map_err(Into::into)
        }
        Err(error) => Err(error.into()),
    }
}

async fn process_alive(pid: i32) -> Result<bool, HotRestartError> {
    if pid <= 0 {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .await?;
        return Ok(status.success());
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .await?;
        return Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()));
    }
    #[allow(unreachable_code)]
    Ok(false)
}

async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), HotRestartError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let temp = unique_temp_path(path);
    let bytes = format!("{}\n", serde_json::to_string_pretty(value)?);
    let mut file = fs::File::create(&temp).await?;
    file.write_all(bytes.as_bytes()).await?;
    file.sync_all().await?;
    drop(file);
    fs::rename(&temp, path).await?;
    Ok(())
}

async fn write_json_exclusive<T: Serialize>(path: &Path, value: &T) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let temp = unique_temp_path(path);
    let bytes = format!("{}\n", serde_json::to_string_pretty(value).map_err(std::io::Error::other)?);
    let mut file = fs::File::create(&temp).await?;
    file.write_all(bytes.as_bytes()).await?;
    file.sync_all().await?;
    drop(file);
    match fs::hard_link(&temp, path).await {
        Ok(()) => {
            let _ = fs::remove_file(&temp).await;
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temp).await;
            Err(error)
        }
    }
}

async fn read_intent_at(path: &Path) -> Result<Option<HotRestartIntent>, HotRestartError> {
    match fs::read_to_string(path).await {
        Ok(raw) => Ok(parse_hot_restart_intent(&serde_json::from_str(&raw)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn remove_matching(
    path: &Path,
    expected: Option<&HotRestartIntent>,
) -> Result<(), HotRestartError> {
    if let Some(expected) = expected {
        let Some(current) = read_intent_at(path).await? else {
            return Ok(());
        };
        if !same_request(&current, expected) {
            return Ok(());
        }
    }
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn with_path_lock<T, F, Fut>(path: &Path, operation: F) -> Result<T, HotRestartError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, HotRestartError>>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let lock = PathBuf::from(format!("{}{}", path.display(), LOCK_SUFFIX));
    let deadline = tokio::time::Instant::now() + LOCK_TIMEOUT;
    loop {
        match fs::create_dir(&lock).await {
            Ok(()) => {
                let owner = lock.join("owner.json");
                let _ = fs::write(owner, format!("{{\"pid\":{},\"createdAt\":\"{}\"}}\n", std::process::id(), now_iso())).await;
                let result = operation().await;
                let _ = fs::remove_dir_all(&lock).await;
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&lock)
                    .await
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > LOCK_STALE);
                if stale {
                    let _ = fs::remove_dir_all(&lock).await;
                    continue;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(HotRestartError::LockTimeout(lock));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn same_request(left: &HotRestartIntent, right: &HotRestartIntent) -> bool {
    left.requested_at == right.requested_at
        && left.previous_server_pid == right.previous_server_pid
        && left.drain_required == right.drain_required
        && left.requested_by_run_id == right.requested_by_run_id
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !value.trim().is_empty() && !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn normalize_date(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn now_iso() -> String {
    DateTime::<Utc>::from(SystemTime::now())
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    PathBuf::from(format!("{}.{}.{}.tmp", path.display(), std::process::id(), nanos))
}
