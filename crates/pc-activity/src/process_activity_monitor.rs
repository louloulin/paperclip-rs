//! Process activity monitor（对齐 Node process-activity-monitor.ts）。
//!
//! 周期（默认 15s）采样目标进程及其进程组的 CPU ticks + IO 字节数；
//! 当变化超过阈值时调用 `on_activity`。用于在子进程"看似卡住但没
//! 触发 output inactivity monitor"时提供兜底（paperclip 的"silent
//! run safety net"）。
//!
//! 仅 Linux 平台有效：依赖 `/proc/<pid>/stat` + `/proc/<pid>/io`。
//! 其他平台 `sample_process_activity` 直接返回 `None`，monitor 永
//! 不触发 `on_activity`。

use std::path::Path;
use std::time::Duration;

/// 默认采样间隔：15 秒（与 Node `CODEX_PROCESS_ACTIVITY_POLL_INTERVAL_MS` 一致）。
pub const DEFAULT_PROCESS_ACTIVITY_POLL_INTERVAL_MS: u64 = 15_000;

/// 单次采样的快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessActivitySnapshot {
    /// 用户态 + 系统态 CPU ticks 累计值。
    pub cpu_ticks: u64,
    /// `read_bytes` + `write_bytes` 累计值。
    pub io_bytes: u64,
    /// 进程组成员 PID 列表（排序后逗号拼接）。
    pub process_ids: String,
}

/// 平台相关：`Some` 当快照可读，`None` 当不可读（非 Linux / 进程已退）。
pub type SampleFn = std::pin::Pin<
    Box<
        dyn Fn() -> futures_core::future::BoxFuture<'static, Option<ProcessActivitySnapshot>>
            + Send
            + Sync,
    >,
>;

/// 监控选项。
pub struct ProcessActivityMonitorOptions {
    pub pid: u32,
    pub process_group_id: Option<u32>,
    pub on_activity: Box<dyn Fn() + Send + Sync>,
    /// 采样间隔，默认 15s。
    pub interval: Option<Duration>,
    /// 测试时注入采样函数。
    pub sample: Option<SampleFn>,
}

/// 监控句柄：`stop()` 终止轮询循环。
pub struct ProcessActivityMonitorHandle {
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl ProcessActivityMonitorHandle {
    pub fn stop(mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            handle.abort();
        }
    }
}

/// 解析 `/proc/<pid>/stat`：command 字段可能含空格/括号，所以从
/// 最后一个 `)` 切片。对齐 Node `parseProcStat`。
fn parse_proc_stat(stat: &str) -> Option<(u32, u64)> {
    let command_end = stat.rfind(')')?;
    let fields = stat[command_end + 2..]
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.len() < 13 {
        return None;
    }
    let pgid: u32 = fields.get(2)?.parse().ok()?;
    let user_ticks: u64 = fields.get(11)?.parse().ok()?;
    let system_ticks: u64 = fields.get(12)?.parse().ok()?;
    Some((pgid, user_ticks + system_ticks))
}

/// 解析 `/proc/<pid>/io`：累加 `read_bytes` + `write_bytes`。
fn parse_proc_io(io: &str) -> u64 {
    let mut bytes = 0u64;
    for line in io.lines() {
        let line = line.trim();
        let after = line
            .strip_prefix("read_bytes:")
            .or_else(|| line.strip_prefix("write_bytes:"));
        if let Some(rest) = after {
            if let Ok(v) = rest.trim().parse::<u64>() {
                bytes = bytes.saturating_add(v);
            }
        }
    }
    bytes
}

/// 读取 /proc/<pid>/{stat,io}，聚合 CPU + IO。Linux only。
async fn read_proc_entry(pid: u32) -> Option<(u32, u64, u64)> {
    let stat = match tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await {
        Ok(s) => s,
        Err(_) => return None,
    };
    let (pgid, cpu_ticks) = parse_proc_stat(&stat)?;
    // io 文件可能在容器/受限命名空间中不存在（permission denied）
    let io = tokio::fs::read_to_string(format!("/proc/{pid}/io"))
        .await
        .unwrap_or_default();
    Some((pgid, cpu_ticks, parse_proc_io(&io)))
}

