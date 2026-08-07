//! Subprocess signal dispatch — port of Node `signalRunningProcess`
//! from `packages/adapter-utils/src/server-utils.ts` (L82-112).
//!
//! Mirrors the runtime behavior:
//! 1. On non-Windows, if `process_group_id` is set and positive, signal
//!    the entire process group first via `kill -<signum> -<pgid>`.
//! 2. If the group signal fails (or no group), fall back to the direct
//!    child signal `kill -<signum> <pid>`.
//! 3. Skip the signal entirely when the caller asserts the child is
//!    already terminated (so we do not suppress a follow-up SIGKILL).
//!
//! We shell out to `/bin/sh -c 'kill ...; echo $?'` so the crate stays
//! `unsafe_code = "forbid"`-clean (the workspace `unsafe_code = "forbid"`
//! policy forbids `libc::kill`). The semantics are equivalent to
//! `kill(pid, signum)` for a permission probe / signal dispatch from
//! the calling process group.
//!
//! Pure helpers — no I/O state, no async, no global state. Designed for
//! high cohesion; callers opt in to the helpers they need.

use std::fmt;
use std::io::Read;
use std::process::Command;

// ============================================================================
// Signal enum
// ============================================================================

/// A subset of POSIX signals that the runtime actually dispatches. Mirrors
/// the subset Node `signalRunningProcess` exposes via `NodeJS.Signals`.
/// Add new variants here as the runtime grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    SIGHUP,
    SIGINT,
    SIGQUIT,
    SIGTERM,
    SIGKILL,
    SIGUSR1,
    SIGUSR2,
}

impl Signal {
    /// Numeric signum (POSIX) for use with `/bin/sh -c 'kill -<n>'`.
    pub const fn signum(self) -> i32 {
        match self {
            Signal::SIGHUP => 1,
            Signal::SIGINT => 2,
            Signal::SIGQUIT => 3,
            Signal::SIGTERM => 15,
            Signal::SIGKILL => 9,
            Signal::SIGUSR1 => 10,
            Signal::SIGUSR2 => 12,
        }
    }

    /// ASCII name used by `kill -<name>`. Matches Node's `NodeJS.Signals`
    /// string form so logs read consistently.
    pub const fn as_str(self) -> &'static str {
        match self {
            Signal::SIGHUP => "SIGHUP",
            Signal::SIGINT => "SIGINT",
            Signal::SIGQUIT => "SIGQUIT",
            Signal::SIGTERM => "SIGTERM",
            Signal::SIGKILL => "SIGKILL",
            Signal::SIGUSR1 => "SIGUSR1",
            Signal::SIGUSR2 => "SIGUSR2",
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// SignalOutcome
// ============================================================================

/// Outcome of a [`signal_running_process`] call. Mirrors Node's "did
/// group signal succeed? fall back to direct signal? skip because the
/// child already exited?" decision tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalOutcome {
    /// Group signal sent; carries the pgid actually signaled.
    GroupSent(u32),
    /// Direct signal sent to the child PID; carries the child pid.
    DirectSent(u32),
    /// Caller asserted the child has already exited — no signal sent.
    SkippedAlreadyExited,
    /// Both group and direct signal attempts failed.
    Failed { reason: String },
}

// ============================================================================
// signal_running_process
// ============================================================================

/// Options for [`signal_running_process`]. Mirrors the Node function
/// parameters (`child` + `processGroupId` + `signal`), but expressed in
/// terms of raw pid values to keep this module independent of the
/// tokio-based `SubprocessHandle` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalRunningProcessInput {
    /// Direct child PID (always required).
    pub child_pid: u32,
    /// POSIX process group id, if the child was started in its own group
    /// (`setpgid(0, pid)`). `None` means "no separate group, signal the
    /// direct child only".
    pub process_group_id: Option<u32>,
    /// The signal to deliver.
    pub signal: Signal,
    /// Optional override: when `true`, skip signal dispatch entirely. This
    /// is the Rust equivalent of Node's `exitCode === null &&
    /// signalCode === null` gate — callers that have observed the child
    /// has already exited set this to avoid suppressing a follow-up
    /// SIGKILL. Defaults to `false`.
    pub child_already_exited: bool,
}

impl SignalRunningProcessInput {
    pub fn new(child_pid: u32, process_group_id: Option<u32>, signal: Signal) -> Self {
        Self {
            child_pid,
            process_group_id,
            signal,
            child_already_exited: false,
        }
    }
}

/// Dispatch a signal to a running subprocess. Mirrors Node
/// `signalRunningProcess` (server-utils.ts L82-112). Pure (no I/O
/// side-effects beyond the actual `kill` syscall); tests can drive it
/// against any real `pid`.
pub fn signal_running_process(input: SignalRunningProcessInput) -> SignalOutcome {
    if input.child_already_exited {
        return SignalOutcome::SkippedAlreadyExited;
    }

    // Unix-only. The acpx engine never runs on Windows, so we treat
    // non-Unix as a hard failure.
    #[cfg(unix)]
    {
        if let Some(pgid) = input.process_group_id {
            if pgid > 0 {
                let neg_pgid = -(pgid as i64);
                match dispatch_signal_i64(Some(neg_pgid), input.signal) {
                    Ok(()) => return SignalOutcome::GroupSent(pgid),
                    Err(_) => {
                        // Fall through to direct child signal.
                    }
                }
            }
        }
        match dispatch_signal_i64(Some(input.child_pid as i64), input.signal) {
            Ok(()) => SignalOutcome::DirectSent(input.child_pid),
            Err(reason) => SignalOutcome::Failed { reason },
        }
    }
    #[cfg(not(unix))]
    {
        let _ = input;
        SignalOutcome::Failed {
            reason: "signal_running_process is only supported on Unix".to_string(),
        }
    }
}

