//! Service 实现 —— IssueRewakeThrottleService。
//!
//! 纯函数 service（无 DB I/O），封装三个公开函数 + Hook。

use std::sync::Arc;

use super::hook::{IssueRewakeThrottleHook, NoopIssueRewakeThrottleHook};
use super::types::{
    IssueRewakeCandidateInput, IssueRewakeThrottleDecision, IssueRewakeThrottleInput,
    ISSUE_REWAKE_BASE_COOLDOWN_MS, ISSUE_REWAKE_MAX_COOLDOWN_MS,
    ISSUE_REWAKE_NO_PROGRESS_THRESHOLD, THROTTLED_ISSUE_REWAKE_REASONS,
};

/// 顶层公开函数：判定 wake 是否为 throttle 候选（与 Node `isThrottleCandidateIssueRewake` 1:1 对齐）。
///
/// 逻辑：
/// - `forceFreshSession=true` → false（pass）
/// - `wakeCommentId` 非空 → false（pass）
/// - `hasExplicitResume=true` → false（pass）
/// - `reason == null` → true（候选）
/// - `reason` 在 THROTTLED_ISSUE_REWAKE_REASONS 中 → true（候选）
/// - 其他 → false（pass）
pub fn is_throttle_candidate_issue_rewake(input: &IssueRewakeCandidateInput) -> bool {
    if input.force_fresh_session {
        return false;
    }
    if input.wake_comment_id.is_some() {
        return false;
    }
    if input.has_explicit_resume {
        return false;
    }
    match &input.reason {
        None => true,
        Some(r) => THROTTLED_ISSUE_REWAKE_REASONS.contains(&r.as_str()),
    }
}

/// 顶层公开函数：计算 cooldown 毫秒数（与 Node `computeIssueRewakeCooldownMs` 1:1 对齐）。
///
/// 公式：
/// - doublings = max(0, streak - threshold)
/// - factor = 2^doublings（capped at 2^16 避免溢出）
/// - cooldown = min(base * factor, max_cooldown)
pub fn compute_issue_rewake_cooldown_ms(no_progress_streak: usize) -> u64 {
    let threshold = ISSUE_REWAKE_NO_PROGRESS_THRESHOLD;
    let doublings = no_progress_streak.saturating_sub(threshold);
    let safe_doublings = doublings.min(16); // guard against overflow
    let factor: u64 = 1u64 << safe_doublings;
    let base = ISSUE_REWAKE_BASE_COOLDOWN_MS.saturating_mul(factor);
    base.min(ISSUE_REWAKE_MAX_COOLDOWN_MS)
}

/// 顶层公开函数：主 throttle 决策（与 Node `evaluateIssueRewakeThrottle` 1:1 对齐）。
pub fn evaluate_issue_rewake_throttle(
    input: &IssueRewakeThrottleInput,
) -> IssueRewakeThrottleDecision {
    let runs = &input.recent_terminal_runs;
    if runs.is_empty() {
        return IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0,
        };
    }
    if input.has_new_issue_input_since_last_run {
        return IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0,
        };
    }

    let mut no_progress_streak = 0usize;
    for run in runs {
        // failed/cancelled/interrupted → break streak
        if run.status != "succeeded" {
            break;
        }
        if run.finished_at.is_none() {
            break;
        }
        if input.run_ids_with_issue_progress.contains(&run.id) {
            break;
        }
        no_progress_streak += 1;
    }

    if no_progress_streak < ISSUE_REWAKE_NO_PROGRESS_THRESHOLD {
        return IssueRewakeThrottleDecision::Allowed { no_progress_streak };
    }

    let last_run_finished_at = match runs[0].finished_at {
        Some(t) => t,
        None => {
            return IssueRewakeThrottleDecision::Allowed { no_progress_streak };
        }
    };

    let cooldown_ms = compute_issue_rewake_cooldown_ms(no_progress_streak);
    let next_allowed_at = last_run_finished_at + chrono::Duration::milliseconds(cooldown_ms as i64);

    if input.now < next_allowed_at {
        IssueRewakeThrottleDecision::Blocked {
            no_progress_streak,
            cooldown_ms,
            last_run_finished_at,
            next_allowed_at,
        }
    } else {
        IssueRewakeThrottleDecision::Allowed { no_progress_streak }
    }
}

/// Issue rewake throttle service —— 封装 + Hook。
pub struct IssueRewakeThrottleService {
    hook: Arc<dyn IssueRewakeThrottleHook>,
}

impl std::fmt::Debug for IssueRewakeThrottleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueRewakeThrottleService").finish()
    }
}

impl Default for IssueRewakeThrottleService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueRewakeThrottleService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueRewakeThrottleHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn IssueRewakeThrottleHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueRewakeThrottleHook> {
        self.hook.clone()
    }

    /// 判定 wake 是否为 throttle 候选（hook 集成）。
    pub fn is_candidate(&self, candidate: &IssueRewakeCandidateInput) -> bool {
        self.hook.before_evaluate(candidate);
        let result = is_throttle_candidate_issue_rewake(candidate);
        if !result {
            self.hook.on_not_candidate(&candidate.reason);
        }
        result
    }

    /// 主 throttle 决策（hook 集成）。
    pub fn evaluate(&self, input: &IssueRewakeThrottleInput) -> IssueRewakeThrottleDecision {
        let decision = evaluate_issue_rewake_throttle(input);
        match &decision {
            IssueRewakeThrottleDecision::Allowed { no_progress_streak } => {
                self.hook.after_allowed(*no_progress_streak);
            }
            IssueRewakeThrottleDecision::Blocked {
                no_progress_streak,
                cooldown_ms,
                ..
            } => {
                self.hook.after_blocked(*no_progress_streak, *cooldown_ms);
            }
        }
        decision
    }
}
