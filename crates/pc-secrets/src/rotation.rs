//! Secret 轮换策略。
//!
//! 与 Node `secrets-rotation-policy.ts` 思路一致：依据 "最大有效期 +
//! 最大使用次数 + 手动触发" 判断是否需要轮换。
//!
//! 设计目标：
//! - 纯策略层（不依赖 provider），便于测试。
//! - 返回 `next_rotation_at` 和 `should_rotate` 判定。
//! - 支持 manual rotation flag。

use chrono::{DateTime, Duration, Utc};

/// 轮换策略配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationPolicyConfig {
    /// 最大有效期（None 表示不限）。
    pub max_age: Option<Duration>,
    /// 最大使用次数（None 表示不限）。
    pub max_uses: Option<u64>,
    /// 是否允许手动触发。
    pub allow_manual: bool,
}

impl Default for RotationPolicyConfig {
    fn default() -> Self {
        Self {
            max_age: Some(Duration::days(90)),
            max_uses: None,
            allow_manual: true,
        }
    }
}

/// 轮换触发原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RotationReason {
    /// 超过 max_age。
    MaxAgeExceeded,
    /// 超过 max_uses。
    MaxUsesExceeded,
    /// 手动触发。
    Manual,
    /// 紧急轮换（泄漏/被替换）。
    Emergency,
    /// 不需要轮换。
    NotNeeded,
}

impl RotationReason {
    #[must_use]
    pub fn requires_rotation(self) -> bool {
        !matches!(self, Self::NotNeeded)
    }
}

/// 轮换策略评估输入。
#[derive(Debug, Clone)]
pub struct RotationEvaluationInput {
    /// secret 创建时间。
    pub created_at: DateTime<Utc>,
    /// 上次轮换时间；None 表示从未轮换。
    pub last_rotated_at: Option<DateTime<Utc>>,
    /// 累计使用次数。
    pub use_count: u64,
    /// 手动触发标志。
    pub manual: bool,
    /// 紧急标志（安全事件 / 主动作废）。
    pub emergency: bool,
}

/// 轮换评估结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationEvaluation {
    pub reason: RotationReason,
    /// 下次轮换时间（基于 max_age + last_rotated_at）。
    pub next_rotation_at: Option<DateTime<Utc>>,
}

impl RotationEvaluation {
    #[must_use]
    pub fn should_rotate(&self) -> bool {
        self.reason.requires_rotation()
    }
}

/// 给定策略 + 输入，评估是否需要轮换。
#[must_use]
pub fn evaluate_rotation(
    policy: &RotationPolicyConfig,
    input: &RotationEvaluationInput,
    now: DateTime<Utc>,
) -> RotationEvaluation {
    if input.emergency {
        return RotationEvaluation {
            reason: RotationReason::Emergency,
            next_rotation_at: Some(now),
        };
    }
    if input.manual && policy.allow_manual {
        return RotationEvaluation {
            reason: RotationReason::Manual,
            next_rotation_at: Some(now),
        };
    }
    // 优先级：max_uses > max_age
    if let Some(max_uses) = policy.max_uses {
        if input.use_count >= max_uses {
            return RotationEvaluation {
                reason: RotationReason::MaxUsesExceeded,
                next_rotation_at: Some(now),
            };
        }
    }
    let anchor = input.last_rotated_at.unwrap_or(input.created_at);
    if let Some(max_age) = policy.max_age {
        let next_rotation_at = anchor + max_age;
        if now >= next_rotation_at {
            return RotationEvaluation {
                reason: RotationReason::MaxAgeExceeded,
                next_rotation_at: Some(next_rotation_at),
            };
        }
        return RotationEvaluation {
            reason: RotationReason::NotNeeded,
            next_rotation_at: Some(next_rotation_at),
        };
    }
    RotationEvaluation {
        reason: RotationReason::NotNeeded,
        next_rotation_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(days: i64) -> DateTime<Utc> {
        Utc::now() + Duration::days(days)
    }

    #[test]
    fn r567_fresh_secret_does_not_need_rotation() {
        let policy = RotationPolicyConfig::default();
        let input = RotationEvaluationInput {
            created_at: Utc::now(),
            last_rotated_at: None,
            use_count: 0,
            manual: false,
            emergency: false,
        };
        let r = evaluate_rotation(&policy, &input, Utc::now());
        assert_eq!(r.reason, RotationReason::NotNeeded);
        assert!(!r.should_rotate());
        assert!(r.next_rotation_at.is_some());
    }

    #[test]
    fn r567_old_secret_triggers_max_age() {
        let policy = RotationPolicyConfig {
            max_age: Some(Duration::days(30)),
            ..Default::default()
        };
        let input = RotationEvaluationInput {
            created_at: at(-60),
            last_rotated_at: None,
            use_count: 0,
            manual: false,
            emergency: false,
        };
        let r = evaluate_rotation(&policy, &input, Utc::now());
        assert_eq!(r.reason, RotationReason::MaxAgeExceeded);
        assert!(r.should_rotate());
    }

    #[test]
    fn r567_max_uses_triggers() {
        let policy = RotationPolicyConfig {
            max_age: None,
            max_uses: Some(100),
            ..Default::default()
        };
        let input = RotationEvaluationInput {
            created_at: Utc::now(),
            last_rotated_at: None,
            use_count: 100,
            manual: false,
            emergency: false,
        };
        let r = evaluate_rotation(&policy, &input, Utc::now());
        assert_eq!(r.reason, RotationReason::MaxUsesExceeded);
    }

    #[test]
    fn r567_manual_rotation_requires_allow() {
        let policy_disallow = RotationPolicyConfig {
            allow_manual: false,
            ..Default::default()
        };
        let input = RotationEvaluationInput {
            created_at: Utc::now(),
            last_rotated_at: None,
            use_count: 0,
            manual: true,
            emergency: false,
        };
        let r = evaluate_rotation(&policy_disallow, &input, Utc::now());
        assert_eq!(r.reason, RotationReason::NotNeeded);
    }

    #[test]
    fn r567_emergency_overrides_everything() {
        let policy = RotationPolicyConfig::default();
        let input = RotationEvaluationInput {
            created_at: Utc::now(),
            last_rotated_at: None,
            use_count: 0,
            manual: false,
            emergency: true,
        };
        let r = evaluate_rotation(&policy, &input, Utc::now());
        assert_eq!(r.reason, RotationReason::Emergency);
    }

    #[test]
    fn r567_max_uses_takes_priority_over_max_age() {
        let policy = RotationPolicyConfig {
            max_age: Some(Duration::days(30)),
            max_uses: Some(10),
            ..Default::default()
        };
        let input = RotationEvaluationInput {
            created_at: at(-60), // 已超过 max_age
            last_rotated_at: None,
            use_count: 100, // 也超过 max_uses
            manual: false,
            emergency: false,
        };
        let r = evaluate_rotation(&policy, &input, Utc::now());
        // 优先级：emergency > manual > max_uses > max_age
        assert_eq!(r.reason, RotationReason::MaxUsesExceeded);
    }

    #[test]
    fn r567_last_rotated_resets_clock() {
        let policy = RotationPolicyConfig {
            max_age: Some(Duration::days(30)),
            ..Default::default()
        };
        let input = RotationEvaluationInput {
            created_at: at(-100),
            last_rotated_at: Some(at(-5)),
            use_count: 0,
            manual: false,
            emergency: false,
        };
        let r = evaluate_rotation(&policy, &input, Utc::now());
        assert_eq!(r.reason, RotationReason::NotNeeded);
    }
}
