//! Codex 输出不活动监控（R433）。
//!
//! 复刻 Node `packages/adapters/codex-local/src/server/output-inactivity-monitor.ts`：
//! - 子进程长时间无输出/无进程活动时触发 `onFire`；
//! - 心跳判定：stdout 行可被 JSON 解析即视为心跳（stderr 只重置计时）；
//! - 支持注入 `now` 与 `set_timer/clear_timer`，便于 fake-clock 离线单测。
//!
//! 设计上状态机与 timer 完全解耦：`OutputInactivityMonitor` 是同步状态机，
//! `spawn_monitor` 用 tokio 驱动计时；测试用 `FakeClock` 直接驱动。

use std::sync::Arc;
use std::sync::Mutex;

/// 默认 30 分钟无活动超时。
pub const DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS: u64 = 30 * 60 * 1000;
/// SIGTERM 宽限 5 秒后升级 SIGKILL。
pub const CODEX_OUTPUT_INACTIVITY_MONITOR_SIGTERM_GRACE_MS: u64 = 5_000;

/// 超时解析结果（对齐 Node `CodexOutputInactivityMonitorResolution`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexOutputInactivityResolution {
    /// 未配置/缺失 → 默认 30m。
    Default { timeout_ms: u64 },
    /// 显式 null → 禁用。
    Disabled { reason: &'static str },
    /// 显式配置的正数超时。
    Configured { timeout_ms: u64 },
}

impl CodexOutputInactivityResolution {
    /// 当前是否禁用。
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled { .. })
    }

    /// 解析后的实际超时（disabled 为 None）。
    #[must_use]
    pub fn timeout_ms(&self) -> Option<u64> {
        match self {
            Self::Default { timeout_ms } | Self::Configured { timeout_ms } => Some(*timeout_ms),
            Self::Disabled { .. } => None,
        }
    }
}

/// 解析原始配置值（对齐 Node `resolveCodexInactivityTimeout`）。
///
/// - `null` → disabled；
/// - 缺失/非数字 → 默认 30m；
/// - 数字 > 0 → configured；
/// - 数字 ≤ 0 → 默认 30m（non_positive 备注）。
#[must_use]
pub fn resolve_codex_inactivity_timeout(
    raw_value: Option<&serde_json::Value>,
) -> CodexOutputInactivityResolution {
    match raw_value {
        Some(serde_json::Value::Null) => CodexOutputInactivityResolution::Disabled {
            reason: "explicit_null",
        },
        Some(serde_json::Value::Number(number)) => {
            if let Some(value) = number.as_f64() {
                if value.is_finite() && value > 0.0 {
                    return CodexOutputInactivityResolution::Configured {
                        timeout_ms: value as u64,
                    };
                }
            }
            CodexOutputInactivityResolution::Default {
                timeout_ms: DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS,
            }
        }
        _ => CodexOutputInactivityResolution::Default {
            timeout_ms: DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS,
        },
    }
}

/// 监控状态（对齐 Node `CodexOutputInactivityMonitorState`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexOutputInactivityState {
    pub fired: bool,
    pub spawned_at: u64,
    pub last_event_at: u64,
    pub fired_at: Option<u64>,
    pub output_chunk_count: u64,
    pub output_bytes: u64,
    pub parsed_event_count: u64,
    pub process_activity_count: u64,
}

/// 心跳行判定（默认：stdout 行可被 JSON 解析）。
fn default_is_heartbeat_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// 同步状态机（与 timer 解耦，可注入 fake clock 测试）。
pub struct OutputInactivityMonitor {
    timeout_ms: u64,
    state: CodexOutputInactivityState,
    stopped: bool,
    on_fire: Box<dyn FnMut(&CodexOutputInactivityState) + Send>,
    is_heartbeat_line: Box<dyn Fn(&str) -> bool + Send>,
}

