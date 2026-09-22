//! `retry` 的测试。

use super::*;
use mc_core::Id;

fn issue_link() -> TaskLink {
    TaskLink::Issue(Id::new())
}

#[test]
fn all_reasons_round_trip_and_match_upstream_order() {
    // taskfailure.allReasons 的前 27 个，逐字比对（顺序也一致）。
    let upstream = [
        "queued_expired",
        "runtime_offline",
        "runtime_reconnect_timeout",
        "runtime_recovery",
        "timeout",
        "iteration_limit",
        "agent_blocked",
        "api_invalid_request",
        "skill_bundle_unavailable",
        "runtime_cli_timeout",
        "environment_prepare_failed",
        "invalid_task_identity",
        "runtime_access_denied",
        "agent_error.provider_auth_or_access",
        "agent_error.provider_quota_limit",
        "agent_error.provider_capacity_or_rate_limit",
        "agent_error.provider_server_error",
        "agent_error.provider_network",
        "agent_error.process_failure",
        "agent_error.empty_or_unparseable_output",
        "agent_error.agent_timeout",
        "agent_error.context_overflow",
        "agent_error.missing_config",
        "agent_error.model_not_found_or_unavailable",
        "agent_error.runtime_version_unsupported",
        "agent_error.runtime_missing_executable",
        "agent_error.unknown",
    ];
    let ours: Vec<&str> = FailureReason::ALL[..27]
        .iter()
        .map(|r| r.as_str())
        .collect();
    assert_eq!(ours, upstream.to_vec());

    for reason in FailureReason::ALL {
        assert_eq!(FailureReason::parse(reason.as_str()).unwrap(), reason);
    }
    for extra in [
        FailureReason::AgentFallbackMessage,
        FailureReason::CodexResumeOversized,
    ] {
        assert_eq!(FailureReason::parse(extra.as_str()).unwrap(), extra);
    }
    assert!(FailureReason::parse("nope").is_err());
}

#[test]
fn agent_error_prefix_rule_matches_upstream() {
    for reason in FailureReason::ALL {
        let by_prefix = reason.as_str().starts_with("agent_error.");
        assert_eq!(
            reason.is_agent_error(),
            by_prefix,
            "{} 的前缀判定不一致",
            reason.as_str()
        );
    }
    // 上游 IsAgentError 只按前缀判定：CodexSemanticInactivity 不是 agent_error.*，
    // 所以它是 false —— 这是与上游一致的行为，不是遗漏。
    assert!(!FailureReason::CodexSemanticInactivity.is_agent_error());
}

#[test]
fn coarse_classes_are_exactly_the_five_from_055() {
    let upstream = [
        "agent_error",
        "timeout",
        "runtime_offline",
        "runtime_recovery",
        "manual",
    ];
    let ours: Vec<&str> = FailureClass::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(ours, upstream.to_vec());
    assert!(FailureClass::parse("delegated_failure").is_err());

    // EVERY reason 都必须落进某一个桶（分类是全函数）。
    for reason in FailureReason::ALL {
        let class = reason.class();
        assert!(FailureClass::ALL.contains(&class));
    }
}

#[test]
fn retryable_set_equals_upstream_retryable_reasons() {
    let upstream = [
        "runtime_offline",
        "runtime_recovery",
        "timeout",
        "skill_bundle_unavailable",
        "agent_error.provider_network",
        "codex_semantic_inactivity",
    ];
    let ours: Vec<&str> = FailureReason::ALL
        .iter()
        .filter(|r| r.is_retryable())
        .map(|r| r.as_str())
        .collect();
    // 顺序无意义（Go 的 map 遍历本来就是随机的），这里只需集合相等。
    let mut sorted_ours = ours.clone();
    sorted_ours.sort_unstable();
    let mut sorted_upstream = upstream.to_vec();
    sorted_upstream.sort_unstable();
    assert_eq!(sorted_ours, sorted_upstream);
    assert_eq!(upstream.len(), 6, "上游 retryableReasons 恰好 6 项");
    // manual 恒不可重试（人工终止不许被自动复活）。
    assert!(!FailureReason::Manual.is_retryable());
    assert!(FailureReason::Manual.class().is_human_initiated());
    assert!(!FailureReason::Manual.is_persisted_in_failure_reason_column());
}

