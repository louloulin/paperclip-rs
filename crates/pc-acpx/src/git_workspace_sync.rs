//! `pc-acpx::git_workspace_sync` - port of `git-workspace-sync.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for git workspace synchronization between a local
//! workspace and a remote/sandbox runtime. The async functions that
//! actually invoke the `git` CLI are deferred to a follow-up round;
//! this module ports the constant, the pure ref-name generators, the
//! script builder, and the prerequisite-error classifier.

/// Default excludes for git archive operations. Mirrors Node
/// `GIT_ARCHIVE_EXCLUDES`.
pub const GIT_ARCHIVE_EXCLUDES: &[&str] = &[".git", ".git/*"];

/// Substrings that `git` emits when a bundle import failed because the
/// importer lacks a prerequisite commit. Mirrors Node
/// `GIT_MISSING_PREREQUISITE_MARKERS`.
pub const GIT_MISSING_PREREQUISITE_MARKERS: &[&str] = &[
    "did not send all necessary objects",
    "lacks these prerequisite commits",
    "revision walk setup failed",
];

/// Create a git ref name under which an imported (delta) bundle is
/// stored. Mirrors Node `createImportedGitRef`.
#[must_use]
pub fn create_imported_git_ref(scope: &str) -> String {
    format!("refs/paperclip/git-sync/imported/{scope}/{}", uuid::Uuid::new_v4())
}

/// Create a git ref name used by the remote to export its current
/// `HEAD`. Mirrors Node `createRemoteGitExportRef`.
#[must_use]
pub fn create_remote_git_export_ref(scope: &str) -> String {
    format!("refs/paperclip/git-sync/export/{scope}/{}", uuid::Uuid::new_v4())
}

/// True when a bundle import failed because the importer lacks a
/// prerequisite commit. Mirrors Node `isMissingGitPrerequisiteError`.
/// Such a failure is recoverable by re-exporting a full, self-contained
/// bundle from the still-live sandbox rather than discarding the run.
#[must_use]
pub fn is_missing_git_prerequisite_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let message = error.to_string();
    GIT_MISSING_PREREQUISITE_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

/// True for any error-like value that exposes a `.message` (Error) or
/// coerces to a string. Convenience wrapper that accepts
/// `Box<dyn Error>` and `String` uniformly.
#[must_use]
pub fn is_missing_git_prerequisite_error_anyhow(error_message: &str) -> bool {
    GIT_MISSING_PREREQUISITE_MARKERS
        .iter()
        .any(|marker| error_message.contains(marker))
}

/// Options for [`build_remote_git_delta_bundle_script`]. Mirrors the
/// input shape of Node `buildRemoteGitDeltaBundleScript`.
#[derive(Debug, Clone)]
pub struct RemoteGitDeltaBundleOptions {
    pub remote_dir: String,
    pub base_sha: String,
    pub export_ref: String,
    pub bundle_path: String,
    pub status_path: Option<String>,
    pub cat_bundle: bool,
    pub cleanup_bundle: bool,
    /// Skip the delta boundary entirely and always emit a full,
    /// self-contained bundle (no prerequisites).
    pub force_full_bundle: bool,
}

/// Build a shell script that creates a git bundle on the remote,
/// optionally relative to `base_sha`. Mirrors Node
/// `buildRemoteGitDeltaBundleScript`.
#[must_use]
pub fn build_remote_git_delta_bundle_script(input: &RemoteGitDeltaBundleOptions) -> String {
    let remote_dir = shell_quote(&input.remote_dir);
    let bundle_path = shell_quote(&input.bundle_path);
    let export_ref = shell_quote(&input.export_ref);
    let base_sha = shell_quote(&input.base_sha);
    let status_path = input.status_path.as_deref().map(shell_quote);

    let cleanup_parts = [
        format!("rm -f {bundle_path}"),
        status_path
            .as_ref()
            .map(|s| format!("rm -f {s}"))
            .unwrap_or_default(),
        format!(
            "git -C {remote_dir} update-ref -d {export_ref} >/dev/null 2>&1 || true"
        ),
    ];

    let mut lines: Vec<String> = Vec::new();
    lines.push("set -e".to_string());
    if input.cleanup_bundle {
        lines.push(format!(
            "cleanup() {{ {}; }}",
            cleanup_parts.join("; ")
        ));
        lines.push("trap cleanup EXIT".to_string());
    }
    let parent_dir = std::path::Path::new(&input.bundle_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    lines.push(format!("mkdir -p {}", shell_quote(&parent_dir)));
    lines.push(format!("rm -f {bundle_path}"));

    if input.force_full_bundle {
        lines.push("bundle_base=\"\"".to_string());
    } else {
        lines.push(format!(
            "if git -C {remote_dir} cat-file -e {base_sha}^{{commit}} 2>/dev/null; then"
        ));
        lines.push(format!(
            "  bundle_base=$(git -C {remote_dir} merge-base {base_sha} HEAD 2>/dev/null || true)"
        ));
        lines.push("else".to_string());
        lines.push("  bundle_base=\"\"".to_string());
        lines.push("fi".to_string());
    }

    lines.push("if [ -n \"$bundle_base\" ]; then".to_string());
    lines.push(format!(
        "  commit_count=$(git -C {remote_dir} rev-list --count HEAD --not \"$bundle_base\")"
    ));
    lines.push("else".to_string());
    lines.push(format!(
        "  commit_count=$(git -C {remote_dir} rev-list --count HEAD)"
    ));
    lines.push("fi".to_string());
    lines.push("if [ \"$commit_count\" -gt 0 ]; then".to_string());
    lines.push(format!(
        "  git -C {remote_dir} update-ref {export_ref} HEAD"
    ));
    lines.push("  if [ -n \"$bundle_base\" ]; then".to_string());
    lines.push(format!(
        "    git -C {remote_dir} bundle create {bundle_path} {export_ref} --not \"$bundle_base\" >/dev/null"
    ));
    lines.push("  else".to_string());
    lines.push(format!(
        "    git -C {remote_dir} bundle create {bundle_path} {export_ref} >/dev/null"
    ));
    lines.push("  fi".to_string());
    lines.push("else".to_string());
    lines.push(format!("  : > {bundle_path}"));
    lines.push("fi".to_string());

    if let Some(status) = &status_path {
        lines.push(format!(
            "if [ -z \"$(git -C {remote_dir} status --porcelain=v1 --untracked-files=normal)\" ]; then"
        ));
        lines.push(format!("  printf clean > {status}"));
        lines.push("else".to_string());
        lines.push(format!("  printf dirty > {status}"));
        lines.push("fi".to_string());
    }

    if input.cat_bundle {
        lines.push(format!("cat {bundle_path}"));
    }

    lines.join("\n")
}

/// POSIX shell single-quote a string. Mirrors the Node helper.
fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

// =============================================================================
// Async git CLI helpers (port of Node `runLocalGit` + snapshot reader +
// ref deletion). These wrap `git -C <localDir> <args>` so the rest of the
// workspace-sync layer can stay synchronous / pure.
// =============================================================================

use std::path::Path;
use std::process::Stdio;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use crate::runtime_progress::{create_transfer_progress, TransferProgressOptions, RuntimeProgressPhase, RuntimeProgressDirection, RuntimeProgressSink};
use crate::ssh::{SshAuthArgs, SshRemoteExecutionSpec};

/// Result of a local `git` invocation (mirrors Node `GitCommandResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommandResult {
    pub stdout: String,
    pub stderr: String,
}

/// Errors from `run_local_git` (mirrors Node `execFile` rejected promise).
#[derive(Debug, Error)]
pub enum RunLocalGitError {
    #[error("git command timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
    #[error("git command exited with status {status:?}: {stderr}")]
    NonZeroExit {
        status: Option<i32>,
        stderr: String,
    },
    #[error("git command output exceeded maxBuffer of {max_buffer_bytes} bytes")]
    OutputOverflow { max_buffer_bytes: usize },
    #[error("failed to spawn git: {0}")]
    Spawn(#[from] std::io::Error),
}

/// Snapshot of a local git workspace (mirrors Node `GitWorkspaceSnapshot`).
///
/// `head_commit` is the SHA-1 of the current `HEAD`; `branch_name` is the
/// short branch name when on a branch (None when detached). `overlay_paths`
/// covers modifications, additions, and untracked files (in working tree
/// vs HEAD); `deleted_paths` covers deletions; `ignored_paths` covers
/// entries git would normally ignore under standard excludes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitWorkspaceSnapshot {
    pub head_commit: String,
    pub branch_name: Option<String>,
    pub overlay_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
    pub ignored_paths: Vec<String>,
}

/// Run a local `git` invocation under `local_dir` with the given args.
///
/// Mirrors Node `runLocalGit(localDir, args, options)`:
/// - `timeout_ms` default 15_000 ms (Node default)
/// - `max_buffer_bytes` default 128 KiB (Node default; per-call override
///   matters for large `ls-files --others` outputs)
/// - On error, the original stderr is preserved in the returned error so
///   callers can detect missing-prerequisite markers.
pub async fn run_local_git(
    local_dir: &str,
    args: &[&str],
    timeout_ms: Option<u64>,
    max_buffer_bytes: Option<usize>,
) -> Result<GitCommandResult, RunLocalGitError> {
    let timeout_ms = timeout_ms.unwrap_or(15_000);
    let max_buffer_bytes = max_buffer_bytes.unwrap_or(128 * 1024);

    let mut command = Command::new("git");
    command.arg("-C").arg(local_dir);
    for arg in args {
        command.arg(arg);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let read_stdout = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(max_buffer_bytes.min(64 * 1024));
        let mut tmp = [0u8; 4096];
        loop {
            match stdout.read(&mut tmp).await {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > max_buffer_bytes {
                        return Err(max_buffer_bytes);
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                Err(_) => break,
            }
        }
        Ok(buf)
    });
    let stderr_bytes = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut tmp).await {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        buf
    });

    let status_res = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait(),
    )
    .await;

    let status = match status_res {
        Err(_elapsed) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(RunLocalGitError::Timeout { timeout_ms });
        }
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(RunLocalGitError::Spawn(error)),
    };

    let stdout_bytes = match read_stdout.await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(limit)) => return Err(RunLocalGitError::OutputOverflow { max_buffer_bytes: limit }),
        Err(_) => Vec::new(),
    };
    let stderr_bytes = stderr_bytes.await.unwrap_or_default();

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    if !status.success() {
        return Err(RunLocalGitError::NonZeroExit {
            status: status.code(),
            stderr,
        });
    }

    Ok(GitCommandResult { stdout, stderr })
}