/// 同步采样整个进程组（或单一进程）的活动快照。非 Linux 直接返回 None。
pub async fn sample_process_activity(
    pid: u32,
    process_group_id: Option<u32>,
) -> Option<ProcessActivitySnapshot> {
    if !Path::new("/proc").exists() {
        return None;
    }
    let target_pgid = process_group_id.filter(|p| *p > 0);
    let entries: Vec<String> = if let Some(pgid) = target_pgid {
        match tokio::fs::read_dir("/proc").await {
            Ok(mut dir) => {
                let mut out = Vec::new();
                while let Ok(Some(entry)) = dir.next_entry().await {
                    if let Ok(name) = entry.file_name().into_string() {
                        out.push(name);
                    }
                }
                out
            }
            Err(_) => return None,
        }
    } else {
        vec![pid.to_string()]
    };
    let mut cpu_ticks = 0u64;
    let mut io_bytes = 0u64;
    let mut process_ids = Vec::new();
    for entry in entries {
        if !entry.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let entry_pid: u32 = match entry.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        match read_proc_entry(entry_pid).await {
            Some((pgid, ticks, io)) => {
                if let Some(target) = target_pgid {
                    if pgid != target {
                        continue;
                    }
                } else if entry_pid != pid {
                    continue;
                }
                cpu_ticks = cpu_ticks.saturating_add(ticks);
                io_bytes = io_bytes.saturating_add(io);
                process_ids.push(entry_pid);
            }
            None => {
                // 进程可能在 readdir 与 read 之间退出 → 静默忽略。
            }
        }
    }
    if process_ids.is_empty() {
        return None;
    }
    process_ids.sort_unstable();
    Some(ProcessActivitySnapshot {
        cpu_ticks,
        io_bytes,
        process_ids: process_ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    })
}

