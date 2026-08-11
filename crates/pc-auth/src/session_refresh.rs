//! Session 刷新 / 轮换策略。
//!
//! 与 Node `auth/better-auth.ts` 中 session rotation 等价：
//! - idle timeout：每次使用 session 把 expires_at 向前推 `idle_window`。
//! - absolute timeout：从 issued_at 起 `absolute_lifetime` 后强制失效。
//! - rotation：当 token 被使用且距离 last_rotated 超过 `rotate_every`，
//!   服务端应颁发新 token 并作废旧 token（防固定 token 泄漏）。

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// 会话策略。所有字段为可空；`None` 表示"不限"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPolicy {
    /// 空闲窗口：每次使用把 expires_at 推进到 `now + idle_window`。
    pub idle_window: Duration,
    /// 绝对生命周期：从 issued_at 起，超过这个时间后必须重新登录。
    pub absolute_lifetime: Duration,
    /// 轮换周期：距离 `last_rotated_at` 超过这个时间时，应颁发新 token。
    pub rotate_every: Duration,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            // 30 min idle
            idle_window: Duration::minutes(30),
            // 30 days absolute
            absolute_lifetime: Duration::days(30),
            // 12 hour rotation
            rotate_every: Duration::hours(12),
        }
    }
}

impl SessionPolicy {
    /// 构造一个新的会话记录。
    #[must_use]
    pub fn new_session(&self, now: DateTime<Utc>) -> SessionRecord {
        SessionRecord {
            issued_at: now,
            expires_at: now + self.idle_window,
            last_used_at: now,
            last_rotated_at: now,
            revoked_at: None,
        }
    }
}

/// 顶层便捷函数：等价于 `SessionPolicy::default().new_session(now)`。
#[must_use]
pub fn new_session_record(now: DateTime<Utc>) -> SessionRecord {
    SessionPolicy::default().new_session(now)
}

/// 会话状态（持久化层视角）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub last_rotated_at: DateTime<Utc>,
    /// R512: 该 token 是否已被作废（轮换或显式登出）。`Some(ts)` 表示作废时间。
    /// 持久化层可序列化为 `null`（未作废）以兼容旧记录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCheckOutcome {
    /// 仍然有效；返回建议的 `expires_at`（可能与原值相同）。
    Ok { new_expires_at: DateTime<Utc> },
    /// 已超过绝对生命周期 —— 强制登出。
    ExpiredAbsolute,
    /// 已超过 idle 窗口 —— token 过期。
    ExpiredIdle,
    /// R512: token 已被作废（轮换 / 显式登出）—— 应视为攻击信号。
    Revoked,
}

impl SessionCheckOutcome {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

/// 校验一个会话是否仍然有效，并根据 idle 窗口返回建议的 `expires_at`。
/// 不修改 `record`；调用方负责把 `last_used_at` + `expires_at` 写回。
#[must_use]
pub fn check_session(
    policy: &SessionPolicy,
    record: &SessionRecord,
    now: DateTime<Utc>,
) -> SessionCheckOutcome {
    // R512: 已作废的 token 一律视为不可用（在 idle/absolute 之前检查）。
    if record.revoked_at.is_some() {
        return SessionCheckOutcome::Revoked;
    }
    // 绝对生命周期优先
    if now - record.issued_at >= policy.absolute_lifetime {
        return SessionCheckOutcome::ExpiredAbsolute;
    }
    if now >= record.expires_at {
        return SessionCheckOutcome::ExpiredIdle;
    }
    let new_expires_at = now + policy.idle_window;
    SessionCheckOutcome::Ok { new_expires_at }
}

/// 是否应该轮换 token（防止长期固定 token 泄漏）。
#[must_use]
pub fn should_rotate(policy: &SessionPolicy, record: &SessionRecord, now: DateTime<Utc>) -> bool {
    now - record.last_rotated_at >= policy.rotate_every
}

/// 给会话打上"刚刚被使用"标记，返回新 record。
#[must_use]
pub fn touch_session(
    policy: &SessionPolicy,
    record: &SessionRecord,
    now: DateTime<Utc>,
) -> SessionRecord {
    SessionRecord {
        issued_at: record.issued_at,
        expires_at: now + policy.idle_window,
        last_used_at: now,
        last_rotated_at: record.last_rotated_at,
        revoked_at: record.revoked_at,
    }
}

/// 给会话打上"刚刚被轮换"标记，返回新 record。
#[must_use]
pub fn rotate_session(
    _policy: &SessionPolicy,
    record: &SessionRecord,
    now: DateTime<Utc>,
) -> SessionRecord {
    SessionRecord {
        issued_at: record.issued_at,
        expires_at: record.expires_at,
        last_used_at: record.last_used_at,
        last_rotated_at: now,
        revoked_at: record.revoked_at,
    }
}

/// R512: 标记一个 session 已被作废。`now` 作为作废时间戳。
#[must_use]
pub fn mark_revoked(record: &SessionRecord, now: DateTime<Utc>) -> SessionRecord {
    SessionRecord {
        issued_at: record.issued_at,
        expires_at: record.expires_at,
        last_used_at: record.last_used_at,
        last_rotated_at: record.last_rotated_at,
        revoked_at: Some(now),
    }
}

/// R512: 该 session 是否已作废。
#[must_use]
pub fn is_revoked(record: &SessionRecord) -> bool {
    record.revoked_at.is_some()
}

/// R512: 重用检测的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseOutcome {
    /// 没有检测到重用 —— 当前 token 在 family 中是最新且未被作废。
    Ok,
    /// 检测到重用 —— 攻击信号：调用方应作废整个 family。
    ReuseDetected,
}