/// Read a snapshot of the local git workspace under `local_dir`.
///
/// Returns `Ok(None)` when `local_dir` is not a git working tree — either
/// `rev-parse --is-inside-work-tree` exits non-zero (no `.git` ancestor) or
/// returns anything other than `"true"` (e.g. bare repo). Mirrors the Node
/// "treat absence as no-snapshot" convention used by callers in
/// `sandbox-managed-runtime.ts`.
pub async fn read_git_workspace_snapshot(
    local_dir: &str,
) -> Result<Option<GitWorkspaceSnapshot>, RunLocalGitError> {
    let inside_work_tree = run_local_git(
        local_dir,
        &["rev-parse", "--is-inside-work-tree"],
        Some(10_000),
        Some(16 * 1024),
    )
    .await;
    let stdout = match inside_work_tree {
        Ok(result) => result.stdout,
        // Non-zero exit (e.g. `fatal: not a git repository`) → no snapshot.
        Err(RunLocalGitError::NonZeroExit { .. }) => return Ok(None),
        Err(other) => return Err(other),
    };
    if stdout.trim() != "true" {
        return Ok(None);
    }

    let head_commit_res = run_local_git(
        local_dir,
        &["rev-parse", "HEAD"],
        Some(10_000),
        Some(16 * 1024),
    )
    .await?;
    let branch_res = run_local_git(
        local_dir,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        Some(10_000),
        Some(16 * 1024),
    )
    .await?;
    let overlay_diff_res = run_local_git(
        local_dir,
        &[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=ACMRTUXB",
            "HEAD",
            "--",
        ],
        Some(10_000),
        Some(1024 * 1024),
    )
    .await?;
    let untracked_res = run_local_git(
        local_dir,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        Some(10_000),
        Some(1024 * 1024),
    )
    .await?;
    let deleted_res = run_local_git(
        local_dir,
        &["diff", "--name-only", "-z", "--diff-filter=D", "HEAD", "--"],
        Some(10_000),
        Some(256 * 1024),
    )
    .await?;
    let ignored_res = run_local_git(
        local_dir,
        &[
            "status",
            "--ignored",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
        ],
        Some(10_000),
        Some(1024 * 1024),
    )
    .await?;

    let branch_name = branch_res.stdout.trim().to_owned();
    let branch_name = if branch_name.is_empty() || branch_name == "HEAD" {
        None
    } else {
        Some(branch_name)
    };

    let split_nul = |value: &str| -> Vec<String> {
        value
            .split('\0')
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty())
            .collect()
    };

    let mut overlay_paths = split_nul(&overlay_diff_res.stdout);
    overlay_paths.extend(split_nul(&untracked_res.stdout));
    overlay_paths.sort();
    overlay_paths.dedup();

    let mut deleted_paths = split_nul(&deleted_res.stdout);
    deleted_paths.sort();
    deleted_paths.dedup();

    let ignored_paths = split_nul(&ignored_res.stdout);

    Ok(Some(GitWorkspaceSnapshot {
        head_commit: head_commit_res.stdout.trim().to_owned(),
        branch_name,
        overlay_paths,
        deleted_paths,
        ignored_paths,
    }))
}

/// Delete a local git ref (best-effort: errors are swallowed). Mirrors
/// Node `deleteLocalGitRef`.
pub async fn delete_local_git_ref(local_dir: &str, git_ref: &str) -> Result<(), RunLocalGitError> {
    let res = run_local_git(
        local_dir,
        &["update-ref", "-d", git_ref],
        Some(10_000),
        Some(16 * 1024),
    )
    .await;
    // Mirror Node `.catch(() => undefined)`: ignore failure.
    let _ = res;
    Ok(())
}

/// Variant of [`read_git_workspace_snapshot`] that takes a `Path` for
/// ergonomic callers (avoids leaking `&str` to fs callers).
pub async fn read_git_workspace_snapshot_path(
    local_dir: &Path,
) -> Result<Option<GitWorkspaceSnapshot>, RunLocalGitError> {
    let local_dir_str = local_dir.to_string_lossy();
    read_git_workspace_snapshot(&local_dir_str).await
}


// =============================================================================
// SSH streaming helpers (port of Node `streamLocalFileToSsh` +
// `streamSshToLocalFile`). These spawn an `ssh` child with stdin/stdout
// piped so that a local file can be transferred through an existing remote
// shell command (used by git-bundle transfer).
// =============================================================================

/// Errors from SSH file streaming (port of Node `streamLocalFileToSsh`
/// / `streamSshToLocalFile` rejection paths).
#[derive(Debug, Error)]
pub enum SshStreamError {
    #[error("failed to build ssh auth args: {0}")]
    Auth(String),
    #[error("failed to spawn ssh: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("ssh exited with status {status:?}: {stderr}")]
    NonZeroExit {
        status: Option<i32>,
        stderr: String,
    },
    #[error("local file not found: {0}")]
    LocalFileMissing(String),
    #[error("local file output write failed: {0}")]
    LocalFileWrite(String),
}

/// Stream a local file's bytes into a remote shell command's stdin (port
/// of Node `streamLocalFileToSsh`).
///
/// The remote script runs under `sh -c <script>` on the SSH host; the
/// file's bytes are piped to its stdin. Used by
/// [`import_git_workspace_to_ssh`] to push a local `git bundle` into the
/// remote workspace setup script.
pub async fn stream_local_file_to_ssh(
    spec: &SshRemoteExecutionSpec,
    local_file: &Path,
    remote_script: &str,
    progress: Option<&RuntimeProgressSink>,
) -> Result<(), SshStreamError> {
    use tokio::io::AsyncWriteExt;

    if !local_file.exists() {
        return Err(SshStreamError::LocalFileMissing(
            local_file.to_string_lossy().into_owned(),
        ));
    }

    let auth = SshAuthArgs::create(&spec.as_connection_config()).map_err(SshStreamError::Auth)?;

    let mut command = tokio::process::Command::new("ssh");
    command
        .args(auth.args())
        .arg("-p")
        .arg(spec.port.to_string())
        .arg(format!("{}@{}", spec.username, spec.host))
        .arg(format!("sh -c {}", shell_quote(remote_script)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "stdin pipe unavailable"))?;

    let mut file = tokio::fs::File::open(local_file).await?;
    let total_bytes = std::fs::metadata(local_file).map(|m| m.len()).unwrap_or(0);

    // Wrap the local file with a counting reader when a progress sink is
    // provided. The bundle is a real file of known size, so we use the
    // exact-percent mode (`estimated = false`); no cap, so the terminal
    // 100% line is emitted exactly when the last byte is written.
    let copy_result = if let Some(sink) = progress {
        let progress_inner = create_transfer_progress(
            Box::new(file),
            TransferProgressOptions {
                on_progress: sink.clone(),
                phase: RuntimeProgressPhase::ImportingGitHistory,
                direction: RuntimeProgressDirection::To,
                label: None,
                total_bytes: Some(total_bytes),
                estimated: false,
            },
        );
        let mut counter = progress_inner.counter;
        let finish = progress_inner.finish;
        let fail = progress_inner.fail;
        let copy_result = tokio::io::copy(&mut counter, &mut stdin).await;
        match &copy_result {
            Ok(_) => finish().await,
            Err(_) => fail().await,
        };
        drop(stdin); // close stdin to signal EOF
        copy_result
    } else {
        let copy_result = tokio::io::copy(&mut file, &mut stdin).await;
        drop(stdin); // close stdin to signal EOF
        copy_result
    };
    copy_result?;

    let mut stderr_buf = Vec::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut stderr_buf).await;
    }
    let status = child.wait().await?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf).into_owned();
        return Err(SshStreamError::NonZeroExit {
            status: status.code(),
            stderr,
        });
    }
    Ok(())
}

/// Stream a remote shell command's stdout into a local file (port of
/// Node `streamSshToLocalFile`).
///
/// The remote script's stdout bytes are piped to the local file. Used by
/// [`export_git_workspace_from_ssh`] to receive a remote `git bundle`.
pub async fn stream_ssh_to_local_file(
    spec: &SshRemoteExecutionSpec,
    remote_script: &str,
    local_file: &Path,
) -> Result<(), SshStreamError> {
    use tokio::io::AsyncWriteExt;

    let auth = SshAuthArgs::create(&spec.as_connection_config()).map_err(SshStreamError::Auth)?;

    let mut command = tokio::process::Command::new("ssh");
    command
        .args(auth.args())
        .arg("-p")
        .arg(spec.port.to_string())
        .arg(format!("{}@{}", spec.username, spec.host))
        .arg(format!("sh -c {}", shell_quote(remote_script)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "stdout pipe unavailable"))?;

    // Ensure parent dir exists, then create the destination file with 0o600
    // (Node `createWriteStream({ mode: 0o600 })`).
    if let Some(parent) = local_file.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(local_file)
        .await
        .map_err(|error| SshStreamError::LocalFileWrite(error.to_string()))?;

    if let Err(error) = tokio::io::copy(&mut stdout, &mut file).await {
        return Err(SshStreamError::LocalFileWrite(error.to_string()));
    }
    file.flush().await.map_err(|error| SshStreamError::LocalFileWrite(error.to_string()))?;
    drop(file);

    let mut stderr_buf = Vec::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut stderr_buf).await;
    }
    let status = child.wait().await?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf).into_owned();
        return Err(SshStreamError::NonZeroExit {
            status: status.code(),
            stderr,
        });
    }
    Ok(())
}

// =============================================================================
// Git workspace import / export via SSH bundle transfer (port of Node
// `importGitWorkspaceToSsh` / `exportGitWorkspaceFromSsh`).
// =============================================================================