#[test]
fn resume_unsafe_set_equals_upstream() {
    let expected = [
        FailureReason::IterationLimit,
        FailureReason::AgentFallbackMessage,
        FailureReason::ApiInvalidRequest,
        FailureReason::CodexSemanticInactivity,
        FailureReason::AgentContextOverflow,
        FailureReason::CodexResumeOversized,
    ];
    for reason in FailureReason::ALL {
        assert_eq!(
            reason.is_resume_unsafe(),
            expected.contains(&reason),
            "{} 的 resume 安全性判定不一致",
            reason.as_str()
        );
    }
}

#[test]
fn attempt_ceiling_only_widens_and_never_revives() {
    // provider_network 是唯一的专用上限。
    assert_eq!(
        FailureReason::AgentProviderNetwork.attempt_ceiling(2),
        PROVIDER_NETWORK_MAX_ATTEMPTS
    );
    assert_eq!(FailureReason::Timeout.attempt_ceiling(2), 2);
    // max_attempts <= 1 时绝不放大（055: "1 disables retry"）。
    assert_eq!(FailureReason::AgentProviderNetwork.attempt_ceiling(1), 1);
    assert_eq!(FailureReason::AgentProviderNetwork.attempt_ceiling(0), 0);
}

#[test]
fn retry_delay_matches_upstream_schedule() {
    // runtime_offline 恒为 1s（走健康门控提升，不立即 claim）。
    assert_eq!(FailureReason::RuntimeOffline.retry_delay_secs(1), 1);
    assert_eq!(FailureReason::RuntimeOffline.retry_delay_secs(2), 1);
    // provider_network：前两次立即，末次（failed_attempt >= 2）冷却 5s。
    assert_eq!(FailureReason::AgentProviderNetwork.retry_delay_secs(1), 0);
    assert_eq!(
        FailureReason::AgentProviderNetwork.retry_delay_secs(2),
        PROVIDER_NETWORK_FINAL_RETRY_WAIT_SECS
    );
    // 其余可重试原因为立即。
    assert_eq!(FailureReason::Timeout.retry_delay_secs(1), 0);
}