impl ReuseOutcome {
    #[must_use]
    pub fn is_reuse(&self) -> bool {
        matches!(self, Self::ReuseDetected)
    }
}

/// R512: 检测 token 是否被重用。
///
/// 重用判定规则（任一满足即视为重用）：
/// 1. **presented 本身已作废**：表示旧 token 又被拿来用。
/// 2. **family 中存在更新的活跃 token**：表示 token 已被轮换，旧 token 仍在被使用。
///
/// `presented` 是当前被提交的 session；`family` 是 family 内所有 session 的快照
/// （调用方负责提供；本函数不读取存储）。
#[must_use]
pub fn detect_reuse(presented: &SessionRecord, family: &[SessionRecord]) -> ReuseOutcome {
    // Rule 1: presented 自身已作废 → 重用。
    if presented.revoked_at.is_some() {
        return ReuseOutcome::ReuseDetected;
    }
    // Rule 2: family 中有比 presented 更新（last_rotated_at 更晚）且未作废的 token。
    for sibling in family {
        // 跳过自己
        if sibling.issued_at == presented.issued_at
            && sibling.last_rotated_at == presented.last_rotated_at
        {
            continue;
        }
        if sibling.revoked_at.is_none() && sibling.last_rotated_at > presented.last_rotated_at {
            return ReuseOutcome::ReuseDetected;
        }
    }
    ReuseOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r565_new_session_has_correct_expiry() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        assert_eq!(s.issued_at, now);
        assert_eq!(s.expires_at, now + p.idle_window);
        assert_eq!(s.last_rotated_at, now);
    }

    #[test]
    fn r565_fresh_session_is_ok() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        match check_session(&p, &s, now + Duration::minutes(5)) {
            SessionCheckOutcome::Ok { new_expires_at } => {
                assert_eq!(new_expires_at, now + Duration::minutes(5) + p.idle_window);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn r565_idle_expiry_fires() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        // idle window = 30 min, check at 31 min
        let outcome = check_session(&p, &s, now + Duration::minutes(31));
        assert_eq!(outcome, SessionCheckOutcome::ExpiredIdle);
    }

    #[test]
    fn r565_absolute_expiry_takes_priority_over_idle() {
        // 绝对生命周期比 idle 短；两个条件都满足时绝对优先。
        let mut p = SessionPolicy::default();
        p.absolute_lifetime = Duration::hours(2);
        p.idle_window = Duration::hours(1);
        let now = Utc::now();
        let s = p.new_session(now);
        // 3 小时后：idle 已过期（3h > 1h），absolute 也已过期（3h > 2h）。
        // 优先级：absolute 先命中。
        let outcome = check_session(&p, &s, now + Duration::hours(3));
        assert_eq!(outcome, SessionCheckOutcome::ExpiredAbsolute);
    }

    #[test]
    fn r565_session_killed_by_absolute_even_if_renewed_idle() {
        // 即使用户刚 touch 过 session，只要 absolute 到了就强制过期。
        // touch_session 会把 expires_at 推进，但 issued_at 不变。
        let mut p = SessionPolicy::default();
        p.absolute_lifetime = Duration::hours(2);
        p.idle_window = Duration::hours(1);
        let now = Utc::now();
        let s = p.new_session(now);
        // 1.5h 后 touch，expires_at 推到 2.5h
        let touched = touch_session(&p, &s, now + Duration::hours(1) + Duration::minutes(30));
        // 2.1h 后：absolute_lifetime(2h) 已过；expires_at 仍未到
        let outcome = check_session(
            &p,
            &touched,
            now + Duration::hours(2) + Duration::minutes(6),
        );
        assert_eq!(outcome, SessionCheckOutcome::ExpiredAbsolute);
    }

    #[test]
    fn r565_should_rotate_after_rotate_every() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        // rotate_every = 12h
        assert!(!should_rotate(&p, &s, now + Duration::hours(11)));
        assert!(should_rotate(&p, &s, now + Duration::hours(12)));
    }

    #[test]
    fn r565_touch_advances_expires_at() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        let after = now + Duration::minutes(10);
        let s2 = touch_session(&p, &s, after);
        assert_eq!(s2.last_used_at, after);
        assert_eq!(s2.expires_at, after + p.idle_window);
        assert_eq!(
            s2.last_rotated_at, s.last_rotated_at,
            "touch must not rotate"
        );
    }

    #[test]
    fn r565_rotate_session_advances_last_rotated() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        let after = now + Duration::hours(13);
        let s2 = rotate_session(&p, &s, after);
        assert_eq!(s2.last_rotated_at, after);
        assert_eq!(
            s2.last_used_at, s.last_used_at,
            "rotate must not change last_used"
        );
    }

    // -------- r512: family tracking + reuse detection --------

    #[test]
    fn r512_new_session_has_revoked_at_none() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        assert!(s.revoked_at.is_none());
        assert!(!is_revoked(&s));
    }

    #[test]
    fn r512_mark_revoked_sets_timestamp_and_preserves_other_fields() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        let later = now + Duration::minutes(7);
        let r = mark_revoked(&s, later);
        assert_eq!(r.revoked_at, Some(later));
        assert_eq!(r.issued_at, s.issued_at);
        assert_eq!(r.last_used_at, s.last_used_at);
        assert!(is_revoked(&r));
    }

    #[test]
    fn r512_check_session_returns_revoked_when_revoked_at_set() {
        let p = SessionPolicy::default();
        let now = Utc::now();
        let s = p.new_session(now);
        let r = mark_revoked(&s, now + Duration::minutes(1));
        let outcome = check_session(&p, &r, now + Duration::minutes(2));
        assert_eq!(outcome, SessionCheckOutcome::Revoked);
        assert!(!outcome.is_ok());
    }

    #[test]
    fn r512_revoked_takes_priority_over_idle_and_absolute() {
        // 即使 idle/absolute 都未过期，revoked 也要先命中。
        let mut p = SessionPolicy::default();
        p.idle_window = Duration::hours(1);
        p.absolute_lifetime = Duration::days(7);
        let now = Utc::now();
        let s = p.new_session(now);
        let r = mark_revoked(&s, now + Duration::minutes(1));
        let outcome = check_session(&p, &r, now + Duration::minutes(5));
        assert_eq!(outcome, SessionCheckOutcome::Revoked);
    }

    #[test]
    fn r512_detect_reuse_ok_for_fresh_presented_alone() {
        let now = Utc::now();
        let s = SessionRecord {
            issued_at: now,
            expires_at: now + Duration::minutes(30),
            last_used_at: now,
            last_rotated_at: now,
            revoked_at: None,
        };
        assert_eq!(detect_reuse(&s, &[s.clone()]), ReuseOutcome::Ok);
    }

    #[test]
    fn r512_detect_reuse_fires_when_presented_is_revoked() {
        let now = Utc::now();
        let s = SessionRecord {
            issued_at: now,
            expires_at: now + Duration::minutes(30),
            last_used_at: now,
            last_rotated_at: now,
            revoked_at: Some(now + Duration::minutes(1)),
        };
        assert_eq!(detect_reuse(&s, &[s.clone()]), ReuseOutcome::ReuseDetected);
    }

    #[test]
    fn r512_detect_reuse_fires_when_sibling_is_newer_and_active() {
        // presented 是旧 token；family 中存在一个更新的 active token → 重用。
        let now = Utc::now();
        let presented = SessionRecord {
            issued_at: now,
            expires_at: now + Duration::minutes(30),
            last_used_at: now,
            last_rotated_at: now,
            revoked_at: None,
        };
        let later = now + Duration::hours(1);
        let newer = SessionRecord {
            issued_at: now,
            expires_at: later + Duration::minutes(30),
            last_used_at: later,
            last_rotated_at: later,
            revoked_at: None,
        };
        let family = vec![presented.clone(), newer];
        assert_eq!(
            detect_reuse(&presented, &family),
            ReuseOutcome::ReuseDetected
        );
    }

    #[test]
    fn r512_detect_reuse_ok_when_newer_sibling_is_also_revoked() {
        // 较新的兄弟 token 已作废（被强制登出）→ 不会再被利用，不算 reuse。
        let now = Utc::now();
        let presented = SessionRecord {
            issued_at: now,
            expires_at: now + Duration::minutes(30),
            last_used_at: now,
            last_rotated_at: now,
            revoked_at: None,
        };
        let later = now + Duration::hours(1);
        let newer_revoked = SessionRecord {
            issued_at: now,
            expires_at: later + Duration::minutes(30),
            last_used_at: later,
            last_rotated_at: later,
            revoked_at: Some(later),
        };
        let family = vec![presented.clone(), newer_revoked];
        assert_eq!(detect_reuse(&presented, &family), ReuseOutcome::Ok);
    }

    #[test]
    fn r512_detect_reuse_skips_self_when_comparing_siblings() {
        // 边界情况：family 中只有 presented 自己，不应被自己的 last_rotated_at 误判。
        let now = Utc::now();
        let s = SessionRecord {
            issued_at: now,
            expires_at: now + Duration::minutes(30),
            last_used_at: now,
            last_rotated_at: now,
            revoked_at: None,
        };
        assert_eq!(detect_reuse(&s, std::slice::from_ref(&s)), ReuseOutcome::Ok);
    }
}