#[cfg(unix)]
fn dispatch_signal_i64(target: Option<i64>, signal: Signal) -> Result<(), String> {
    let target_str = match target {
        Some(pid) => {
            if pid < 0 {
                format!("-{}", -pid)
            } else {
                pid.to_string()
            }
        }
        None => "0".to_string(),
    };
    let cmd = format!(
        "kill -{} {} 2>/dev/null; echo $?",
        signal.signum(),
        target_str
    );
    let output = Command::new("sh").arg("-c").arg(&cmd).output();
    match output {
        Ok(out) => {
            let mut s = String::new();
            out.stdout.as_slice().read_to_string(&mut s).ok();
            if s.trim() == "0" {
                Ok(())
            } else {
                Err(format!(
                    "kill {} {} exited {}",
                    target_str,
                    signal,
                    s.trim()
                ))
            }
        }
        Err(e) => Err(format!("spawn sh: {}", e)),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_signum_matches_posix_values() {
        assert_eq!(Signal::SIGHUP.signum(), 1);
        assert_eq!(Signal::SIGINT.signum(), 2);
        assert_eq!(Signal::SIGQUIT.signum(), 3);
        assert_eq!(Signal::SIGKILL.signum(), 9);
        assert_eq!(Signal::SIGUSR1.signum(), 10);
        assert_eq!(Signal::SIGUSR2.signum(), 12);
        assert_eq!(Signal::SIGTERM.signum(), 15);
    }

    #[test]
    fn signal_as_str_matches_node_signals() {
        assert_eq!(Signal::SIGTERM.as_str(), "SIGTERM");
        assert_eq!(Signal::SIGKILL.as_str(), "SIGKILL");
        assert_eq!(Signal::SIGINT.as_str(), "SIGINT");
        assert_eq!(Signal::SIGHUP.as_str(), "SIGHUP");
        assert_eq!(Signal::SIGQUIT.as_str(), "SIGQUIT");
    }

    #[test]
    fn signal_outcome_skipped_when_child_already_exited() {
        let mut input = SignalRunningProcessInput::new(
            std::process::id(),
            Some(std::process::id()),
            Signal::SIGTERM,
        );
        input.child_already_exited = true;
        let out = signal_running_process(input);
        assert_eq!(out, SignalOutcome::SkippedAlreadyExited);
    }

    #[cfg(unix)]
    #[test]
    fn signal_outcome_skipped_for_self_pid_when_already_exited() {
        // Use the current test process — `kill -TERM` would kill the
        // test runner, so we set `child_already_exited = true` to force
        // the no-op branch.
        let mut input = SignalRunningProcessInput::new(std::process::id(), None, Signal::SIGTERM);
        input.child_already_exited = true;
        let out = signal_running_process(input);
        assert_eq!(out, SignalOutcome::SkippedAlreadyExited);
    }

    #[cfg(unix)]
    #[test]
    fn signal_running_process_dispatches_to_unlikely_high_pid() {
        // PID `0x7FFFFFFE` is reserved (Linux "no such process"). The
        // dispatcher must not panic; it returns `Failed` because both
        // the group and direct attempts find no such pid.
        let input = SignalRunningProcessInput::new(0x7FFFFFFE_u32, None, Signal::SIGTERM);
        let out = signal_running_process(input);
        assert!(matches!(out, SignalOutcome::Failed { .. }), "got {:?}", out);
    }

    #[cfg(unix)]
    #[test]
    fn signal_running_process_group_signal_with_zero_pgid_skipped() {
        // When pgid is `Some(0)`, the group-signal branch must be
        // skipped (`pgid > 0` gate). Direct signal path then runs against
        // the unlikely high pid, returning Failed without ever touching
        // the test runner. This avoids the SIGKILL-the-runner trap that
        // the older integration-style test hit.
        let input = SignalRunningProcessInput {
            child_pid: 0x7FFFFFFE_u32,
            process_group_id: Some(0),
            signal: Signal::SIGTERM,
            child_already_exited: false,
        };
        let out = signal_running_process(input);
        // The dispatch path goes direct (because pgid==0 is skipped) and
        // fails because the unlikely high pid has no such process.
        assert!(
            matches!(out, SignalOutcome::Failed { .. }),
            "expected Failed for unlikely pid, got {:?}",
            out
        );
    }

    #[cfg(unix)]
    #[test]
    fn signal_running_process_returns_failed_for_unlikely_high_pid() {
        // PID 0x7FFFFFFE is reserved (Linux "no such process"). The
        // dispatcher returns `Failed` because the direct kill exits
        // non-zero. Crucially, this does NOT target the test runner's
        // pid, so the test process is safe.
        let input = SignalRunningProcessInput {
            child_pid: 0x7FFFFFFE_u32,
            process_group_id: None,
            signal: Signal::SIGTERM,
            child_already_exited: false,
        };
        let out = signal_running_process(input);
        assert!(matches!(out, SignalOutcome::Failed { .. }), "got {:?}", out);
    }

    #[cfg(unix)]
    #[test]
    fn signal_running_process_skipped_when_child_already_exited_for_self() {
        // Same as `signal_outcome_skipped_when_child_already_exited`,
        // but using the test process pid. The dispatcher must short
        // circuit before reaching `kill`, so this is safe.
        let mut input = SignalRunningProcessInput::new(std::process::id(), None, Signal::SIGTERM);
        input.child_already_exited = true;
        let out = signal_running_process(input);
        assert_eq!(out, SignalOutcome::SkippedAlreadyExited);
    }
}
