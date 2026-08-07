//! `pc-acpx` child stderr routing — port of `routeChildStderr`,
//! `flushChildStderr`, and `readChildStderrTail` from Node
//! `acpx-engine/execute.ts`.
//!
//! The Node implementation:
//! - Appends every stderr chunk to a per-child log file (so the tail is
//!   available for post-mortem diagnostics).
//! - Splits the buffered output on newlines, drops lines matching the
//!   benign `nes/close` no-method JSON-RPC error, and writes the rest to
//!   the host's stderr.
//! - Defers the last partial line (no trailing newline) until the next
//!   chunk arrives or the stream ends via `flushChildStderr`.
//!
//! The Rust port keeps the same shape and adds an `&mut dyn Write` seam so
//! tests can capture stderr output without spawning a real subprocess.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Regex that matches a benign JSON-RPC "method not found" error for the
/// internal `nes/close` notification. Mirrors
/// `/method: ['"]nes\/close['"].*-32601/` from the Node implementation.
pub static BENIGN_NES_CLOSE_STDERR: OnceLock<Regex> = OnceLock::new();

fn benign_regex() -> &'static Regex {
    BENIGN_NES_CLOSE_STDERR.get_or_init(|| {
        // Pattern: `method: 'nes/close'` or `method: "nes/close"` followed by
        // anything up to a `-32601` JSON-RPC error code.
        Regex::new(r#"method: ['"]nes/close['"].*-32601"#).expect("valid regex")
    })
}

// ============================================================================
// Public state
// ============================================================================

/// Per-child stderr routing state. `log_path` is the path of the per-child
/// stderr capture file (or `None` when the caller opted out of capture).
/// `pending_live_line` is the buffered partial line that has not yet seen a
/// newline terminator.
#[derive(Debug, Clone)]
pub struct ChildStderrState {
    pub log_path: Option<PathBuf>,
    pub pending_live_line: String,
}

impl ChildStderrState {
    /// Build a fresh state. Pass `None` to disable per-child capture.
    pub fn new(log_path: Option<impl Into<PathBuf>>) -> Self {
        Self {
            log_path: log_path.map(Into::into),
            pending_live_line: String::new(),
        }
    }

    /// Convenience constructor for the most common case: a state with no
    /// log file capture.
    pub fn without_log() -> Self {
        Self::new(Option::<PathBuf>::None)
    }
}

// ============================================================================
// routeChildStderr
// ============================================================================

/// Append `chunk` to the per-child log file (when configured), then route
/// the host-visible stderr to `stderr`. The internal pending-live-line
/// buffer keeps any trailing partial line so we never split a benign
/// `nes/close` notification in half.
pub fn route_child_stderr(
    state: &mut ChildStderrState,
    chunk: &str,
) -> Result<RoutedStderr, ChildStderrError> {
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut writer = CaptureWriter(&mut stderr_buf);
    route_child_stderr_with(state, chunk, &mut writer)?;
    Ok(RoutedStderr {
        host_visible: String::from_utf8_lossy(&stderr_buf).into_owned(),
    })
}

/// Like [`route_child_stderr`] but writes the host-visible portion to the
/// caller-supplied writer. Returns the empty string when nothing was
/// routed to stderr.
pub fn route_child_stderr_with<W: Write>(
    state: &mut ChildStderrState,
    chunk: &str,
    stderr: &mut W,
) -> Result<(), ChildStderrError> {
    if let Some(log_path) = state.log_path.as_ref() {
        append_to_log_sync(log_path, chunk).map_err(|error| ChildStderrError::LogAppend {
            path: log_path.clone(),
            error,
        })?;
    }
    let combined = format!("{}{}", state.pending_live_line, chunk);
    let last_newline = combined.rfind('\n');
    let Some(last_newline_idx) = last_newline else {
        state.pending_live_line = combined;
        return Ok(());
    };
    let complete = &combined[..=last_newline_idx];
    state.pending_live_line = combined[last_newline_idx + 1..].to_string();
    let filtered = filter_lines(complete);
    if !filtered.is_empty() {
        stderr
            .write_all(filtered.as_bytes())
            .map_err(ChildStderrError::StderrWrite)?;
    }
    Ok(())
}

/// Return value of [`route_child_stderr`] when no writer was supplied. The
/// host-visible portion is collected into `host_visible`.
#[derive(Debug, Clone, Default)]
pub struct RoutedStderr {
    pub host_visible: String,
}

struct CaptureWriter<'a>(&'a mut Vec<u8>);

