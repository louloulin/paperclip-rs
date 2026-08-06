//! Run liveness 续跑决策（纯函数部分）
//!
//! 对齐 Node `services/recovery/run-liveness-continuations.ts`：
//! - 常量 `RUN_LIVENESS_CONTINUATION_REASON` / `DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS = 2`
//! - 类型 `RunContinuationDecision`（`enqueue` / `exhausted` / `skip` 三态）
//! - 函数 `read_continuation_attempt(value)` —— 从任意值规整为非负整数
//! - 函数 `build_run_liveness_continuation_idempotency_key(input)` —— 拼接 idempotency key
//! - 函数 `decide_run_liveness_continuation(input)` —— 核心决策：何时入队、何时耗尽、何时跳过
//!
//! 设计：
//! - 纯函数无副作用（除 IO 边界），便于单测
//! - 与 DB 调用解耦：`findExistingRunLivenessContinuationWake` 留待调用方集成
//! - 决策顺序按 Node 1:1 复刻：liveness_state → issue → agent → 公司域匹配 → 分配匹配 →
//!   issue.status → executionState → agent.status → budgetBlocked → 尝试上限 → idempotent

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::origins::recovery_reason_kinds;

// ============================================================================
// Constants
// ============================================================================

/// Run liveness 续跑的 wake reason，对齐 Node `RUN_LIVENESS_CONTINUATION_REASON`。
pub const RUN_LIVENESS_CONTINUATION_REASON: &str = recovery_reason_kinds::RUN_LIVENESS_CONTINUATION;

/// 默认最大续跑尝试次数（2）。
pub const DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS: u32 = 2;

/// 触发续跑的 liveness 状态集合（`plan_only` / `empty_response`）。
pub const ACTIONABLE_LIVENESS_STATES: &[&str] = &["plan_only", "empty_response"];

/// Issue 必须处于的活跃状态（`todo` / `in_progress`）。
pub const CONTINUATION_ACTIVE_ISSUE_STATUSES: &[&str] = &["todo", "in_progress"];

/// Agent 允许被续跑的状态集合（含 error，因为前次错误不应永久抑制 bounded 续跑）。
pub const CONTINUATION_AGENT_STATUSES: &[&str] = &["active", "idle", "running", "error"];

/// Idempotent wake 视为已存在的状态集合。
pub const IDEMPOTENT_WAKE_STATUSES: &[&str] = &["queued", "deferred_issue_execution", "completed"];

// ============================================================================
// Inputs / Decision
// ============================================================================

/// HeartbeatRun 行（决策所需的最小子集）。
#[derive(Debug, Clone)]
pub struct HeartbeatRunRef {
    pub id: String,
    pub company_id: String,
    pub agent_id: String,
    pub continuation_attempt: Option<i64>,
}

/// Issue 行（决策所需的最小子集）。
#[derive(Debug, Clone)]
pub struct IssueRef {
    pub id: String,
    pub company_id: String,
    pub status: String,
    pub assignee_agent_id: Option<String>,
    pub execution_state: Option<String>,
}

/// Agent 行（决策所需的最小子集）。
#[derive(Debug, Clone)]
pub struct AgentRef {
    pub id: String,
    pub company_id: String,
    pub status: String,
}

/// `decide_run_liveness_continuation` 输入。
#[derive(Debug, Clone)]
pub struct DecideRunLivenessContinuationInput {
    pub run: HeartbeatRunRef,
    pub issue: Option<IssueRef>,
    pub agent: Option<AgentRef>,
    pub liveness_state: Option<String>,
    pub liveness_reason: Option<String>,
    pub next_action: Option<String>,
    pub budget_blocked: bool,
    pub idempotent_wake_exists: bool,
    pub max_attempts: Option<u32>,
}

/// 续跑决策结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunContinuationDecision {
    Enqueue {
        next_attempt: u32,
        idempotency_key: String,
        instruction: String,
    },
    Exhausted {
        attempt: u32,
        max_attempts: u32,
        comment: String,
    },
    Skip {
        reason: String,
    },
}