impl OutputInactivityMonitor {
    /// 创建监控状态机。`timeout_ms` 必须 > 0（对齐 Node 抛错）。
    pub fn new(
        timeout_ms: u64,
        now_ms: u64,
        on_fire: impl FnMut(&CodexOutputInactivityState) + Send + 'static,
    ) -> Result<Self, String> {
        if timeout_ms == 0 {
            return Err(format!(
                "createCodexOutputInactivityMonitor requires timeoutMs > 0 (got {timeout_ms})"
            ));
        }
        Ok(Self {
            timeout_ms,
            state: CodexOutputInactivityState {
                fired: false,
                spawned_at: now_ms,
                last_event_at: now_ms,
                fired_at: None,
                output_chunk_count: 0,
                output_bytes: 0,
                parsed_event_count: 0,
                process_activity_count: 0,
            },
            stopped: false,
            on_fire: Box::new(on_fire),
            is_heartbeat_line: Box::new(default_is_heartbeat_line),
        })
    }

    /// 自定义心跳判定。
    pub fn with_heartbeat(
        mut self,
        is_heartbeat_line: impl Fn(&str) -> bool + Send + 'static,
    ) -> Self {
        self.is_heartbeat_line = Box::new(is_heartbeat_line);
        self
    }

    /// 当前是否已触发或已停止。
    fn inactive(&self) -> bool {
        self.stopped || self.state.fired
    }

    /// 记录一个输出块（stdout 行解析心跳，stderr 只重置计时）。
    pub fn note_output_chunk(&mut self, stream: &str, chunk: &str, now_ms: u64) {
        if self.inactive() || chunk.is_empty() {
            return;
        }
        self.state.output_chunk_count += 1;
        self.state.output_bytes = self.state.output_bytes.saturating_add(chunk.len() as u64);
        if stream == "stdout" {
            for raw_line in chunk.split('\n') {
                let line = raw_line.trim_end_matches('\r');
                if (self.is_heartbeat_line)(line) {
                    self.state.parsed_event_count += 1;
                }
            }
        }
        self.state.last_event_at = now_ms;
    }

    /// 记录进程活动（重置计时）。
    pub fn note_process_activity(&mut self, now_ms: u64) {
        if self.inactive() {
            return;
        }
        self.state.process_activity_count += 1;
        self.state.last_event_at = now_ms;
    }

    /// 检查是否超过不活动阈值，超时则触发一次。
    ///
    /// 由 timer 驱动（tokio task 或 fake clock）。
    pub fn check_timeout(&mut self, now_ms: u64) {
        if self.inactive() {
            return;
        }
        if now_ms.saturating_sub(self.state.last_event_at) >= self.timeout_ms {
            self.state.fired = true;
            self.state.fired_at = Some(now_ms);
            let snapshot = self.state.clone();
            (self.on_fire)(&snapshot);
        }
    }

    /// 当前状态快照。
    #[must_use]
    pub fn state(&self) -> &CodexOutputInactivityState {
        &self.state
    }

    /// 停止监控（幂等），返回最终状态。
    pub fn stop(&mut self) -> CodexOutputInactivityState {
        self.stopped = true;
        self.state.clone()
    }
}

/// 格式化错误消息（对齐 Node `formatOutputInactivityMonitorErrorMessage`）。
#[must_use]
pub fn format_output_inactivity_monitor_error_message(elapsed_ms: u64) -> String {
    let total = elapsed_ms.div_euclid(1000);
    let minutes = total / 60;
    let seconds = total % 60;
    format!("monitor: no codex activity (output or process) for {minutes}m {seconds}s")
}