/// Import a local git workspace into a remote SSH host by transferring
/// a `git bundle` containing the snapshot's HEAD commit. Mirrors Node
/// `importGitWorkspaceToSsh`.
///
/// Flow:
/// 1. Create a per-import ref so concurrent imports against the same
///    local repo don't race on `update-ref`.
/// 2. `git update-ref <tempRef> <headCommit>` locally.
/// 3. `git bundle create <bundlePath> <tempRef>` locally.
/// 4. Stream the bundle to the remote via `stream_local_file_to_ssh`,
///    where a `set -e` shell script:
///    - mkdir `<remote>/.paperclip-runtime`
///    - `tmp_bundle=$(mktemp ...)` and `cat > "$tmp_bundle"`
///    - `git init <remote>` if missing `.git`
///    - `git fetch --force "$tmp_bundle" <tempRef>:<tempRef>`
///    - `git checkout --force -B <branch>` or `--detach <headCommit>`
///    - `git reset --hard <headCommit>`
///    - `git clean -fdx -e .paperclip-runtime`
///    - drop the per-import ref on the remote side
/// 5. Always `git update-ref -d <tempRef>` locally + cleanup bundle.
pub async fn import_git_workspace_to_ssh(
    spec: &SshRemoteExecutionSpec,
    local_dir: &Path,
    remote_dir: &str,
    snapshot: &GitWorkspaceSnapshot,
    progress: Option<&RuntimeProgressSink>,
) -> Result<(), String> {
    let local_dir_str = local_dir.to_string_lossy();
    let bundle_dir = std::env::temp_dir().join(format!(
        "paperclip-ssh-bundle-import-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&bundle_dir)
        .map_err(|error| format!("create bundle dir: {error}"))?;
    let bundle_path = bundle_dir.join("workspace.bundle");
    let bundle_path_str = bundle_path.to_string_lossy().into_owned();
    let temp_ref = format!(
        "refs/paperclip/ssh-sync/import/{}",
        uuid::Uuid::new_v4()
    );

    // Build remote script. Use string concatenation (via Vec<String>) to
    // avoid Rust 2021's reserved `$identifier` syntax in string literals.
    let runtime_dir_quoted = shell_quote(&format!("{remote_dir}/.paperclip-runtime"));
    let remote_dir_quoted = shell_quote(remote_dir);
    let head_quoted = shell_quote(&snapshot.head_commit);
    let checkout_line = match &snapshot.branch_name {
        Some(branch) => format!(
            "git -C {} checkout --force -B {} {} >/dev/null",
            remote_dir_quoted,
            shell_quote(branch),
            head_quoted,
        ),
        None => format!(
            "git -C {} -c advice.detachedHead=false checkout --force --detach {} >/dev/null",
            remote_dir_quoted,
            head_quoted,
        ),
    };
    let mut script_lines: Vec<String> = vec![
        "set -e".to_owned(),
        format!("mkdir -p {}", runtime_dir = runtime_dir_quoted),
        format!(
            "tmp_bundle=$(mktemp {}/import-XXXXXX.bundle)",
            runtime_dir = runtime_dir_quoted,
        ),
        // Build the trap line by concatenating at runtime; this keeps the
        // shell variable literal (`$` + `tmp_bundle`) out of any single
        // Rust string/format literal where `$identifier` would be reserved.
        String::from("trap 'rm -f \"")
            + "$" + "tmp_bundle"
            + &String::from("\"' EXIT"),
        // cat > "$tmp_bundle"
        String::from("cat > \"")
            + "$" + "tmp_bundle"
            + &String::from("\""),
        format!(
            "if [ ! -d {0}/.git ]; then git init {0} >/dev/null; fi",
            remote_dir_quoted,
        ),
        // Same for the fetch line — assemble at runtime.
        {
            let mut s = String::from("git -C ");
            s.push_str(&remote_dir_quoted);
            s.push_str(" fetch --force \"");
            s.push_str("$");
            s.push_str("tmp_bundle\" '");
            s.push_str(&temp_ref);
            s.push_str(":");
            s.push_str(&temp_ref);
            s.push_str("' >/dev/null");
            s
        },
        checkout_line,
        format!(
            "git -C {} reset --hard {} >/dev/null",
            remote_dir = remote_dir_quoted,
            head = head_quoted,
        ),
        format!(
            "git -C {} clean -fdx -e .paperclip-runtime >/dev/null",
            remote_dir = remote_dir_quoted,
        ),
        format!(
            "git -C {} update-ref -d {} >/dev/null 2>&1 || true",
            remote_dir = remote_dir_quoted,
            temp_ref = temp_ref,
        ),
    ];
    let remote_setup_script = script_lines.join("\n");

        let result: Result<(), String> = async {
        // 1. update-ref
        run_local_git(
            &local_dir_str,
            &["update-ref", &temp_ref, &snapshot.head_commit],
            Some(10_000),
            Some(16 * 1024),
        )
        .await
        .map_err(|error| format!("local git update-ref failed: {error}"))?;

        // 2. bundle create
        run_local_git(
            &local_dir_str,
            &["bundle", "create", &bundle_path_str, &temp_ref],
            Some(60_000),
            Some(1024 * 1024),
        )
        .await
        .map_err(|error| format!("local git bundle create failed: {error}"))?;

        // 3. stream bundle → remote setup script
        stream_local_file_to_ssh(spec, &bundle_path, &remote_setup_script, progress)
            .await
            .map_err(|error| format!("ssh stream to remote failed: {error}"))?;
        Ok(())
    }
    .await;

    // Cleanup local ref + bundle dir (best-effort).
    let _ = run_local_git(
        &local_dir_str,
        &["update-ref", "-d", &temp_ref],
        Some(10_000),
        Some(16 * 1024),
    )
    .await;
    let _ = std::fs::remove_dir_all(&bundle_dir);

    result
}

/// Export a remote git workspace back to the local repo by streaming a
/// `git bundle` over SSH and resetting the local workspace to the imported
/// HEAD. Mirrors Node `exportGitWorkspaceFromSsh`.
///
/// Returns the imported HEAD commit SHA on success.
pub async fn export_git_workspace_from_ssh(
    spec: &SshRemoteExecutionSpec,
    remote_dir: &str,
    local_dir: &Path,
    reset_local_workspace: bool,
    progress: Option<&RuntimeProgressSink>,
) -> Result<String, String> {
    let local_dir_str = local_dir.to_string_lossy();
    let bundle_dir = std::env::temp_dir().join(format!(
        "paperclip-ssh-bundle-export-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&bundle_dir)
        .map_err(|error| format!("create bundle dir: {error}"))?;
    let bundle_path = bundle_dir.join("workspace.bundle");
    let bundle_path_str = bundle_path.to_string_lossy().into_owned();
    let imported_ref = format!(
        "refs/paperclip/ssh-sync/imported/{}",
        uuid::Uuid::new_v4()
    );

        // Build export script. Use string concatenation to avoid Rust 2021
    // reserved `$identifier` syntax in single string literals.
    let remote_dir_quoted = shell_quote(remote_dir);
    let runtime_dir_quoted = shell_quote(&format!("{remote_dir}/.paperclip-runtime"));
    let mut export_lines: Vec<String> = vec![
        "set -e".to_owned(),
        format!(
            "git -C {} update-ref refs/paperclip/ssh-sync/export HEAD",
            remote_dir = remote_dir_quoted,
        ),
        format!("mkdir -p {}", runtime_dir = runtime_dir_quoted),
        format!(
            "tmp_bundle=$(mktemp {}/export-XXXXXX.bundle)",
            runtime_dir = runtime_dir_quoted,
        ),
        // cleanup() body — assemble at runtime to avoid reserved prefix.
        String::from("cleanup() { rm -f \"")
            + "$" + "tmp_bundle"
            + &String::from("\"; git -C ")
            + &remote_dir_quoted
            + " update-ref -d refs/paperclip/ssh-sync/export >/dev/null 2>&1 || true; }",
        "trap cleanup EXIT".to_owned(),
        // bundle create — also assemble at runtime.
        String::from("git -C ")
            + &remote_dir_quoted
            + " bundle create \""
            + "$" + "tmp_bundle"
            + "\" refs/paperclip/ssh-sync/export >/dev/null",
        // cat "$tmp_bundle"
        String::from("cat \"") + "$" + "tmp_bundle" + "\"",
    ];
    let export_script = export_lines.join("\n");

    let result: Result<String, String> = async {
        // 1. stream remote → local bundle
        stream_ssh_to_local_file(spec, &export_script, &bundle_path)
            .await
            .map_err(|error| format!("ssh stream from remote failed: {error}"))?;

        // 2. fetch bundle into local importedRef
        let fetch_refspec = format!("refs/paperclip/ssh-sync/export:{imported_ref}");
        run_local_git(
            &local_dir_str,
            &["fetch", "--force", &bundle_path_str, &fetch_refspec],
            Some(60_000),
            Some(1024 * 1024),
        )
        .await
        .map_err(|error| format!("local git fetch bundle failed: {error}"))?;

        // 3. optional reset
        if reset_local_workspace {
            run_local_git(
                &local_dir_str,
                &["reset", "--hard", &imported_ref],
                Some(60_000),
                Some(1024 * 1024),
            )
            .await
            .map_err(|error| format!("local git reset --hard failed: {error}"))?;
        }

        // 4. rev-parse importedRef → imported head SHA
        let rev_parse = run_local_git(
            &local_dir_str,
            &["rev-parse", &imported_ref],
            Some(10_000),
            Some(16 * 1024),
        )
        .await
        .map_err(|error| format!("local git rev-parse failed: {error}"))?;
        Ok(rev_parse.stdout.trim().to_owned())
    }
    .await;

    // Cleanup local importedRef + bundle dir (best-effort).
    if reset_local_workspace {
        let _ = run_local_git(
            &local_dir_str,
            &["update-ref", "-d", &imported_ref],
            Some(10_000),
            Some(16 * 1024),
        )
        .await;
    }
    let _ = std::fs::remove_dir_all(&bundle_dir);

    result
}

/// POSIX shell single-quote a string (path-style, uses forward slashes).
/// Mirrors the Node `shellQuote` helper. We use forward slashes here so
/// remote shell scripts are portable across platforms.
// shell_quote is defined at line ~171; we use that one.


// =============================================================================
// Tar-based directory sync to SSH (port of Node `syncDirectoryToSsh`).
// Pipes `tar -cf -` (local) through `ssh` to `tar -xf - -C <remote>` on
// the SSH host. Used for non-git-backed workspaces (the common case).
// =============================================================================

/// Stream a local directory into a remote SSH host's directory by piping
/// `tar -cf -` through `ssh` to `tar -xf - -C <remote_dir>` on the host.
/// Mirrors Node `syncDirectoryToSsh` (no progress sink yet — that lands in
/// R499/R503).
///
/// - `exclude` adds `--exclude <pattern>` flags (always prepended with `._*`
///   via [`crate::ssh::tar_exclude_args`])
/// - `follow_symlinks=true` passes `-h` so symlinks are followed during the
///   archive creation (Node's `input.followSymlinks`)
///
/// Both processes must exit 0; the error includes the first non-zero exit's
/// stderr for diagnostics.
pub async fn sync_directory_to_ssh(
    spec: &SshRemoteExecutionSpec,
    local_dir: &Path,
    remote_dir: &str,
    exclude: Option<&[String]>,
    follow_symlinks: bool,
    progress: Option<&RuntimeProgressSink>,
) -> Result<(), String> {
    use crate::ssh::tar_exclude_args;
    use tokio::io::AsyncReadExt;

    // Build tar argv: tar [-h] -C <local_dir> <exclude args...> -cf - .
    let mut tar_args: Vec<String> = Vec::new();
    if follow_symlinks {
        tar_args.push("-h".to_owned());
    }
    tar_args.push("-C".to_owned());
    tar_args.push(local_dir.to_string_lossy().into_owned());
    tar_args.extend(tar_exclude_args(exclude));
    tar_args.push("-cf".to_owned());
    tar_args.push("-".to_owned());
    tar_args.push(".".to_owned());

    let mut tar_child = tokio::process::Command::new("tar")
        .args(&tar_args)
        .env_clear()
        .envs(crate::ssh::tar_spawn_env_defaults())
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn tar failed: {error}"))?;

    let tar_stdout = tar_child
        .stdout
        .take()
        .ok_or_else(|| "tar stdout pipe unavailable".to_owned())?;
    let mut tar_stderr = tar_child.stderr.take().ok_or_else(|| "tar stderr pipe unavailable".to_owned())?;

    // Build ssh argv + spawn.
    let auth = SshAuthArgs::create(&spec.as_connection_config()).map_err(|error| format!("ssh auth: {error}"))?;
    let remote_script = format!(
        "mkdir -p {} && tar -xf - -C {}",
        shell_quote(remote_dir),
        shell_quote(remote_dir),
    );
    let ssh_argv: Vec<String> = auth
        .args()
        .iter()
        .cloned()
        .chain([
            "-p".to_owned(),
            spec.port.to_string(),
            format!("{}@{}", spec.username, spec.host),
            format!("sh -c {}", shell_quote(&remote_script)),
        ])
        .collect();
    let mut ssh_child = tokio::process::Command::new("ssh")
        .args(&ssh_argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn ssh failed: {error}"))?;

    let mut ssh_stdin = ssh_child
        .stdin
        .take()
        .ok_or_else(|| "ssh stdin pipe unavailable".to_owned())?;
    let mut ssh_stderr = ssh_child.stderr.take().ok_or_else(|| "ssh stderr pipe unavailable".to_owned())?;

    // Pump tar.stdout → ssh.stdin concurrently with stderr drains. When a
    // progress sink is provided, wrap the tar stdout so every byte that flows
    // is counted and throttled-emitted.
    let mut pump = if let Some(sink) = progress {
        let progress_inner = create_transfer_progress(
            Box::new(tar_stdout_for_pump(tar_stdout)),
            TransferProgressOptions {
                on_progress: sink.clone(),
                phase: RuntimeProgressPhase::Syncing,
                direction: RuntimeProgressDirection::To,
                label: None,
                total_bytes: None,
                estimated: true,
            },
        );
        let mut counter = progress_inner.counter;
        let finish = progress_inner.finish;
        let fail = progress_inner.fail;
        tokio::spawn(async move {
            let result = tokio::io::copy(&mut counter, &mut ssh_stdin).await;
            match &result {
                Ok(_) => finish().await,
                Err(_) => fail().await,
            };
            result
        })
    } else {
        tokio::spawn(async move {
            tokio::io::copy(&mut tar_stdout_for_pump(tar_stdout), &mut ssh_stdin).await
        })
    };
    let tar_stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = tar_stderr.read_to_end(&mut buf).await;
        buf
    });
    let ssh_stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = ssh_stderr.read_to_end(&mut buf).await;
        buf
    });

    let tar_status = tar_child.wait().await.map_err(|error| format!("tar wait: {error}"))?;
    let ssh_status = ssh_child.wait().await.map_err(|error| format!("ssh wait: {error}"))?;
    let _ = (&mut pump).await;
    let tar_stderr_bytes = tar_stderr_task.await.unwrap_or_default();
    let ssh_stderr_bytes = ssh_stderr_task.await.unwrap_or_default();

    let tar_stderr_str = String::from_utf8_lossy(&tar_stderr_bytes).into_owned();
    let ssh_stderr_str = String::from_utf8_lossy(&ssh_stderr_bytes).into_owned();

    if !tar_status.success() {
        return Err(format!(
            "tar exited with code {:?}: {}",
            tar_status.code(),
            tar_stderr_str
        ));
    }
    if !ssh_status.success() {
        return Err(format!(
            "ssh exited with code {:?}: {}",
            ssh_status.code(),
            ssh_stderr_str
        ));
    }
    Ok(())
}

// Helper: turn `ChildStdout` into a type that `tokio::io::copy` accepts.
// Both `ChildStdout` and `File` implement `AsyncRead` directly, so we just
// pass through.
fn tar_stdout_for_pump(s: tokio::process::ChildStdout) -> tokio::process::ChildStdout {
    s
}



// =============================================================================
// Tar-based directory sync FROM SSH (port of Node `syncDirectoryFromSsh`).
// Streams `tar -cf -` on the remote via SSH to `tar -xf -` into a local
// staging directory, then atomically replaces the destination (clear +
// copy). Used for restore after remote execution.
// =============================================================================

/// Stream a remote directory into a local directory by piping the
/// remote `tar -cf -` through SSH to a local `tar -xf - -C <staging>`,
/// then atomically replace the destination (clear + copy). Mirrors Node
/// `syncDirectoryFromSsh` (no progress sink yet — that lands in R503).
///
/// - `exclude` adds `--exclude <pattern>` flags (always prepended with `._*`)
/// - `preserve_local_entries` are local paths kept during the clear step
///   (e.g. user `.env` files that should survive a restore)
pub async fn sync_directory_from_ssh(
    spec: &SshRemoteExecutionSpec,
    remote_dir: &str,
    local_dir: &Path,
    exclude: Option<&[String]>,
    preserve_local_entries: Option<&[String]>,
    progress: Option<&RuntimeProgressSink>,
) -> Result<(), String> {
    use crate::ssh::tar_exclude_args;
    use tokio::io::AsyncReadExt;

    // Staging dir: atomic replace via clear + copy (Node `clearLocalDirectory`
    // + `copyDirectoryContents`).
    let staging_dir = std::env::temp_dir().join(format!(
        "paperclip-ssh-sync-back-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&staging_dir)
        .map_err(|error| format!("create staging dir: {error}"))?;

    // Build remote script (Node joins with ` && ` so the cd applies first).
    // Avoid Rust 2021 `$identifier` parsing by using runtime concat.
    let exclude_args = tar_exclude_args(exclude);
    let mut tar_cmd_parts: Vec<String> = Vec::with_capacity(exclude_args.len() + 3);
    for arg in &exclude_args {
        tar_cmd_parts.push(shell_quote(arg));
    }
    tar_cmd_parts.push(shell_quote("-cf"));
    tar_cmd_parts.push(shell_quote("-"));
    tar_cmd_parts.push(shell_quote("."));
    let tar_cmd = tar_cmd_parts.join(" ");
    let remote_script = format!(
        "cd {} && tar {}",
        shell_quote(remote_dir),
        tar_cmd,
    );

    // Spawn ssh.
    let auth = SshAuthArgs::create(&spec.as_connection_config())
        .map_err(|error| format!("ssh auth: {error}"))?;
    let ssh_argv: Vec<String> = auth
        .args()
        .iter()
        .cloned()
        .chain([
            "-p".to_owned(),
            spec.port.to_string(),
            format!("{}@{}", spec.username, spec.host),
            format!("sh -c {}", shell_quote(&remote_script)),
        ])
        .collect();
    let mut ssh_child = tokio::process::Command::new("ssh")
        .args(&ssh_argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn ssh failed: {error}"))?;

    let ssh_stdout = ssh_child
        .stdout
        .take()
        .ok_or_else(|| "ssh stdout pipe unavailable".to_owned())?;
    let mut ssh_stderr = ssh_child.stderr.take().ok_or_else(|| "ssh stderr pipe unavailable".to_owned())?;

    // Spawn local tar.
    let staging_str = staging_dir.to_string_lossy().into_owned();
    let mut tar_child = tokio::process::Command::new("tar")
        .args(["-xf", "-", "-C", &staging_str])
        .env_clear()
        .envs(crate::ssh::tar_spawn_env_defaults())
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn tar failed: {error}"))?;

    let mut tar_stdin = tar_child
        .stdin
        .take()
        .ok_or_else(|| "tar stdin pipe unavailable".to_owned())?;
    let mut tar_stderr = tar_child.stderr.take().ok_or_else(|| "tar stderr pipe unavailable".to_owned())?;

    // Pump + drain stderr. When a progress sink is provided, wrap the ssh
    // stdout so every byte received is counted and throttled-emitted.
    let mut pump = if let Some(sink) = progress {
        let progress_inner = create_transfer_progress(
            Box::new(ssh_stdout_for_pump(ssh_stdout)),
            TransferProgressOptions {
                on_progress: sink.clone(),
                phase: RuntimeProgressPhase::Restoring,
                direction: RuntimeProgressDirection::From,
                label: None,
                total_bytes: None,
                estimated: true,
            },
        );
        let mut counter = progress_inner.counter;
        let finish = progress_inner.finish;
        let fail = progress_inner.fail;
        tokio::spawn(async move {
            let result = tokio::io::copy(&mut counter, &mut tar_stdin).await;
            match &result {
                Ok(_) => finish().await,
                Err(_) => fail().await,
            };
            result
        })
    } else {
        tokio::spawn(async move {
            tokio::io::copy(&mut ssh_stdout_for_pump(ssh_stdout), &mut tar_stdin).await
        })
    };
    let ssh_stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = ssh_stderr.read_to_end(&mut buf).await;
        buf
    });
    let tar_stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = tar_stderr.read_to_end(&mut buf).await;
        buf
    });

    let ssh_status = ssh_child.wait().await.map_err(|error| format!("ssh wait: {error}"))?;
    let tar_status = tar_child.wait().await.map_err(|error| format!("tar wait: {error}"))?;
    let _ = (&mut pump).await;
    let ssh_stderr_bytes = ssh_stderr_task.await.unwrap_or_default();
    let tar_stderr_bytes = tar_stderr_task.await.unwrap_or_default();
    let ssh_stderr_str = String::from_utf8_lossy(&ssh_stderr_bytes).into_owned();
    let tar_stderr_str = String::from_utf8_lossy(&tar_stderr_bytes).into_owned();

    if !ssh_status.success() {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(format!(
            "ssh exited with code {:?}: {}",
            ssh_status.code(),
            ssh_stderr_str
        ));
    }
    if !tar_status.success() {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(format!(
            "tar exited with code {:?}: {}",
            tar_status.code(),
            tar_stderr_str
        ));
    }

    // Atomic replace local_dir: clear (preserving entries) + copy staging contents.
    if let Err(error) = clear_local_directory(local_dir, preserve_local_entries) {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(format!("clear local dir: {error}"));
    }
    if let Err(error) = copy_directory_contents(&staging_dir, local_dir) {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(format!("copy staging: {error}"));
    }

    // Cleanup staging.
    let _ = std::fs::remove_dir_all(&staging_dir);
    Ok(())
}