/// 启动一个 process activity monitor（tokio task）。返回句柄用于停止。
pub fn spawn_process_activity_monitor(
    options: ProcessActivityMonitorOptions,
) -> ProcessActivityMonitorHandle {
    let interval = options.interval.unwrap_or(Duration::from_millis(
        DEFAULT_PROCESS_ACTIVITY_POLL_INTERVAL_MS,
    ));
    let minimum_cpu_tick_delta = std::cmp::max(1, (interval.as_millis() / 1000) as u64);
    let on_activity = options.on_activity;
    let sample: SampleFn = options.sample.unwrap_or_else(|| {
        let pid = options.pid;
        let pgid = options.process_group_id;
        Box::pin(move || Box::pin(sample_process_activity(pid, pgid)))
    });
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped_clone = std::sync::Arc::clone(&stopped);
    let join = tokio::spawn(async move {
        let mut previous: Option<ProcessActivitySnapshot> = None;
        while !stopped_clone.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(interval).await;
            if stopped_clone.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let current = sample().await;
            if stopped_clone.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            if let (Some(cur), Some(prev)) = (current.as_ref(), previous.as_ref()) {
                let cpu_increased =
                    cur.cpu_ticks.saturating_sub(prev.cpu_ticks) >= minimum_cpu_tick_delta;
                let io_increased = cur.io_bytes > prev.io_bytes;
                let members_changed = cur.process_ids != prev.process_ids;
                if cpu_increased || io_increased || members_changed {
                    on_activity();
                }
            }
            previous = current;
        }
    });
    ProcessActivityMonitorHandle {
        stopped,
        join: Some(join),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    #[test]
    fn parse_proc_stat_extracts_pgid_and_cpu_ticks() {
        // 构造一个含 13 个字段的 /proc/<pid>/stat 字符串：
        // 字段 0: state        = S
        // 字段 1: ppid         = 1234
        // 字段 2: pgrp         = 1234
        // 字段 11: utime (ticks)= 100
        // 字段 12: stime (ticks)= 200
        let fields_after_paren = "S 1234 1234 1234 0 -1 4194304 0 0 0 0 100 200";
        let stat = format!("1234 (codex) {fields_after_paren}");
        let (pgid, ticks) = parse_proc_stat(&stat).expect("parse");
        assert_eq!(pgid, 1234);
        // ticks = user_ticks(100) + system_ticks(200) = 300
        assert_eq!(ticks, 300);
    }

    #[test]
    fn parse_proc_stat_returns_none_without_paren() {
        assert!(parse_proc_stat("nope no paren at all").is_none());
    }

    #[test]
    fn parse_proc_io_sums_read_and_write_bytes() {
        let io = "rchar: 100
wchar: 200
read_bytes: 1024
write_bytes: 2048
";
        assert_eq!(parse_proc_io(io), 3072);
    }

    #[test]
    fn parse_proc_io_handles_empty_and_garbage() {
        assert_eq!(parse_proc_io(""), 0);
        assert_eq!(
            parse_proc_io(
                "foo: bar
baz: qux"
            ),
            0
        );
    }

    #[test]
    fn default_interval_is_15_seconds() {
        assert_eq!(DEFAULT_PROCESS_ACTIVITY_POLL_INTERVAL_MS, 15_000);
    }

    #[tokio::test]
    async fn monitor_invocates_on_activity_when_cpu_ticks_increase() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let tick = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tick_clone = Arc::clone(&tick);
        let sample: SampleFn = Box::pin(move || {
            let next = tick_clone.fetch_add(100, std::sync::atomic::Ordering::SeqCst) + 100;
            let snapshot = ProcessActivitySnapshot {
                cpu_ticks: next,
                io_bytes: 0,
                process_ids: "1".into(),
            };
            Box::pin(async move { Some(snapshot) })
        });
        let handle = spawn_process_activity_monitor(ProcessActivityMonitorOptions {
            pid: 1,
            process_group_id: None,
            on_activity: Box::new(move || {
                counter_clone.fetch_add(1, AtomicOrdering::SeqCst);
            }),
            interval: Some(Duration::from_millis(50)),
            sample: Some(sample),
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.stop();
        // 至少触发一次（CPU ticks 每次增 100，阈值 = max(1, 50/1000) = 1）
        assert!(counter.load(AtomicOrdering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn monitor_does_not_invoke_when_snapshot_unchanged() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let sample: SampleFn = Box::pin(|| {
            let snapshot = ProcessActivitySnapshot {
                cpu_ticks: 100,
                io_bytes: 0,
                process_ids: "1".into(),
            };
            Box::pin(async move { Some(snapshot) })
        });
        let handle = spawn_process_activity_monitor(ProcessActivityMonitorOptions {
            pid: 1,
            process_group_id: None,
            on_activity: Box::new(move || {
                counter_clone.fetch_add(1, AtomicOrdering::SeqCst);
            }),
            interval: Some(Duration::from_millis(50)),
            sample: Some(sample),
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.stop();
        // 快照不变 → 不触发
        assert_eq!(counter.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn monitor_stops_when_handle_dropped_via_stop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let sample: SampleFn = Box::pin(|| Box::pin(async { None }));
        let handle = spawn_process_activity_monitor(ProcessActivityMonitorOptions {
            pid: 1,
            process_group_id: None,
            on_activity: Box::new(move || {
                counter_clone.fetch_add(1, AtomicOrdering::SeqCst);
            }),
            interval: Some(Duration::from_millis(20)),
            sample: Some(sample),
        });
        // 立刻停止
        handle.stop();
        tokio::time::sleep(Duration::from_millis(80)).await;
        // 不会触发任何回调
        assert_eq!(counter.load(AtomicOrdering::SeqCst), 0);
    }
}