/// 运行中的监控句柄（tokio 驱动）。
pub struct RunningMonitor {
    inner: Arc<Mutex<OutputInactivityMonitor>>,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    clock: std::time::Instant,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RunningMonitor {
    /// 记录输出块（线程安全）。
    pub fn note_output_chunk(&self, stream: &str, chunk: &str) {
        let now_ms = self.clock.elapsed().as_millis() as u64;
        self.inner
            .lock()
            .expect("monitor lock poisoned")
            .note_output_chunk(stream, chunk, now_ms);
    }

    /// 记录进程活动。
    pub fn note_process_activity(&self) {
        let now_ms = self.clock.elapsed().as_millis() as u64;
        self.inner
            .lock()
            .expect("monitor lock poisoned")
            .note_process_activity(now_ms);
    }

    /// 状态快照。
    #[must_use]
    pub fn state(&self) -> CodexOutputInactivityState {
        self.inner
            .lock()
            .expect("monitor lock poisoned")
            .state()
            .clone()
    }
}

impl Drop for RunningMonitor {
    fn drop(&mut self) {
        self.stop_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// 启动一个后台监控任务（对齐 Node `createCodexOutputInactivityMonitor` 的自动 arm）。
///
/// 每 `tick_ms`（默认 250ms）检查一次；`stop()` 通过 drop 或返回的 handle 终止。
pub fn spawn_monitor(
    timeout_ms: u64,
    on_fire: impl FnMut(&CodexOutputInactivityState) + Send + 'static,
) -> Result<RunningMonitor, String> {
    // 单调时钟基准（等价 Node `Date.now()` 的间隔语义）。
    let clock = std::time::Instant::now();
    let now_ms = clock.elapsed().as_millis() as u64;
    let inner = Arc::new(Mutex::new(OutputInactivityMonitor::new(
        timeout_ms, now_ms, on_fire,
    )?));
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_inner = Arc::clone(&inner);
    let task_stop = Arc::clone(&stop_flag);
    let task_clock = clock;
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            ticker.tick().await;
            if task_stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let now_ms = task_clock.elapsed().as_millis() as u64;
            let mut guard = task_inner.lock().expect("monitor lock poisoned");
            let before = guard.state().fired;
            guard.check_timeout(now_ms);
            let fired_now = guard.state().fired && !before;
            drop(guard);
            if fired_now {
                // on_fire 已由状态机在 check_timeout 内同步调用；这里只负责终止循环。
                break;
            }
        }
    });
    Ok(RunningMonitor {
        inner,
        stop_flag,
        clock,
        task: Some(task),
    })
}

/// 采样 Codex 进程活动（Linux `/proc`，对齐 Node `sampleCodexProcessActivity`）。
///
/// 非 Linux 平台返回 `None`（与 Node 一致）。
pub async fn sample_codex_process_activity(
    pid: u32,
    process_group_id: Option<i32>,
) -> Option<CodexProcessActivitySnapshot> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let target_group = process_group_id.filter(|value| *value > 0);
    let entries: Vec<String> = match &target_group {
        Some(_) => {
            let mut entries = Vec::new();
            if let Ok(read_dir) = tokio::fs::read_dir("/proc").await {
                let mut stream = read_dir;
                while let Ok(Some(entry)) = stream.next_entry().await {
                    entries.push(entry.file_name().to_string_lossy().to_string());
                }
            }
            entries
        }
        None => vec![pid.to_string()],
    };

    let mut process_ids: Vec<u32> = Vec::new();
    let mut cpu_ticks: u64 = 0;
    let mut io_bytes: u64 = 0;

    for entry in entries {
        if !entry.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = tokio::fs::read_to_string(format!("/proc/{entry}/stat")).await else {
            continue;
        };
        let Some(parsed) = parse_proc_stat(&stat) else {
            continue;
        };
        if let Some(group) = target_group {
            if parsed.process_group_id != group {
                continue;
            }
        } else if entry != pid.to_string() {
            continue;
        }
        let io = tokio::fs::read_to_string(format!("/proc/{entry}/io"))
            .await
            .unwrap_or_default();
        process_ids.push(entry.parse::<u32>().ok()?);
        cpu_ticks = cpu_ticks.saturating_add(parsed.cpu_ticks);
        io_bytes = io_bytes.saturating_add(parse_proc_io(&io));
    }

    if process_ids.is_empty() {
        return None;
    }
    process_ids.sort_unstable();
    Some(CodexProcessActivitySnapshot {
        cpu_ticks,
        io_bytes,
        process_ids: process_ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    })
}

/// 进程活动快照（对齐 Node `CodexProcessActivitySnapshot`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProcessActivitySnapshot {
    pub cpu_ticks: u64,
    pub io_bytes: u64,
    pub process_ids: String,
}

/// 解析 `/proc/<pid>/stat`：进程组 ID + CPU ticks。
fn parse_proc_stat(stat: &str) -> Option<ProcStat> {
    let command_end = stat.rfind(')')?;
    let fields: Vec<&str> = stat[command_end + 2..].trim().split_whitespace().collect();
    // fields[0]=state, [1]=ppid, [2]=pgrp, [11]=utime, [12]=stime
    let process_group_id: i32 = fields.get(2)?.parse().ok()?;
    let user_ticks: u64 = fields.get(11)?.parse().ok()?;
    let system_ticks: u64 = fields.get(12)?.parse().ok()?;
    Some(ProcStat {
        process_group_id,
        cpu_ticks: user_ticks.saturating_add(system_ticks),
    })
}