impl RunContinuationDecision {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Enqueue { .. } => "enqueue",
            Self::Exhausted { .. } => "exhausted",
            Self::Skip { .. } => "skip",
        }
    }
}

/// Idempotency key 输入。
#[derive(Debug, Clone)]
pub struct IdempotencyKeyInput {
    pub issue_id: String,
    pub source_run_id: String,
    pub liveness_state: String,
    pub next_attempt: u32,
}

// ============================================================================
// Public API
// ============================================================================

/// 从任意值规整为非负整数尝试次数。
///
/// 对齐 Node `readContinuationAttempt`：
/// - 数字 → 取 `floor`（仅当 > 0 且有限）
/// - 字符串 → `parseInt` 后同样处理
/// - 其他 / NaN / ≤ 0 → 0
pub fn read_continuation_attempt(value: Option<&dyn std::fmt::Display>) -> u32 {
    let s = match value {
        Some(v) => v.to_string(),
        None => return 0,
    };
    // Try parse as integer (handles both numeric strings and numeric types)
    let parsed = if let Ok(n) = s.parse::<i64>() {
        n
    } else {
        return 0;
    };
    if parsed > 0 {
        parsed as u32
    } else {
        0
    }
}

/// 构建 idempotency key。
///
/// 格式：`{reason}:{issueId}:{sourceRunId}:{livenessState}:{nextAttempt}`
pub fn build_run_liveness_continuation_idempotency_key(input: &IdempotencyKeyInput) -> String {
    [
        RUN_LIVENESS_CONTINUATION_REASON,
        &input.issue_id,
        &input.source_run_id,
        &input.liveness_state,
        &input.next_attempt.to_string(),
    ]
    .join(":")
}