impl Write for CaptureWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        std::io::Write::write(&mut self.0, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn append_to_log_sync(path: &Path, chunk: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(chunk.as_bytes())?;
    Ok(())
}

/// Split `complete` on newlines (preserving the trailing `\n`), drop any
/// line that matches the benign regex, and re-join.
fn filter_lines(complete: &str) -> String {
    let re = benign_regex();
    let mut out = String::with_capacity(complete.len());
    let mut start = 0usize;
    for (idx, _) in complete.match_indices('\n') {
        let line_with_newline = &complete[start..=idx];
        if !re.is_match(line_with_newline) {
            out.push_str(line_with_newline);
        }
        start = idx + 1;
    }
    out
}

// ============================================================================
// flushChildStderr
// ============================================================================

/// Flush the trailing partial line of `state` to the host's stderr. Drops
/// benign `nes/close` notifications the same way `route_child_stderr`
/// does. Mirrors the Node `flushChildStderr` semantics.
pub fn flush_child_stderr(state: &mut ChildStderrState) -> Result<FlushedStderr, ChildStderrError> {
    let mut stderr_buf: Vec<u8> = Vec::new();
    flush_child_stderr_with(state, &mut stderr_buf)?;
    Ok(FlushedStderr {
        host_visible: String::from_utf8_lossy(&stderr_buf).into_owned(),
    })
}

/// Like [`flush_child_stderr`] but writes the host-visible portion to the
/// caller-supplied writer.
pub fn flush_child_stderr_with<W: Write>(
    state: &mut ChildStderrState,
    stderr: &mut W,
) -> Result<(), ChildStderrError> {
    let pending = std::mem::take(&mut state.pending_live_line);
    if !pending.is_empty() && !benign_regex().is_match(&pending) {
        stderr
            .write_all(pending.as_bytes())
            .map_err(ChildStderrError::StderrWrite)?;
    }
    Ok(())
}

/// Return value of [`flush_child_stderr`] when no writer was supplied.
#[derive(Debug, Clone, Default)]
pub struct FlushedStderr {
    pub host_visible: String,
}

// ============================================================================
// readChildStderrTail
// ============================================================================

/// Read up to `max_bytes` from the tail of `log_path`. Return `None` when
/// the log path is absent, when the file does not exist, when the file is
/// empty, or when the resulting tail is whitespace-only.
///
/// Mirrors the Node `readChildStderrTail` helper: any read/open error is
/// swallowed into `None` so the failure path of the engine can never raise
/// from the diagnostics lookup.
pub async fn read_child_stderr_tail(log_path: Option<&Path>, max_bytes: usize) -> Option<String> {
    let path = log_path?;
    let metadata = tokio::fs::metadata(path).await.ok()?;
    if metadata.len() == 0 {
        return None;
    }
    let mut handle = OpenOptions::new().read(true).open(path).await.ok()?;
    let read_bytes = (metadata.len() as usize).min(max_bytes);
    let mut buffer = vec![0u8; read_bytes];
    let start_pos = metadata.len().saturating_sub(read_bytes as u64);
    if read_tail_fallback(&mut handle, &mut buffer, start_pos)
        .await
        .is_err()
    {
        return None;
    }
    let _ = handle.shutdown().await;
    let tail = String::from_utf8_lossy(&buffer).trim().to_string();
    if tail.is_empty() {
        None
    } else {
        Some(tail)
    }
}

async fn read_tail_fallback(
    handle: &mut tokio::fs::File,
    buffer: &mut [u8],
    start_pos: u64,
) -> io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    handle.seek(io::SeekFrom::Start(start_pos)).await?;
    let mut offset = 0usize;
    while offset < buffer.len() {
        let n = handle.read(&mut buffer[offset..]).await?;
        if n == 0 {
            break;
        }
        offset += n;
    }
    Ok(())
}

// ============================================================================
// Error type
// ============================================================================

/// Errors surfaced by the synchronous routing helpers. The async
/// `read_child_stderr_tail` swallows its own errors into `None` so the
/// engine failure path can call it safely.
#[derive(Debug, thiserror::Error)]
pub enum ChildStderrError {
    /// The per-child stderr log file could not be appended to.
    #[error("failed to append to child stderr log `{path}`: {error}")]
    LogAppend {
        path: PathBuf,
        #[source]
        error: io::Error,
    },
    /// The host-visible stderr writer refused the write.
    #[error("failed to write child stderr: {0}")]
    StderrWrite(#[source] io::Error),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_log_path(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pc-acpx-childstderr-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        dir.join("child.stderr.log")
    }