/// Clear `local_dir` of all entries except those in `preserve_local_entries`.
/// Mirrors Node `clearLocalDirectory` (fs.mkdir recursive + fs.readdir + filter
/// + fs.rm recursive).
fn clear_local_directory(
    local_dir: &Path,
    preserve_entries: Option<&[String]>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(local_dir)?;
    let preserve: std::collections::HashSet<String> = preserve_entries
        .map(|entries| entries.iter().cloned().collect())
        .unwrap_or_default();
    for entry in std::fs::read_dir(local_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if preserve.contains(&name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Copy all entries from `source_dir` into `target_dir` (one level deep,
/// recursive). Mirrors Node `copyDirectoryContents`.
fn copy_directory_contents(
    source_dir: &Path,
    target_dir: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(target_dir)?;
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let from = entry.path();
        let to = target_dir.join(&file_name);
        copy_dir_entry(&from, &to)?;
    }
    Ok(())
}

/// Recursive copy helper (matches Node `fs.cp({ recursive, force,
/// preserveTimestamps })`). Plain recursive copy is sufficient for our
/// purposes — `preserveTimestamps` is best-effort.
fn copy_dir_entry(from: &Path, to: &Path) -> std::io::Result<()> {
    let from_meta = std::fs::metadata(from)?;
    if from_meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_dir_entry(&entry.path(), &to.join(&name))?;
        }
    } else if from_meta.is_file() {
        std::fs::copy(from, to)?;
    } else {
        // Skip symlinks / special files (Node's `fs.cp` would follow by
        // default, but our tar extraction doesn't create them either).
    }
    Ok(())
}

fn ssh_stdout_for_pump(s: tokio::process::ChildStdout) -> tokio::process::ChildStdout {
    s
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_archive_excludes_constant() {
        assert_eq!(GIT_ARCHIVE_EXCLUDES, &[".git", ".git/*"]);
    }

    #[test]
    fn create_imported_git_ref_uses_scope_and_uuid() {
        let ref_name = create_imported_git_ref("remote");
        assert!(ref_name.starts_with("refs/paperclip/git-sync/imported/remote/"));
        // UUID v4 is 36 chars
        assert_eq!(ref_name.len(), "refs/paperclip/git-sync/imported/remote/".len() + 36);
    }

    #[test]
    fn create_remote_git_export_ref_uses_scope_and_uuid() {
        let ref_name = create_remote_git_export_ref("sandbox");
        assert!(ref_name.starts_with("refs/paperclip/git-sync/export/sandbox/"));
    }

    #[test]
    fn refs_are_unique_across_calls() {
        let a = create_imported_git_ref("remote");
        let b = create_imported_git_ref("remote");
        assert_ne!(a, b);
    }

    #[test]
    fn is_missing_prerequisite_detects_known_markers() {
        // Simulate errors
        struct FakeErr(String);
        impl std::fmt::Display for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::fmt::Debug for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::error::Error for FakeErr {}

        let err1 = FakeErr("fatal: remote did not send all necessary objects".to_string());
        let err2 = FakeErr("error: lacks these prerequisite commits".to_string());
        let err3 = FakeErr("fatal: revision walk setup failed".to_string());
        let err4 = FakeErr("some other error".to_string());

        assert!(is_missing_git_prerequisite_error(&err1));
        assert!(is_missing_git_prerequisite_error(&err2));
        assert!(is_missing_git_prerequisite_error(&err3));
        assert!(!is_missing_git_prerequisite_error(&err4));
    }

    #[test]
    fn is_missing_prerequisite_anyhow_wrapper() {
        assert!(is_missing_git_prerequisite_error_anyhow(
            "fatal: remote did not send all necessary objects"
        ));
        assert!(!is_missing_git_prerequisite_error_anyhow("other error"));
    }

    #[test]
    fn build_delta_bundle_script_basic() {
        let opts = RemoteGitDeltaBundleOptions {
            remote_dir: "/workspace".to_string(),
            base_sha: "abc123".to_string(),
            export_ref: "refs/paperclip/export".to_string(),
            bundle_path: "/tmp/bundle.git".to_string(),
            status_path: None,
            cat_bundle: false,
            cleanup_bundle: false,
            force_full_bundle: false,
        };
        let script = build_remote_git_delta_bundle_script(&opts);
        assert!(script.starts_with("set -e"));
        assert!(script.contains("git -C '/workspace'"));
        assert!(script.contains("'abc123'"));
        assert!(script.contains("merge-base 'abc123' HEAD"));
        assert!(script.contains("bundle create"));
    }

    #[test]
    fn build_delta_bundle_script_force_full() {
        let opts = RemoteGitDeltaBundleOptions {
            remote_dir: "/workspace".to_string(),
            base_sha: "abc".to_string(),
            export_ref: "refs/export".to_string(),
            bundle_path: "/tmp/b.git".to_string(),
            status_path: None,
            cat_bundle: false,
            cleanup_bundle: false,
            force_full_bundle: true,
        };
        let script = build_remote_git_delta_bundle_script(&opts);
        assert!(script.contains("bundle_base=\"\""));
        // No merge-base call when force_full
        assert!(!script.contains("merge-base"));
    }

    #[test]
    fn build_delta_bundle_script_with_status_path() {
        let opts = RemoteGitDeltaBundleOptions {
            remote_dir: "/ws".to_string(),
            base_sha: "abc".to_string(),
            export_ref: "refs/export".to_string(),
            bundle_path: "/tmp/b.git".to_string(),
            status_path: Some("/tmp/status.txt".to_string()),
            cat_bundle: true,
            cleanup_bundle: true,
            force_full_bundle: false,
        };
        let script = build_remote_git_delta_bundle_script(&opts);
        assert!(script.contains("trap cleanup EXIT"));
        assert!(script.contains("printf clean >"));
        assert!(script.contains("printf dirty >"));
        assert!(script.contains("cat '/tmp/b.git'"));
        assert!(script.contains("rm -f '/tmp/status.txt'"));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("hello"), "'hello'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use std::path::PathBuf;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn init_repo_with_commit(name: &str, commit_message: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperclip-r497-git-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let dir_str = dir.to_string_lossy().to_string();
        run_local_git(&dir_str, &["init", "-q"], None, None)
            .await
            .expect("git init");
        run_local_git(
            &dir_str,
            &["config", "user.email", "test@example.com"],
            None,
            None,
        )
        .await
        .expect("git config email");
        run_local_git(
            &dir_str,
            &["config", "user.name", "Test"],
            None,
            None,
        )
        .await
        .expect("git config name");
        let readme = dir.join("README.md");
        std::fs::write(&readme, "# Hello\n").expect("write readme");
        run_local_git(&dir_str, &["add", "README.md"], None, None)
            .await
            .expect("git add");
        run_local_git(&dir_str, &["commit", "-q", "-m", commit_message], None, None)
            .await
            .expect("git commit");
        dir
    }

    #[tokio::test]
    async fn run_local_git_returns_stdout_for_rev_parse() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let dir = init_repo_with_commit("rev-parse", "init").await;
        let result = run_local_git(
            &dir.to_string_lossy(),
            &["rev-parse", "HEAD"],
            Some(5_000),
            None,
        )
        .await
        .expect("rev-parse must succeed");
        assert_eq!(result.stdout.trim().len(), 40, "SHA-1 must be 40 chars");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_local_git_returns_non_zero_exit_error_on_bad_command() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let dir = init_repo_with_commit("bad", "init").await;
        let err = run_local_git(
            &dir.to_string_lossy(),
            &["this-is-not-a-real-subcommand"],
            Some(5_000),
            None,
        )
        .await
        .expect_err("must error on bad command");
        match err {
            RunLocalGitError::NonZeroExit { status, stderr } => {
                assert!(status.unwrap_or(0) != 0);
                assert!(!stderr.is_empty(), "stderr must be captured");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_git_workspace_snapshot_returns_none_for_non_git_dir() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "paperclip-r497-nongit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let result = read_git_workspace_snapshot(&dir.to_string_lossy())
            .await
            .expect("snapshot must not error");
        assert!(result.is_none(), "non-git dir must yield None");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_git_workspace_snapshot_reads_head_for_clean_repo() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let dir = init_repo_with_commit("clean", "initial commit").await;
        let snapshot = read_git_workspace_snapshot(&dir.to_string_lossy())
            .await
            .expect("snapshot must succeed")
            .expect("must be Some for git dir");
        assert_eq!(snapshot.head_commit.len(), 40);
        assert!(snapshot.overlay_paths.is_empty());
        assert!(snapshot.deleted_paths.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_git_workspace_snapshot_picks_up_overlay_paths() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let dir = init_repo_with_commit("overlay", "init").await;
        std::fs::write(dir.join("untracked.txt"), "u").expect("write untracked");
        std::fs::write(dir.join("README.md"), "# Modified\n").expect("write readme");
        let snapshot = read_git_workspace_snapshot(&dir.to_string_lossy())
            .await
            .expect("snapshot must succeed")
            .expect("must be Some");
        assert!(
            snapshot.overlay_paths.contains(&"untracked.txt".to_owned()),
            "overlay_paths must contain untracked.txt; got {:?}",
            snapshot.overlay_paths
        );
        assert!(
            snapshot.overlay_paths.contains(&"README.md".to_owned()),
            "overlay_paths must contain README.md (modified); got {:?}",
            snapshot.overlay_paths
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_local_git_ref_swallows_errors() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let dir = init_repo_with_commit("delete", "init").await;
        let dir_str = dir.to_string_lossy().to_string();
        delete_local_git_ref(&dir_str, "refs/paperclip/does-not-exist")
            .await
            .expect("delete_local_git_ref must swallow errors");
        std::fs::remove_dir_all(&dir).ok();
    }
}

// =============================================================================
// SSH workspace preparation + restore (port of Node
// `prepareWorkspaceForSshExecution` + `restoreWorkspaceFromSshExecution` +
// `clearRemoteDirectory` + `removeDeletedPathsOnSsh`). These are the
// orchestration entry points the Claude/Codex adapters call before/after a
// remote run.
// =============================================================================

/// Recursively remove all entries in `remote_dir` except those whose names
/// (one level deep) match `preserve_entries`. Mirrors Node
/// `clearRemoteDirectory`.
///
/// We use `find <remote> -mindepth 1 -maxdepth 1 ! -name <p> -exec rm -rf {} +`
/// over ssh. `preserve_entries` is empty by default; pass `[".paperclip-runtime"]`
/// to keep runtime state.
pub async fn clear_remote_directory(
    spec: &SshRemoteExecutionSpec,
    remote_dir: &str,
    preserve_entries: Option<&[String]>,
) -> Result<(), String> {
    use crate::ssh::run_ssh_command;
    use std::collections::BTreeMap;
    let mut preserve_args = Vec::new();
    if let Some(entries) = preserve_entries {
        for entry in entries {
            preserve_args.push(String::from("!"));
            preserve_args.push(String::from("-name"));
            preserve_args.push(shell_quote(entry));
        }
    }
    let find_expr = format!(
        "find {} -mindepth 1 -maxdepth 1 {} -exec rm -rf -- {{}} +",
        shell_quote(remote_dir),
        preserve_args.join(" "),
    );
    let script = format!("set -e\nmkdir -p {}\n{}", shell_quote(remote_dir), find_expr);
    let config = spec.as_connection_config();
    run_ssh_command(
        &config,
        &script,
        &crate::ssh::SshCommandOptions {
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 30_000,
            max_buffer: 256 * 1024,
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("clear remote dir: {error}"))
}

/// On the remote, `rm -rf` each path in `deleted_paths` relative to
/// `remote_dir`. Mirrors Node `removeDeletedPathsOnSsh`. No-op if
/// `deleted_paths` is empty.
pub async fn remove_deleted_paths_on_ssh(
    spec: &SshRemoteExecutionSpec,
    remote_dir: &str,
    deleted_paths: &[String],
) -> Result<(), String> {
    use crate::ssh::run_ssh_command;
    use std::collections::BTreeMap;
    if deleted_paths.is_empty() {
        return Ok(());
    }
    let quoted_paths = deleted_paths
        .iter()
        .map(|p| shell_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "cd {} && rm -rf -- {}",
        shell_quote(remote_dir),
        quoted_paths,
    );
    let config = spec.as_connection_config();
    run_ssh_command(
        &config,
        &script,
        &crate::ssh::SshCommandOptions {
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 30_000,
            max_buffer: 256 * 1024,
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("remove deleted paths: {error}"))
}

/// Prepare a remote workspace before SSH execution. Mirrors Node
/// `prepareWorkspaceForSshExecution`.
///
/// Returns `true` if the local workspace is git-backed (so a git bundle
/// was pushed); `false` if it was treated as a plain directory.
pub async fn prepare_workspace_for_ssh_execution(
    spec: &SshRemoteExecutionSpec,
    local_dir: &Path,
    remote_dir: &str,
    progress: Option<&RuntimeProgressSink>,
    _baseline: Option<&DirectorySnapshot>,
) -> Result<bool, String> {
    let local_dir_str = local_dir.to_string_lossy();
    let git_snapshot = read_git_workspace_snapshot(&local_dir_str)
        .await
        .ok()
        .flatten();
    if let Some(snapshot) = git_snapshot {
        // Git-backed path.
        import_git_workspace_to_ssh(spec, local_dir, remote_dir, &snapshot, progress).await?;
        let exclude = vec![".git".to_string(), ".paperclip-runtime".to_string()];
        sync_directory_to_ssh(spec, local_dir, remote_dir, Some(&exclude), false, progress).await?;
        remove_deleted_paths_on_ssh(spec, remote_dir, &snapshot.deleted_paths).await?;
        Ok(true)
    } else {
        // Non-git path.
        let preserve = vec![".paperclip-runtime".to_string()];
        clear_remote_directory(spec, remote_dir, Some(&preserve)).await?;
        let exclude = vec![".paperclip-runtime".to_string()];
        sync_directory_to_ssh(spec, local_dir, remote_dir, Some(&exclude), false, progress).await?;
        Ok(false)
    }
}


/// Delete any orphan `refs/paperclip/ssh-sync/imported/*` refs left behind by
/// an earlier export_git_workspace_from_ssh call. Best-effort; errors are
/// ignored to mirror Node's `catch(() => undefined)` cleanup.
async fn cleanup_imported_git_refs(local_dir: &str) -> Result<(), String> {
    let list = run_local_git(
        local_dir,
        &["for-each-ref", "--format=%(refname)", "refs/paperclip/ssh-sync/imported"],
        Some(10_000),
        Some(64 * 1024),
    )
    .await;
    if let Ok(list) = list {
        for line in list.stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let _ = delete_local_git_ref(local_dir, line).await;
        }
    }
    Ok(())
}

/// Restore the local workspace from the remote after SSH execution. Mirrors
/// the no-`baselineSnapshot` and `baselineSnapshot` branches of Node
/// `restoreWorkspaceFromSshExecution` (ssh.ts L1559-1647).
///
/// When `baseline` is provided the function takes the early merge path:
/// - export remote git history (return imported head)
/// - sync remote tree into a staging directory (no target mutation yet)
/// - `mergeDirectoryWithBaseline` against `baseline`: deletes files the
///   remote run removed (and target still matches baseline), copies files
///   the remote run changed
/// - `integrateImportedGitHead` inside `before_apply` so dirty remote
///   edits win for the working tree while git history advances locally
///
/// When `baseline` is `None` falls back to the simpler git / non-git branch:
/// - if local was git-backed → push remote git history + restore working
///   tree (preserve `.git` so the just-imported ref stays)
/// - else → restore working tree only
pub async fn restore_workspace_from_ssh_execution(
    spec: &SshRemoteExecutionSpec,
    local_dir: &Path,
    remote_dir: &str,
    progress: Option<&RuntimeProgressSink>,
    baseline: Option<&DirectorySnapshot>,
) -> Result<(), String> {
    let local_dir_str = local_dir.to_string_lossy();
    if let Some(baseline_snapshot) = baseline {
        // baseline path: export git → sync to staging → merge + integrate.
        let imported_head = export_git_workspace_from_ssh(
            spec,
            remote_dir,
            local_dir,
            false,
            progress,
        )
        .await?;

        let staging_dir = std::env::temp_dir().join(format!(
            "paperclip-ssh-sync-back-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&staging_dir)
            .map_err(|error| format!("create staging dir: {error}"))?;

        sync_directory_from_ssh(
            spec,
            remote_dir,
            &staging_dir,
            Some(&baseline_snapshot.exclude),
            None,
            progress,
        )
        .await?;

        let local_dir_for_merge = local_dir.to_path_buf();
        let staging_for_merge = staging_dir.clone();
        let local_str_for_integrate = local_dir.to_string_lossy().into_owned();
        let imported_head_for_integrate = imported_head.clone();
        let before_apply: Box<
            dyn Fn() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>,
                > + Send
                + Sync,
        > = Box::new(move || {
            let local_str = local_str_for_integrate.clone();
            let head = imported_head_for_integrate.clone();
            Box::pin(async move {
                let _ = integrate_imported_git_head(&local_str, &head).await;
                Ok(())
            })
        });
        let after_apply: Box<
            dyn Fn() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>,
                > + Send
                + Sync,
        > = Box::new(|| Box::pin(async { Ok(()) }));
        let merge_result = merge_directory_with_baseline(MergeDirectoryWithBaselineInput {
            baseline: baseline_snapshot,
            source_dir: &staging_for_merge,
            target_dir: &local_dir_for_merge,
            before_apply,
            after_apply,
        })
        .await;

        // Cleanup staging + orphan imported refs.
        let _ = std::fs::remove_dir_all(&staging_dir);
        let _ = cleanup_imported_git_refs(&local_dir_str).await;

        merge_result.map_err(|error| format!("merge directory with baseline: {error}"))?;
        return Ok(());
    }

    let git_snapshot = read_git_workspace_snapshot(&local_dir_str)
        .await
        .ok()
        .flatten();
    if git_snapshot.is_some() {
        export_git_workspace_from_ssh(spec, remote_dir, local_dir, false, progress).await?;
        let exclude = vec![".git".to_string(), ".paperclip-runtime".to_string()];
        let preserve = vec![".git".to_string()];
        sync_directory_from_ssh(spec, remote_dir, local_dir, Some(&exclude), Some(&preserve), progress)
            .await?;
        Ok(())
    } else {
        let exclude = vec![".paperclip-runtime".to_string()];
        sync_directory_from_ssh(spec, remote_dir, local_dir, Some(&exclude), None, progress).await?;
        Ok(())
    }
}

// =============================================================================
// SSH workspace readiness + git history integration + baseline merge
// (port of Node `ensureSshWorkspaceReady` + `integrateImportedGitHead` +
// `mergeDirectoryWithBaseline` + `withDirectoryMergeLock`).
// =============================================================================

/// Ensure the remote SSH workspace exists and return its canonical cwd.
/// Mirrors Node `ensureSshWorkspaceReady` (L1649-1658 of
/// `packages/adapter-utils/src/ssh.ts`).
pub async fn ensure_ssh_workspace_ready(
    config: &crate::ssh::SshConnectionConfig,
) -> Result<SshReadyWorkspace, String> {
    use crate::ssh::run_ssh_command;
    use std::collections::BTreeMap;
    let remote_dir = &config.remote_workspace_path;
    let script = format!(
        "mkdir -p {} && cd {} && pwd",
        shell_quote(remote_dir),
        shell_quote(remote_dir)
    );
    let result = run_ssh_command(
        config,
        &script,
        &crate::ssh::SshCommandOptions {
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 15_000,
            max_buffer: 64 * 1024,
        },
    )
    .await
    .map_err(|error| format!("ensure ssh workspace ready: {error}"))?;
    Ok(SshReadyWorkspace {
        remote_cwd: result.stdout.trim().to_string(),
    })
}

/// Return value of [`ensure_ssh_workspace_ready`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshReadyWorkspace {
    pub remote_cwd: String,
}

/// Integrate a remote-imported git head into the local repository.
///
/// Mirrors Node `integrateImportedGitHead` (L303-385 of
/// `packages/adapter-utils/src/git-workspace-sync.ts`):
/// - up to 5 attempts to resolve concurrent ref updates
/// - merge-base check: if `importedHead` is already an ancestor of `HEAD`,
///   fast-forward `HEAD` (or branch ref) to `importedHead`
/// - if `currentHead` is an ancestor of `importedHead`, fast-forward
/// - otherwise compute a 3-way merge via `merge-tree` + `commit-tree`
///   and `update-ref` to the new merge commit
pub async fn integrate_imported_git_head(
    local_dir: &str,
    imported_head: &str,
) -> Result<(), String> {
    let head_ref = |snapshot: &GitWorkspaceSnapshot| -> String {
        match &snapshot.branch_name {
            Some(branch) => format!("refs/heads/{branch}"),
            None => "HEAD".to_string(),
        }
    };

    for attempt in 0..5 {
        let snapshot = match read_git_workspace_snapshot(local_dir).await {
            Ok(Some(snap)) => snap,
            Ok(None) => return Ok(()),
            Err(error) => return Err(format!("read snapshot: {error}")),
        };

        let current_head = &snapshot.head_commit;
        if current_head.is_empty() || current_head == imported_head {
            return Ok(());
        }

        let ref_arg = head_ref(&snapshot);

        // `git merge-base <a> <b>` — if it succeeds and stdout is `importedHead`,
        // `importedHead` is an ancestor of `currentHead`, so nothing to do.
        // If merge-base is `currentHead`, `currentHead` is an ancestor of
        // `importedHead`, so fast-forward.
        let merge_base_result = run_local_git(
            local_dir,
            &["merge-base", current_head, imported_head],
            Some(10_000),
            Some(16 * 1024),
        )
        .await;
        let merge_base_head = merge_base_result
            .ok()
            .map(|res| res.stdout.trim().to_string())
            .unwrap_or_default();

        if merge_base_head == imported_head {
            return Ok(());
        }

        if merge_base_head == current_head.as_str() {
            // Fast-forward: update the ref to importedHead. Provide
            // currentHead as the old value so concurrent updaters retry.
            match run_local_git(
                local_dir,
                &["update-ref", &ref_arg, imported_head, current_head],
                Some(10_000),
                Some(16 * 1024),
            )
            .await
            {
                Ok(_) => return Ok(()),
                Err(error) => {
                    if is_concurrent_ref_update_error(&error) && attempt < 4 {
                        continue;
                    }
                    return Err(format!("fast-forward update-ref: {error:?}"));
                }
            }
        }

        // 3-way merge via `merge-tree --write-tree` + `commit-tree`.
        let merged_tree = match run_local_git(
            local_dir,
            &["merge-tree", "--write-tree", current_head, imported_head],
            Some(60_000),
            Some(256 * 1024),
        )
        .await
        {
            Ok(out) => out,
            Err(error) => {
                return Err(format!(
                    "Failed to merge concurrent remote git histories for {} and {}: {:?}",
                    &current_head[..current_head.len().min(12)],
                    &imported_head[..imported_head.len().min(12)],
                    error
                ));
            }
        };
        let merged_tree_id = merged_tree
            .stdout
            .trim()
            .lines()
            .next()
            .map(|line| line.trim().to_string())
            .unwrap_or_default();
        if merged_tree_id.is_empty() {
            return Err("Failed to compute a merged git tree for workspace restore.".to_string());
        }

        let merge_message = format!(
            "Paperclip remote git sync merge {}",
            &imported_head[..imported_head.len().min(12)]
        );
        let merge_commit = match run_local_git(
            local_dir,
            &[
                "commit-tree",
                &merged_tree_id,
                "-p",
                current_head,
                "-p",
                imported_head,
                "-m",
                &merge_message,
            ],
            Some(60_000),
            Some(64 * 1024),
        )
        .await
        {
            Ok(out) => out,
            Err(error) => return Err(format!("commit-tree: {error:?}")),
        };
        let merge_commit_id = merge_commit.stdout.trim().to_string();
        if merge_commit_id.is_empty() {
            return Err("commit-tree returned empty stdout".to_string());
        }

        match run_local_git(
            local_dir,
            &["update-ref", &ref_arg, &merge_commit_id, current_head],
            Some(10_000),
            Some(16 * 1024),
        )
        .await
        {
            Ok(_) => return Ok(()),
            Err(error) => {
                if is_concurrent_ref_update_error(&error) && attempt < 4 {
                    continue;
                }
                return Err(format!("update-ref after merge: {error:?}"));
            }
        }
    }

    Err(format!(
        "Failed to integrate concurrent remote git history for {} after multiple retries.",
        &imported_head[..imported_head.len().min(12)]
    ))
}

/// Heuristic check for "cannot lock ref" race errors emitted by `git
/// update-ref` when another process is racing for the same ref.
fn is_concurrent_ref_update_error(error: &RunLocalGitError) -> bool {
    let message = format!("{error:?}");
    message.contains("cannot lock ref") && message.contains("expected")
}

// =============================================================================
// Directory baseline snapshot + merge (port of Node
// `mergeDirectoryWithBaseline` + `captureDirectorySnapshot` +
// `withDirectoryMergeLock` in `packages/adapter-utils/src/workspace-restore-merge.ts`).
// =============================================================================

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;
use sha2::{Digest, Sha256};

/// One entry in a directory snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectorySnapshotEntry {
    Dir,
    File { mode: u32, hash: String },
    Symlink { target: String },
}

/// A snapshot of a directory tree used as a baseline for the merge restore
/// path. Mirrors Node `DirectorySnapshot` (L20-24 of `workspace-restore-merge.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshot {
    pub exclude: Vec<String>,
    pub entries: BTreeMap<String, DirectorySnapshotEntry>,
}

async fn hash_file_sha256(file_path: &std::path::Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(file_path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Walk `root` recursively, collecting `DirectorySnapshot` entries. Skips
/// `exclude` patterns at any depth (forward-slash relative path).
fn should_exclude_relative(rel: &str, exclude: &[String]) -> bool {
    for pat in exclude {
        if rel == pat || rel.starts_with(&format!("{pat}/")) {
            return true;
        }
    }
    false
}

async fn walk_directory_recursive(
    root: &std::path::Path,
    exclude: &[String],
    relative: &str,
    out: &mut BTreeMap<String, DirectorySnapshotEntry>,
) -> std::io::Result<()> {
    // Iterative stack-based walk to avoid async recursion (Rust 2021 cannot
    // auto-box recursive async fns without #[recursion_limit]).
    let mut stack: Vec<String> = vec![relative.to_string()];
    while let Some(rel) = stack.pop() {
        let current = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            let next_relative = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if should_exclude_relative(&next_relative, exclude) {
                continue;
            }
            let full_path = root.join(&next_relative);
            let metadata = match tokio::fs::symlink_metadata(&full_path).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                out.insert(next_relative.clone(), DirectorySnapshotEntry::Dir);
                stack.push(next_relative);
            } else if file_type.is_symlink() {
                let target = tokio::fs::read_link(&full_path)
                    .await
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.insert(next_relative, DirectorySnapshotEntry::Symlink { target });
            } else if file_type.is_file() {
                let hash = hash_file_sha256(&full_path).await?;
                out.insert(
                    next_relative,
                    DirectorySnapshotEntry::File {
                        mode: metadata.permissions().mode(),
                        hash,
                    },
                );
            }
        }
    }
    Ok(())
}

/// Capture a `DirectorySnapshot` for `root_dir`. Mirrors Node
/// `captureDirectorySnapshot`.
pub async fn capture_directory_snapshot(
    root_dir: &std::path::Path,
    options: DirectorySnapshotOptions,
) -> std::io::Result<DirectorySnapshot> {
    let mut exclude = options.exclude;
    exclude.sort();
    exclude.dedup();
    let mut entries = BTreeMap::new();
    walk_directory_recursive(root_dir, &exclude, "", &mut entries).await?;
    Ok(DirectorySnapshot { exclude, entries })
}

/// Options for [`capture_directory_snapshot`].
#[derive(Debug, Default, Clone)]
pub struct DirectorySnapshotOptions {
    pub exclude: Vec<String>,
}

fn entries_match(
    left: Option<&DirectorySnapshotEntry>,
    right: Option<&DirectorySnapshotEntry>,
) -> bool {
    let (Some(l), Some(r)) = (left, right) else { return false };
    match (l, r) {
        (DirectorySnapshotEntry::Dir, DirectorySnapshotEntry::Dir) => true,
        (
            DirectorySnapshotEntry::Symlink { target: a },
            DirectorySnapshotEntry::Symlink { target: b },
        ) => a == b,
        (
            DirectorySnapshotEntry::File { mode: am, hash: ah },
            DirectorySnapshotEntry::File { mode: bm, hash: bh },
        ) => am == bm && ah == bh,
        _ => false,
    }
}

async fn copy_snapshot_entry(
    source_dir: &std::path::Path,
    target_dir: &std::path::Path,
    relative: &str,
    entry: &DirectorySnapshotEntry,
) -> std::io::Result<()> {
    let source_path = source_dir.join(relative);
    let target_path = target_dir.join(relative);
    match entry {
        DirectorySnapshotEntry::Dir => {
            if tokio::fs::metadata(&target_path).await.map(|m| m.is_dir()).unwrap_or(false) {
                return Ok(());
            }
            if tokio::fs::metadata(&target_path).await.is_ok() {
                let _ = tokio::fs::remove_dir_all(&target_path).await;
            }
            tokio::fs::create_dir_all(&target_path).await
        }
        DirectorySnapshotEntry::Symlink { target } => {
            if let Some(parent) = target_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let _ = tokio::fs::remove_file(&target_path).await;
            std::os::unix::fs::symlink(target, &target_path)
        }
        DirectorySnapshotEntry::File { .. } => {
            if let Some(parent) = target_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let _ = tokio::fs::remove_file(&target_path).await;
            tokio::fs::copy(&source_path, &target_path).await.map(|_| ())
        }
    }
}

/// Merge a `sourceDir` snapshot into a `targetDir`, using `baseline` to detect
/// which target-side entries were deleted in the source so they can be removed
/// from target. Mirrors Node `mergeDirectoryWithBaseline` (L212-243).
///
/// Strategy:
/// 1. Capture source snapshot (using baseline exclude set).
/// 2. Acquire `targetDir.paperclip-restore.lock`.
/// 3. Run `before_apply` callback.
/// 4. Capture target snapshot (using baseline exclude set).
/// 5. Remove target entries that exist in baseline but not in source AND
///    whose current target contents still match baseline (so we don't
///    delete user edits made during the remote run).
/// 6. Remove target dirs that exist in baseline but not in source.
/// 7. Copy source entries whose contents differ from baseline.
/// 8. Run `after_apply` callback.
/// 9. Release lock.
pub async fn merge_directory_with_baseline(input: MergeDirectoryWithBaselineInput<'_>) -> std::io::Result<()> {
    let baseline = input.baseline;
    let source_dir = input.source_dir;
    let target_dir = input.target_dir;
    let source = capture_directory_snapshot(source_dir, DirectorySnapshotOptions {
        exclude: baseline.exclude.clone(),
    })
    .await?;
    let lock_path = PathBuf::from(format!("{}.paperclip-restore.lock", target_dir.display()));
    let _ = acquire_directory_merge_lock(&lock_path).await?;
    let release_result: std::io::Result<()> = (async {
        (input.before_apply)().await?;
        let current = capture_directory_snapshot(target_dir, DirectorySnapshotOptions {
            exclude: baseline.exclude.clone(),
        })
        .await?;
        let mut deleted_leafs: Vec<(&String, &DirectorySnapshotEntry)> = baseline
            .entries
            .iter()
            .filter(|(_, entry)| !matches!(entry, DirectorySnapshotEntry::Dir))
            .filter(|(rel, _)| !source.entries.contains_key(*rel))
            .collect();
        deleted_leafs.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        for (relative, baseline_entry) in deleted_leafs {
            if !entries_match(current.entries.get(relative), Some(baseline_entry)) {
                continue;
            }
            let target_path = target_dir.join(relative);
            let _ = tokio::fs::remove_dir_all(&target_path).await;
            let _ = tokio::fs::remove_file(&target_path).await;
        }

        let mut deleted_dirs: Vec<&String> = baseline
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, DirectorySnapshotEntry::Dir))
            .filter(|(rel, _)| !source.entries.contains_key(*rel))
            .map(|(rel, _)| rel)
            .collect();
        deleted_dirs.sort_by(|a, b| b.len().cmp(&a.len()));
        for relative in deleted_dirs {
            let target_path = target_dir.join(relative);
            let _ = tokio::fs::remove_dir(&target_path).await;
        }

        let mut changed_source: Vec<(&String, &DirectorySnapshotEntry)> = source
            .entries
            .iter()
            .filter(|(rel, entry)| !entries_match(baseline.entries.get(*rel), Some(*entry)))
            .collect();
        changed_source.sort_by(|a, b| a.0.cmp(b.0));
        for (relative, entry) in changed_source {
            copy_snapshot_entry(source_dir, target_dir, relative, entry).await?;
        }

        (input.after_apply)().await?;
        Ok(())
    }).await;
    let _ = tokio::fs::remove_dir(&lock_path).await;
    release_result
}

/// Input for [`merge_directory_with_baseline`].
pub struct MergeDirectoryWithBaselineInput<'a> {
    pub baseline: &'a DirectorySnapshot,
    pub source_dir: &'a std::path::Path,
    pub target_dir: &'a std::path::Path,
    pub before_apply: Box<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>> + Send + Sync>,
    pub after_apply: Box<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>> + Send + Sync>,
}

async fn acquire_directory_merge_lock(lock_dir: &std::path::Path) -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    for _ in 0..600u32 {
        match tokio::fs::create_dir(lock_dir).await {
            Ok(_) => {
                let owner_path = lock_dir.join("owner.json");
                let pid = std::process::id();
                let created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let payload = format!("{{\"pid\":{pid},\"createdAt\":{created_at}}}\n");
                let _ = tokio::fs::write(&owner_path, payload).await;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !is_holder_alive(lock_dir).await {
                    let _ = tokio::fs::remove_dir_all(lock_dir).await;
                    // Loop continues; next iteration tries create_dir again.
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "Timed out waiting for workspace restore lock at {}",
                            lock_dir.display()
                        ),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "Exhausted retries waiting for workspace restore lock at {}",
            lock_dir.display()
        ),
    ))
}