struct ProcStat {
    process_group_id: i32,
    cpu_ticks: u64,
}

/// 解析 `/proc/<pid>/io`：read_bytes + write_bytes。
fn parse_proc_io(io: &str) -> u64 {
    let mut bytes: u64 = 0;
    for line in io.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("read_bytes:") {
            if let Ok(value) = rest.trim().parse::<u64>() {
                bytes = bytes.saturating_add(value);
            }
        } else if let Some(rest) = trimmed.strip_prefix("write_bytes:") {
            if let Ok(value) = rest.trim().parse::<u64>() {
                bytes = bytes.saturating_add(value);
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_timeout_defaults_to_30m() {
        assert_eq!(DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS, 30 * 60 * 1000);
        assert_eq!(
            resolve_codex_inactivity_timeout(None),
            CodexOutputInactivityResolution::Default {
                timeout_ms: DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS,
            }
        );
        assert_eq!(
            resolve_codex_inactivity_timeout(Some(&json!("420000"))),
            CodexOutputInactivityResolution::Default {
                timeout_ms: DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS,
            }
        );
    }

    #[test]
    fn resolve_timeout_null_disables() {
        assert_eq!(
            resolve_codex_inactivity_timeout(Some(&serde_json::Value::Null)),
            CodexOutputInactivityResolution::Disabled {
                reason: "explicit_null",
            }
        );
    }

    #[test]
    fn resolve_timeout_configured_and_non_positive() {
        assert_eq!(
            resolve_codex_inactivity_timeout(Some(&json!(12_000))),
            CodexOutputInactivityResolution::Configured { timeout_ms: 12_000 }
        );
        assert_eq!(
            resolve_codex_inactivity_timeout(Some(&json!(0))),
            CodexOutputInactivityResolution::Default {
                timeout_ms: DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS,
            }
        );
        assert_eq!(
            resolve_codex_inactivity_timeout(Some(&json!(-100))),
            CodexOutputInactivityResolution::Default {
                timeout_ms: DEFAULT_CODEX_OUTPUT_INACTIVITY_TIMEOUT_MS,
            }
        );
    }

    #[test]
    fn format_error_message_minutes_seconds() {
        assert_eq!(
            format_output_inactivity_monitor_error_message(0),
            "monitor: no codex activity (output or process) for 0m 0s"
        );
        assert_eq!(
            format_output_inactivity_monitor_error_message(7 * 60 * 1000),
            "monitor: no codex activity (output or process) for 7m 0s"
        );
        assert_eq!(
            format_output_inactivity_monitor_error_message(7 * 60 * 1000 + 12_000),
            "monitor: no codex activity (output or process) for 7m 12s"
        );
        assert_eq!(
            format_output_inactivity_monitor_error_message(45_000),
            "monitor: no codex activity (output or process) for 0m 45s"
        );
    }

    #[test]
    fn monitor_fires_after_silence_and_counts_parsed_events() {
        let fires: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        let fires_for_closure = Arc::clone(&fires);
        let mut monitor = OutputInactivityMonitor::new(7 * 60 * 1000, 0, move |state| {
            fires_for_closure.lock().unwrap().push((
                state
                    .fired_at
                    .unwrap_or(0)
                    .saturating_sub(state.last_event_at),
                state.parsed_event_count,
            ));
        })
        .unwrap();

        monitor.note_output_chunk(
            "stdout",
            "{\"type\":\"thread.started\",\"thread_id\":\"abc\"}\n",
            50,
        );
        assert_eq!(monitor.state().parsed_event_count, 1);
        assert!(fires.lock().unwrap().is_empty());

        monitor.check_timeout(7 * 60 * 1000 + 49);
        assert!(fires.lock().unwrap().is_empty());
        monitor.check_timeout(7 * 60 * 1000 + 50);
        assert_eq!(fires.lock().unwrap().len(), 1);
        assert_eq!(fires.lock().unwrap()[0], (7 * 60 * 1000, 1));

        let final_state = monitor.stop();
        assert!(final_state.fired);
    }

    #[test]
    fn monitor_fires_only_once() {
        let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let fire_count_for_closure = Arc::clone(&fire_count);
        let mut monitor = OutputInactivityMonitor::new(1_000, 0, move |_| {
            *fire_count_for_closure.lock().unwrap() += 1;
        })
        .unwrap();
        monitor.note_output_chunk("stdout", "loading model...\n", 500);
        assert_eq!(monitor.state().output_chunk_count, 1);
        assert_eq!(monitor.state().parsed_event_count, 0);
        monitor.check_timeout(1_500);
        assert_eq!(*fire_count.lock().unwrap(), 1);
        // 再次检查不重复触发。
        monitor.check_timeout(3_000);
        assert_eq!(*fire_count.lock().unwrap(), 1);
        monitor.stop();
    }

    #[test]
    fn monitor_resets_on_process_activity() {
        let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let fire_count_for_closure = Arc::clone(&fire_count);
        let mut monitor = OutputInactivityMonitor::new(1_000, 0, move |_| {
            *fire_count_for_closure.lock().unwrap() += 1;
        })
        .unwrap();
        monitor.note_process_activity(900);
        assert_eq!(monitor.state().process_activity_count, 1);
        monitor.check_timeout(1_899);
        assert_eq!(*fire_count.lock().unwrap(), 0);
        monitor.check_timeout(1_900);
        assert_eq!(*fire_count.lock().unwrap(), 1);
        monitor.stop();
    }

    #[test]
    fn monitor_keeps_alive_on_non_json_output() {
        let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let fire_count_for_closure = Arc::clone(&fire_count);
        let timeout_ms = 7 * 60 * 1000;
        let mut monitor = OutputInactivityMonitor::new(timeout_ms, 0, move |_| {
            *fire_count_for_closure.lock().unwrap() += 1;
        })
        .unwrap();

        monitor.note_output_chunk(
            "stdout",
            "packages/server: typecheck passed\n",
            timeout_ms - 1_000,
        );
        monitor.note_output_chunk(
            "stderr",
            "packages/ui: build still running\n",
            (timeout_ms - 1_000) * 2,
        );
        monitor.note_output_chunk(
            "stdout",
            "packages/ui: build passed\n",
            (timeout_ms - 1_000) * 3,
        );

        assert_eq!(*fire_count.lock().unwrap(), 0);
        assert_eq!(monitor.state().output_chunk_count, 3);
        assert_eq!(monitor.state().parsed_event_count, 0);
        assert!(!monitor.state().fired);
        monitor.stop();
    }

    #[test]
    fn monitor_multiple_events_in_one_chunk() {
        let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let fire_count_for_closure = Arc::clone(&fire_count);
        let mut monitor = OutputInactivityMonitor::new(1_000, 0, move |_| {
            *fire_count_for_closure.lock().unwrap() += 1;
        })
        .unwrap();
        monitor.note_output_chunk(
            "stdout",
            "{\"type\":\"thread.started\",\"thread_id\":\"a\"}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hi\"}}\n",
            500,
        );
        assert_eq!(monitor.state().parsed_event_count, 2);
        monitor.check_timeout(1_499);
        assert_eq!(*fire_count.lock().unwrap(), 0);
        monitor.check_timeout(1_500);
        assert_eq!(*fire_count.lock().unwrap(), 1);
        monitor.stop();
    }

    #[test]
    fn monitor_requires_positive_timeout() {
        assert!(OutputInactivityMonitor::new(0, 0, |_| {}).is_err());
    }

    #[test]
    fn grace_period_is_5s() {
        assert_eq!(CODEX_OUTPUT_INACTIVITY_MONITOR_SIGTERM_GRACE_MS, 5_000);
    }

    #[test]
    fn parse_proc_stat_and_io() {
        // 模拟 /proc/123/stat：pid (comm) S ppid pgrp ... utime stime
        let stat = "123 (codex) S 1 100 0 -1 4194560 0 0 0 0 0 50 30 0 0 20 0 1 0 123 0";
        let parsed = parse_proc_stat(stat).unwrap();
        assert_eq!(parsed.process_group_id, 100);
        assert_eq!(parsed.cpu_ticks, 80);

        let io = "rchar: 100\nread_bytes: 4096\nwrite_bytes: 8192\n";
        assert_eq!(parse_proc_io(io), 12_288);
    }
}