/// 决定 run liveness 是否续跑。
///
/// 决策顺序（与 Node 一致）：
/// 1. liveness_state 必须是 `plan_only` 或 `empty_response`
/// 2. issue / agent 必须存在
/// 3. 三者（run / issue / agent）公司域必须匹配
/// 4. issue.assignee_agent_id 必须等于 run.agent_id
/// 5. issue.status 必须是 `todo` 或 `in_progress`
/// 6. issue.execution_state 必须为空（被 execution policy 阻塞则跳过）
/// 7. agent.status 必须在 `CONTINUATION_AGENT_STATUSES` 内
/// 8. budget_blocked 必须为 false
/// 9. current_attempt < max_attempts，否则 exhausted
/// 10. idempotent_wake_exists → skip
/// 11. 否则 → enqueue
pub fn decide_run_liveness_continuation(
    input: &DecideRunLivenessContinuationInput,
) -> RunContinuationDecision {
    let actionable: HashSet<&str> = ACTIONABLE_LIVENESS_STATES.iter().copied().collect();
    let active_issue: HashSet<&str> = CONTINUATION_ACTIVE_ISSUE_STATUSES.iter().copied().collect();
    let active_agent: HashSet<&str> = CONTINUATION_AGENT_STATUSES.iter().copied().collect();
    let max_attempts = input
        .max_attempts
        .unwrap_or(DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS);

    // 1. liveness_state actionable
    let liveness_state = match &input.liveness_state {
        Some(s) if actionable.contains(s.as_str()) => s.clone(),
        _ => {
            return RunContinuationDecision::Skip {
                reason: "liveness state is not actionable for continuation".to_string(),
            };
        }
    };

    // 2. issue + agent 存在
    let issue = match &input.issue {
        Some(i) => i,
        None => {
            return RunContinuationDecision::Skip {
                reason: "issue not found".to_string(),
            };
        }
    };
    let agent = match &input.agent {
        Some(a) => a,
        None => {
            return RunContinuationDecision::Skip {
                reason: "agent not found".to_string(),
            };
        }
    };

    // 3. company scope 一致
    if issue.company_id != input.run.company_id || agent.company_id != input.run.company_id {
        return RunContinuationDecision::Skip {
            reason: "company scope mismatch".to_string(),
        };
    }

    // 4. assignee 匹配
    if issue.assignee_agent_id.as_deref() != Some(input.run.agent_id.as_str()) {
        return RunContinuationDecision::Skip {
            reason: "issue is no longer assigned to the source run agent".to_string(),
        };
    }

    // 5. issue.status active
    if !active_issue.contains(issue.status.as_str()) {
        return RunContinuationDecision::Skip {
            reason: format!("issue status {} is not continuable", issue.status),
        };
    }

    // 6. issue.execution_state 必须为空
    if let Some(exec_state) = &issue.execution_state {
        if !exec_state.is_empty() {
            return RunContinuationDecision::Skip {
                reason: "issue is blocked by execution policy state".to_string(),
            };
        }
    }

    // 7. agent.status invokable
    if !active_agent.contains(agent.status.as_str()) {
        return RunContinuationDecision::Skip {
            reason: format!("agent status {} is not invokable", agent.status),
        };
    }

    // 8. budget blocked
    if input.budget_blocked {
        return RunContinuationDecision::Skip {
            reason: "budget hard stop blocks continuation".to_string(),
        };
    }

    // 9. attempts 上限
    let current_attempt = read_continuation_attempt(
        input
            .run
            .continuation_attempt
            .as_ref()
            .map(|n| n as &dyn std::fmt::Display),
    );
    if current_attempt >= max_attempts {
        return RunContinuationDecision::Exhausted {
            attempt: current_attempt,
            max_attempts,
            comment: format!(
                "Bounded liveness continuation exhausted\n\n\
                 - Last liveness state: `{liveness_state}`\n\
                 - Attempts used: {current_attempt}/{max_attempts}\n\
                 - Reason: {reason}\n\
                 - Next action: a human or manager should inspect the run and either clarify the task, mark it blocked, or assign a concrete follow-up.",
                reason = input.liveness_reason.as_deref()
                    .unwrap_or("Run ended without concrete progress"),
            ),
        };
    }

    // 10. idempotent wake 已存在
    if input.idempotent_wake_exists {
        return RunContinuationDecision::Skip {
            reason: "continuation wake already exists for this source run and attempt".to_string(),
        };
    }

    // 11. enqueue
    let next_attempt = current_attempt + 1;
    let idempotency_key = build_run_liveness_continuation_idempotency_key(&IdempotencyKeyInput {
        issue_id: issue.id.clone(),
        source_run_id: input.run.id.clone(),
        liveness_state: liveness_state.clone(),
        next_attempt,
    });
    let instruction = input.next_action.clone().unwrap_or_else(|| {
        "The previous run ended without concrete progress. Take the first concrete action \
         now or mark the issue blocked with a specific unblock request."
            .to_string()
    });

    RunContinuationDecision::Enqueue {
        next_attempt,
        idempotency_key,
        instruction,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn run(attempt: Option<i64>) -> HeartbeatRunRef {
        HeartbeatRunRef {
            id: "run-1".to_string(),
            company_id: "co1".to_string(),
            agent_id: "agent-1".to_string(),
            continuation_attempt: attempt,
        }
    }

    fn issue(status: &str, exec_state: Option<&str>) -> IssueRef {
        IssueRef {
            id: "issue-1".to_string(),
            company_id: "co1".to_string(),
            status: status.to_string(),
            assignee_agent_id: Some("agent-1".to_string()),
            execution_state: exec_state.map(String::from),
        }
    }

    fn agent(status: &str) -> AgentRef {
        AgentRef {
            id: "agent-1".to_string(),
            company_id: "co1".to_string(),
            status: status.to_string(),
        }
    }

    fn base_input(
        run: HeartbeatRunRef,
        issue: Option<IssueRef>,
        agent: Option<AgentRef>,
        liveness_state: Option<&str>,
    ) -> DecideRunLivenessContinuationInput {
        DecideRunLivenessContinuationInput {
            run,
            issue,
            agent,
            liveness_state: liveness_state.map(String::from),
            liveness_reason: Some("no progress".to_string()),
            next_action: Some("do something".to_string()),
            budget_blocked: false,
            idempotent_wake_exists: false,
            max_attempts: None,
        }
    }

    // -----------------------------------------------------------------------
    // read_continuation_attempt
    // -----------------------------------------------------------------------

    #[test]
    fn read_attempt_handles_numeric_string() {
        let v: Option<&dyn std::fmt::Display> = Some(&"3");
        assert_eq!(read_continuation_attempt(v), 3);
    }

    #[test]
    fn read_attempt_clamps_zero_or_negative_to_zero() {
        let v: Option<&dyn std::fmt::Display> = Some(&"0");
        assert_eq!(read_continuation_attempt(v), 0);
        let v: Option<&dyn std::fmt::Display> = Some(&"-5");
        assert_eq!(read_continuation_attempt(v), 0);
    }

    #[test]
    fn read_attempt_rejects_garbage() {
        let v: Option<&dyn std::fmt::Display> = Some(&"abc");
        assert_eq!(read_continuation_attempt(v), 0);
    }

    #[test]
    fn read_attempt_handles_none() {
        assert_eq!(read_continuation_attempt(None), 0);
    }

    // -----------------------------------------------------------------------
    // build_run_liveness_continuation_idempotency_key
    // -----------------------------------------------------------------------

    #[test]
    fn idempotency_key_format() {
        let key = build_run_liveness_continuation_idempotency_key(&IdempotencyKeyInput {
            issue_id: "is1".to_string(),
            source_run_id: "run1".to_string(),
            liveness_state: "empty_response".to_string(),
            next_attempt: 1,
        });
        assert_eq!(key, "run_liveness_continuation:is1:run1:empty_response:1");
    }

    // -----------------------------------------------------------------------
    // decide_run_liveness_continuation: skip branches
    // -----------------------------------------------------------------------

    #[test]
    fn skip_when_liveness_state_missing() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("active")),
            None,
        ));
        assert!(matches!(d, RunContinuationDecision::Skip { .. }));
    }

    #[test]
    fn skip_when_liveness_state_not_actionable() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("blocked_external"),
        ));
        assert!(matches!(d, RunContinuationDecision::Skip { .. }));
    }

    #[test]
    fn skip_when_issue_missing() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            None,
            Some(agent("active")),
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => assert_eq!(reason, "issue not found"),
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_agent_missing() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(issue("todo", None)),
            None,
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => assert_eq!(reason, "agent not found"),
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_company_scope_mismatch() {
        let mut r = run(None);
        r.company_id = "co2".to_string();
        let d = decide_run_liveness_continuation(&base_input(
            r,
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert_eq!(reason, "company scope mismatch");
            }
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_issue_assignee_mismatch() {
        let mut is = issue("todo", None);
        is.assignee_agent_id = Some("other-agent".to_string());
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(is),
            Some(agent("active")),
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert!(reason.contains("no longer assigned"));
            }
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_issue_status_not_active() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(issue("done", None)),
            Some(agent("active")),
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert!(reason.contains("done"));
            }
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_issue_has_execution_state() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(issue("todo", Some("blocked"))),
            Some(agent("active")),
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert!(reason.contains("execution policy state"));
            }
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_agent_status_not_invokable() {
        let d = decide_run_liveness_continuation(&base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("terminated")),
            Some("plan_only"),
        ));
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert!(reason.contains("terminated"));
            }
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_budget_blocked() {
        let mut input = base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("plan_only"),
        );
        input.budget_blocked = true;
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert!(reason.contains("budget"));
            }
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn skip_when_idempotent_wake_exists() {
        let mut input = base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("plan_only"),
        );
        input.idempotent_wake_exists = true;
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Skip { reason } => {
                assert!(reason.contains("wake already exists"));
            }
            _ => panic!("expected skip"),
        }
    }

    // -----------------------------------------------------------------------
    // decide_run_liveness_continuation: exhausted branch
    // -----------------------------------------------------------------------

    #[test]
    fn exhausted_when_attempts_at_max() {
        let mut input = base_input(
            run(Some(2)),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("plan_only"),
        );
        input.max_attempts = Some(2);
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Exhausted {
                attempt,
                max_attempts,
                ..
            } => {
                assert_eq!(attempt, 2);
                assert_eq!(max_attempts, 2);
            }
            _ => panic!("expected exhausted"),
        }
    }

    #[test]
    fn exhausted_comment_contains_state_and_reason() {
        let mut input = base_input(
            run(Some(3)),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("empty_response"),
        );
        input.liveness_reason = Some("no concrete progress".to_string());
        input.max_attempts = Some(2);
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Exhausted { comment, .. } => {
                assert!(comment.contains("empty_response"));
                assert!(comment.contains("no concrete progress"));
                assert!(comment.contains("3/2"));
            }
            _ => panic!("expected exhausted"),
        }
    }

    // -----------------------------------------------------------------------
    // decide_run_liveness_continuation: enqueue branch
    // -----------------------------------------------------------------------

    #[test]
    fn enqueue_when_all_conditions_met() {
        let input = base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("plan_only"),
        );
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Enqueue {
                next_attempt,
                idempotency_key,
                instruction,
            } => {
                assert_eq!(next_attempt, 1);
                assert!(idempotency_key.contains("plan_only"));
                assert!(idempotency_key.contains("1"));
                assert_eq!(instruction, "do something");
            }
            _ => panic!("expected enqueue"),
        }
    }

    #[test]
    fn enqueue_uses_default_instruction_when_next_action_missing() {
        let mut input = base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("plan_only"),
        );
        input.next_action = None;
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Enqueue { instruction, .. } => {
                assert!(instruction.contains("first concrete action"));
            }
            _ => panic!("expected enqueue"),
        }
    }

    #[test]
    fn enqueue_increments_attempt_from_existing() {
        let input = base_input(
            run(Some(1)),
            Some(issue("todo", None)),
            Some(agent("active")),
            Some("empty_response"),
        );
        let d = decide_run_liveness_continuation(&input);
        match d {
            RunContinuationDecision::Enqueue { next_attempt, .. } => {
                assert_eq!(next_attempt, 2);
            }
            _ => panic!("expected enqueue"),
        }
    }

    #[test]
    fn enqueue_allows_error_agent_status() {
        // Per Node: prior error should not permanently suppress bounded continuation
        let input = base_input(
            run(None),
            Some(issue("todo", None)),
            Some(agent("error")),
            Some("plan_only"),
        );
        let d = decide_run_liveness_continuation(&input);
        assert!(matches!(d, RunContinuationDecision::Enqueue { .. }));
    }

    // -----------------------------------------------------------------------
    // Serde round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn decision_serde_round_trip() {
        let decisions = vec![
            RunContinuationDecision::Enqueue {
                next_attempt: 1,
                idempotency_key: "k".to_string(),
                instruction: "i".to_string(),
            },
            RunContinuationDecision::Exhausted {
                attempt: 2,
                max_attempts: 2,
                comment: "c".to_string(),
            },
            RunContinuationDecision::Skip {
                reason: "r".to_string(),
            },
        ];
        for d in decisions {
            let json = serde_json::to_string(&d).unwrap();
            let back: RunContinuationDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(d, back);
        }
    }

    #[test]
    fn decision_kind_helper() {
        assert_eq!(
            RunContinuationDecision::Skip {
                reason: "x".to_string()
            }
            .kind(),
            "skip"
        );
        assert_eq!(
            RunContinuationDecision::Exhausted {
                attempt: 1,
                max_attempts: 2,
                comment: "c".to_string()
            }
            .kind(),
            "exhausted"
        );
        assert_eq!(
            RunContinuationDecision::Enqueue {
                next_attempt: 1,
                idempotency_key: "k".to_string(),
                instruction: "i".to_string()
            }
            .kind(),
            "enqueue"
        );
    }
}