async fn is_holder_alive(lock_dir: &std::path::Path) -> bool {
    let owner_path = lock_dir.join("owner.json");
    let raw = match tokio::fs::read_to_string(&owner_path).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let pid = parsed.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
    if pid == 0 {
        return false;
    }
    // Use `kill -0 <pid>` shell command — works on macOS + Linux without
    // needing libc bindings (which the crate refuses via `-F unsafe-code`).
    tokio::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Read a single snapshot entry for `relative` under `root_dir`. Mirrors Node
/// `readSnapshotEntry`.
pub async fn read_directory_snapshot_entry(
    root_dir: &std::path::Path,
    relative: &str,
) -> std::io::Result<Option<DirectorySnapshotEntry>> {
    let full_path = root_dir.join(relative);
    let metadata = match tokio::fs::symlink_metadata(&full_path).await {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        Ok(Some(DirectorySnapshotEntry::Dir))
    } else if file_type.is_symlink() {
        let target = tokio::fs::read_link(&full_path)
            .await
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Some(DirectorySnapshotEntry::Symlink { target }))
    } else if file_type.is_file() {
        let hash = hash_file_sha256(&full_path).await?;
        Ok(Some(DirectorySnapshotEntry::File {
            mode: metadata.permissions().mode(),
            hash,
        }))
    } else {
        Ok(None)
    }
}