    fn append_read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn pending_line_buffered_until_newline() {
        let path = unique_log_path("pending");
        let mut state = ChildStderrState::new(Some(path.clone()));
        let chunk = "first half without newline";
        let routed = route_child_stderr(&mut state, chunk).unwrap();
        assert_eq!(routed.host_visible, "");
        assert_eq!(state.pending_live_line, "first half without newline");
        assert!(path.parent().map(|p| p.exists()).unwrap_or(false));
        assert_eq!(append_read(&path), chunk);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn benign_nes_close_filtered() {
        let path = unique_log_path("benign");
        let mut state = ChildStderrState::new(Some(path.clone()));
        // Two lines: first benign, second a real stderr line. Both must end
        // up appended to the log file but only the second reaches the host.
        let chunk = "method: 'nes/close' -32601 unknown method\nreal error line\n";
        let mut captured: Vec<u8> = Vec::new();
        route_child_stderr_with(&mut state, chunk, &mut captured).unwrap();
        assert_eq!(String::from_utf8_lossy(&captured), "real error line\n");
        assert_eq!(state.pending_live_line, "");
        assert_eq!(append_read(&path), chunk);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn benign_nes_close_with_double_quotes_also_filtered() {
        let path = unique_log_path("benign2");
        let mut state = ChildStderrState::new(Some(path.clone()));
        let chunk = "method: \"nes/close\" -32601 missing\nkeep me\n";
        let mut captured: Vec<u8> = Vec::new();
        route_child_stderr_with(&mut state, chunk, &mut captured).unwrap();
        assert_eq!(String::from_utf8_lossy(&captured), "keep me\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn chunk_with_newline_writes_filtered_to_stderr() {
        let path = unique_log_path("newline");
        let mut state = ChildStderrState::new(Some(path.clone()));
        let chunk = "alpha\nbeta\n";
        let mut captured: Vec<u8> = Vec::new();
        route_child_stderr_with(&mut state, chunk, &mut captured).unwrap();
        assert_eq!(String::from_utf8_lossy(&captured), "alpha\nbeta\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pending_carries_across_chunks_until_newline() {
        let path = unique_log_path("pending2");
        let mut state = ChildStderrState::new(Some(path.clone()));
        let mut captured: Vec<u8> = Vec::new();
        route_child_stderr_with(&mut state, "half", &mut captured).unwrap();
        assert_eq!(captured.len(), 0);
        route_child_stderr_with(&mut state, " rest\n", &mut captured).unwrap();
        assert_eq!(String::from_utf8_lossy(&captured), "half rest\n");
        assert_eq!(state.pending_live_line, "");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn flush_emits_pending_when_non_benign() {
        let mut state = ChildStderrState::new(Option::<PathBuf>::None);
        state.pending_live_line = "trailing line".to_string();
        let flushed = flush_child_stderr(&mut state).unwrap();
        assert_eq!(flushed.host_visible, "trailing line");
        assert_eq!(state.pending_live_line, "");
    }

    #[test]
    fn flush_drops_benign_pending() {
        let mut state = ChildStderrState::new(Option::<PathBuf>::None);
        state.pending_live_line = "method: 'nes/close' -32601".to_string();
        let flushed = flush_child_stderr(&mut state).unwrap();
        assert_eq!(flushed.host_visible, "");
        assert_eq!(state.pending_live_line, "");
    }

    #[test]
    fn flush_with_empty_pending_is_noop() {
        let mut state = ChildStderrState::new(Option::<PathBuf>::None);
        let flushed = flush_child_stderr(&mut state).unwrap();
        assert_eq!(flushed.host_visible, "");
    }

    #[tokio::test]
    async fn read_child_stderr_tail_returns_none_when_path_absent() {
        let result = read_child_stderr_tail(None, 4096).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_child_stderr_tail_returns_none_for_missing_file() {
        let result =
            read_child_stderr_tail(Some(Path::new("/nonexistent/path/missing.log")), 4096).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_child_stderr_tail_returns_tail_for_existing_file() {
        let dir = std::env::temp_dir().join(format!(
            "pc-acpx-tail-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("stderr.log");
        let payload = "first line\nsecond line\nthird line\nfourth line\n";
        tokio::fs::write(&path, payload).await.unwrap();
        let tail = read_child_stderr_tail(Some(&path), 4096).await.unwrap();
        assert_eq!(tail, payload.trim());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn read_child_stderr_tail_truncates_to_max_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "pc-acpx-tail-trunc-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("stderr.log");
        let payload = "a".repeat(2048);
        tokio::fs::write(&path, &payload).await.unwrap();
        let tail = read_child_stderr_tail(Some(&path), 64).await.unwrap();
        assert_eq!(tail.len(), 64);
        // Tail should come from the END of the file.
        assert!(payload.ends_with(&tail));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn read_child_stderr_tail_returns_none_for_empty_file() {
        let dir = std::env::temp_dir().join(format!(
            "pc-acpx-tail-empty-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("stderr.log");
        tokio::fs::write(&path, "").await.unwrap();
        let tail = read_child_stderr_tail(Some(&path), 4096).await;
        assert!(tail.is_none());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