#[test]
fn retry_decision_happy_path_creates_a_queued_child() {
    let d = decide_retry(
        FailureReason::Timeout,
        RetryBudget::FIRST_RUN,
        RetryGate::new(RunKind::Interactive, issue_link(), false),
    );
    assert!(d.retry);
    assert_eq!(d.skip, None);
    assert_eq!(d.ceiling, DEFAULT_MAX_ATTEMPTS);
    let child = d.child.unwrap();
    assert_eq!(child.attempt, 2);
    assert_eq!(child.max_attempts, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(child.status, TaskStatus::Queued);
    assert_eq!(child.delay_secs, 0);
    assert!(!child.force_fresh_session);
}

#[test]
fn retry_decision_deferred_child_for_runtime_offline() {
    let d = decide_retry(
        FailureReason::RuntimeOffline,
        RetryBudget::FIRST_RUN,
        RetryGate::new(RunKind::Interactive, issue_link(), false),
    );
    let child = d.child.unwrap();
    assert_eq!(child.status, TaskStatus::Deferred);
    assert_eq!(child.delay_secs, RUNTIME_OFFLINE_RETRY_DEFERRAL_SECS);
}

#[test]
fn retry_decision_forces_fresh_session_for_poisoned_reasons() {
    // codex_semantic_inactivity 可重试且 resume-unsafe。
    let d = decide_retry(
        FailureReason::CodexSemanticInactivity,
        RetryBudget::FIRST_RUN,
        RetryGate::new(RunKind::Interactive, issue_link(), false),
    );
    assert!(d.retry);
    assert!(d.resume_unsafe);
    assert!(d.child.unwrap().force_fresh_session);
}

#[test]
fn retry_decision_skip_table_is_exhaustive() {
    let link = issue_link();
    let cases = [
        (
            FailureReason::AgentProviderAuthOrAccess,
            RetryBudget::FIRST_RUN,
            RetryGate::new(RunKind::Interactive, link, false),
            RetrySkip::ReasonNotRetryable,
        ),
        // budget 用尽（attempt == ceiling）
        (
            FailureReason::Timeout,
            RetryBudget::new(2, 2),
            RetryGate::new(RunKind::Interactive, link, false),
            RetrySkip::BudgetExhausted,
        ),
        // max_attempts=1 显式关闭
        (
            FailureReason::Timeout,
            RetryBudget::new(1, 1),
            RetryGate::new(RunKind::Interactive, link, false),
            RetrySkip::BudgetExhausted,
        ),
        (
            FailureReason::Timeout,
            RetryBudget::FIRST_RUN,
            RetryGate::new(RunKind::Autopilot, link, false),
            RetrySkip::AutopilotRun,
        ),
        (
            FailureReason::Timeout,
            RetryBudget::FIRST_RUN,
            RetryGate::new(RunKind::Triage, link, false),
            RetrySkip::TriageRun,
        ),
        (
            FailureReason::Timeout,
            RetryBudget::FIRST_RUN,
            RetryGate::new(RunKind::Interactive, TaskLink::None, false),
            RetrySkip::NoRunnableLink,
        ),
        (
            FailureReason::Timeout,
            RetryBudget::FIRST_RUN,
            RetryGate::new(RunKind::Interactive, link, true),
            RetrySkip::PendingSuccessor,
        ),
    ];
    for (reason, budget, gate, want) in cases {
        let d = decide_retry(reason, budget, gate);
        assert!(!d.retry, "{} 不该重试", reason.as_str());
        assert_eq!(d.skip, Some(want), "{} 的 skip 原因不对", reason.as_str());
        assert!(d.child.is_none());
    }
}

#[test]
fn manual_termination_never_auto_retries() {
    for gate in [
        RetryGate::new(RunKind::Interactive, issue_link(), false),
        RetryGate::new(RunKind::Interactive, TaskLink::QuickCreate, false),
    ] {
        let d = decide_retry(FailureReason::Manual, RetryBudget::FIRST_RUN, gate);
        assert!(!d.retry);
        assert_eq!(d.skip, Some(RetrySkip::ReasonNotRetryable));
    }
}

#[test]
fn quick_create_and_chat_links_are_runnable() {
    let chat = TaskLink::Chat(Id::new());
    for link in [chat, TaskLink::QuickCreate] {
        assert!(link.is_runnable());
        let d = decide_retry(
            FailureReason::RuntimeRecovery,
            RetryBudget::FIRST_RUN,
            RetryGate::new(RunKind::Interactive, link, false),
        );
        assert!(d.retry, "{link:?} 应当可以重试");
    }
    assert!(!TaskLink::None.is_runnable());
}

#[test]
fn provider_network_chain_stays_self_consistent() {
    // 第三次（末次）失败：attempt=3 撞上放大后的 ceiling=3 ⇒ 不再重试。
    let d = decide_retry(
        FailureReason::AgentProviderNetwork,
        RetryBudget::new(3, 2),
        RetryGate::new(RunKind::Interactive, issue_link(), false),
    );
    assert_eq!(d.ceiling, PROVIDER_NETWORK_MAX_ATTEMPTS);
    assert!(!d.retry);
    assert_eq!(d.skip, Some(RetrySkip::BudgetExhausted));

    // 第二次失败时子行的 attempt/max_attempts 必须一致（3/3，不是 3/2）。
    let d = decide_retry(
        FailureReason::AgentProviderNetwork,
        RetryBudget::new(2, 2),
        RetryGate::new(RunKind::Interactive, issue_link(), false),
    );
    let child = d.child.unwrap();
    assert_eq!(child.attempt, 3);
    assert_eq!(child.max_attempts, child.attempt);
}

#[test]
fn serialized_form_is_the_wire_form_not_a_derived_one() {
    // 防止 `derive(Serialize)` 的 snake_case 悄悄偏离上游列值 / 事件串。
    for reason in FailureReason::ALL {
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(json, format!("\"{}\"", reason.as_str()));
        let back: FailureReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, reason);
    }
    for class in FailureClass::ALL {
        let json = serde_json::to_string(&class).unwrap();
        assert_eq!(json, format!("\"{}\"", class.as_str()));
    }
    assert!(serde_json::from_str::<FailureReason>("\"terminal_failed\"").is_err());
}

#[test]
fn advisory_skip_is_marked_advisory() {
    assert!(RetrySkip::PendingSuccessor.is_advisory());
    assert!(!RetrySkip::BudgetExhausted.is_advisory());
    assert!(!RetrySkip::NoRunnableLink.is_advisory());
}