/// Compare a single directory entry against a baseline entry. Mirrors Node
/// `directoryEntryMatchesBaseline`.
pub async fn directory_entry_matches_baseline(
    root_dir: &std::path::Path,
    relative: &str,
    baseline_entry: &DirectorySnapshotEntry,
) -> std::io::Result<bool> {
    let current = read_directory_snapshot_entry(root_dir, relative).await?;
    Ok(entries_match(current.as_ref(), Some(baseline_entry)))
}

#[cfg(test)]
mod r506_tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperclip-r506-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        dir
    }

    #[tokio::test]
    async fn capture_directory_snapshot_hashes_files() {
        let root = tmp_dir("snap");
        std::fs::write(root.join("a.txt"), "alpha").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "beta").unwrap();
        let snap = capture_directory_snapshot(&root, DirectorySnapshotOptions { exclude: vec![] })
            .await
            .expect("snap");
        assert!(snap.entries.contains_key("a.txt"));
        assert!(snap.entries.contains_key("sub"));
        assert!(snap.entries.contains_key("sub/b.txt"));
        if let Some(DirectorySnapshotEntry::File { hash, .. }) = snap.entries.get("a.txt") {
            assert!(!hash.is_empty(), "file hash must be non-empty");
        } else {
            panic!("a.txt should be a File entry");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn capture_directory_snapshot_respects_exclude() {
        let root = tmp_dir("snap-excl");
        std::fs::write(root.join("keep.txt"), "k").unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules").join("x.js"), "x").unwrap();
        let snap = capture_directory_snapshot(
            &root,
            DirectorySnapshotOptions { exclude: vec!["node_modules".to_owned()] },
        )
        .await
        .expect("snap");
        assert!(snap.entries.contains_key("keep.txt"));
        assert!(!snap.entries.contains_key("node_modules"));
        assert!(!snap.entries.contains_key("node_modules/x.js"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn merge_directory_with_baseline_removes_target_deletions() {
        let baseline_root = tmp_dir("baseline");
        let source_root = tmp_dir("source");
        let target_root = tmp_dir("target");

        // baseline: a.txt, b.txt, sub/c.txt
        std::fs::write(baseline_root.join("a.txt"), "alpha").unwrap();
        std::fs::write(baseline_root.join("b.txt"), "beta").unwrap();
        std::fs::create_dir_all(baseline_root.join("sub")).unwrap();
        std::fs::write(baseline_root.join("sub").join("c.txt"), "gamma").unwrap();

        // source: a.txt, sub/c.txt (b.txt was deleted remotely)
        std::fs::write(source_root.join("a.txt"), "alpha-new").unwrap();
        std::fs::create_dir_all(source_root.join("sub")).unwrap();
        std::fs::write(source_root.join("sub").join("c.txt"), "gamma-new").unwrap();

        // target: a.txt, b.txt, sub/c.txt
        std::fs::write(target_root.join("a.txt"), "alpha").unwrap();
        std::fs::write(target_root.join("b.txt"), "beta").unwrap();
        std::fs::create_dir_all(target_root.join("sub")).unwrap();
        std::fs::write(target_root.join("sub").join("c.txt"), "gamma").unwrap();

        let baseline = capture_directory_snapshot(
            &baseline_root,
            DirectorySnapshotOptions { exclude: vec![] },
        )
        .await
        .expect("baseline");

        merge_directory_with_baseline(MergeDirectoryWithBaselineInput {
            baseline: &baseline,
            source_dir: &source_root,
            target_dir: &target_root,
            before_apply: Box::new(|| Box::pin(async { Ok(()) })),
            after_apply: Box::new(|| Box::pin(async { Ok(()) })),
        })
        .await
        .expect("merge");

        // After merge:
        // a.txt and sub/c.txt should reflect source (changed)
        let a = std::fs::read_to_string(target_root.join("a.txt")).expect("read a");
        assert_eq!(a, "alpha-new");
        let c = std::fs::read_to_string(target_root.join("sub").join("c.txt")).expect("read c");
        assert_eq!(c, "gamma-new");
        // b.txt should be removed (deleted remotely and target still matches baseline)
        assert!(!target_root.join("b.txt").exists(), "b.txt must be removed");

        let _ = std::fs::remove_dir_all(&baseline_root);
        let _ = std::fs::remove_dir_all(&source_root);
        let _ = std::fs::remove_dir_all(&target_root);
    }

    #[tokio::test]
    async fn merge_directory_with_baseline_preserves_target_edits() {
        let baseline_root = tmp_dir("baseline-pres");
        let source_root = tmp_dir("source-pres");
        let target_root = tmp_dir("target-pres");

        std::fs::write(baseline_root.join("a.txt"), "alpha").unwrap();

        // source: a.txt (unchanged from baseline)
        std::fs::write(source_root.join("a.txt"), "alpha").unwrap();

        // target: a.txt modified by user during remote run
        std::fs::write(target_root.join("a.txt"), "USER_LOCAL_EDIT").unwrap();

        let baseline = capture_directory_snapshot(
            &baseline_root,
            DirectorySnapshotOptions { exclude: vec![] },
        )
        .await
        .expect("baseline");

        merge_directory_with_baseline(MergeDirectoryWithBaselineInput {
            baseline: &baseline,
            source_dir: &source_root,
            target_dir: &target_root,
            before_apply: Box::new(|| Box::pin(async { Ok(()) })),
            after_apply: Box::new(|| Box::pin(async { Ok(()) })),
        })
        .await
        .expect("merge");

        // Target edit must be preserved (source == baseline so no copy applied).
        let a = std::fs::read_to_string(target_root.join("a.txt")).expect("read a");
        assert_eq!(a, "USER_LOCAL_EDIT", "target edit must be preserved");

        let _ = std::fs::remove_dir_all(&baseline_root);
        let _ = std::fs::remove_dir_all(&source_root);
        let _ = std::fs::remove_dir_all(&target_root);
    }

    #[test]
    fn entries_match_file_compare_mode_and_hash() {
        let a = DirectorySnapshotEntry::File { mode: 0o644, hash: "abc".to_owned() };
        let b = DirectorySnapshotEntry::File { mode: 0o644, hash: "abc".to_owned() };
        let c = DirectorySnapshotEntry::File { mode: 0o755, hash: "abc".to_owned() };
        let d = DirectorySnapshotEntry::File { mode: 0o644, hash: "xyz".to_owned() };
        assert!(entries_match(Some(&a), Some(&b)));
        assert!(!entries_match(Some(&a), Some(&c)));
        assert!(!entries_match(Some(&a), Some(&d)));
        assert!(!entries_match(Some(&a), None));
        assert!(!entries_match(None, Some(&a)));
    }
}
