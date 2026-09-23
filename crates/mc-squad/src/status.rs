//! squad member presence 派生 —— 上游 `server/internal/handler/squad.go` L591–L653
//! （`deriveRuntimeAvailability` / `deriveSquadMemberStatus`）的纯函数搬移。
//!
//! 为什么放在这里而不是 handler：这两段是**纯逻辑**（输入 runtime 行 + 在飞任务，输出
//! 一个枚举字符串），没有任何 SQL 或 HTTP 形状。放进 `mc-squad` 后可以只靠单元测试
//! 覆盖全部分支组合（3×3×2 ≈ 18 条），不必起真库；路由层只负责把 DB 行喂进来。
//!
//! 上游注释里的两条判据（逐字保留语义）：
//! - **workload 优先于 runtime 健康**：`working` 即使 runtime 短暂掉线也算 working
//!   （`derive-presence.ts` 的 workload + availability 两段式）；
//! - **archived 恒 archived**：归档 agent 不论残留多少 runtime / task 行都报 `archived`，
//!   否则残留的 `online` runtime 行会把它显示成看起来只是「离线」。
//!
//! 状态词表（上游 `SquadMemberStatusResponse.Status`）：
//! `working` / `idle` / `unstable` / `offline` / `archived`。
//! `agent_runtime.status` 自身的 CHECK 只有 `online|offline`（本仓
//! `migrations/0001_init.up.sql:230` 那条 CHECK 是错的，不当契约）。

use chrono::{DateTime, Duration, Utc};

/// `agent_runtime.status` 在线（上游 `deriveRuntimeAvailability` 唯一认的在线值）。
pub const RUNTIME_STATUS_ONLINE: &str = "online";

/// 可用性三态：在线。
pub const AVAILABILITY_ONLINE: &str = "online";
/// 可用性三态：掉线不足 5 分钟（心跳残留）。
pub const AVAILABILITY_UNSTABLE: &str = "unstable";
/// 可用性三态：离线。
pub const AVAILABILITY_OFFLINE: &str = "offline";

/// squad 成员状态：有 dispatched/running 任务。
pub const MEMBER_STATUS_WORKING: &str = "working";
/// squad 成员状态：runtime 在线且无在飞任务。
pub const MEMBER_STATUS_IDLE: &str = "idle";
/// squad 成员状态：runtime 掉线但在不稳定窗口内。
pub const MEMBER_STATUS_UNSTABLE: &str = "unstable";
/// squad 成员状态：离线（或没有 runtime 行）。
pub const MEMBER_STATUS_OFFLINE: &str = "offline";
/// squad 成员状态：agent 已归档（MUL-2319 决定：仍出现在列表里，但恒为 archived）。
pub const MEMBER_STATUS_ARCHIVED: &str = "archived";

/// 任务状态：已派发（计入 working）。
pub const TASK_STATUS_DISPATCHED: &str = "dispatched";
/// 任务状态：执行中（计入 working）。
pub const TASK_STATUS_RUNNING: &str = "running";
/// 任务状态：等本地目录 —— **仍在飞**（issue 要可见），但**不**算 working。
pub const TASK_STATUS_WAITING_LOCAL_DIRECTORY: &str = "waiting_local_directory";

/// `unstable` 窗口：`last_seen_at` 在这个时长内即视为「刚掉线」。
pub const UNSTABLE_WINDOW_MINUTES: i64 = 5;

/// 上游 `deriveRuntimeAvailability`：runtime 行 → `online` / `unstable` / `offline`。
///
/// `runtime_status` 为 `None` 等价于上游的 `!runtimeStatus.Valid`（没有 runtime 行，
/// 或 `ar.status` 为 NULL）⇒ `offline`。未知字符串（不是 `online`）走 `last_seen_at`
/// 分支，与上游逐字一致。
pub fn derive_runtime_availability(
    runtime_status: Option<&str>,
    last_seen_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> &'static str {
    let Some(status) = runtime_status else {
        return AVAILABILITY_OFFLINE;
    };
    if status == RUNTIME_STATUS_ONLINE {
        return AVAILABILITY_ONLINE;
    }
    let Some(seen) = last_seen_at else {
        return AVAILABILITY_OFFLINE;
    };
    if now.signed_duration_since(seen) < Duration::minutes(UNSTABLE_WINDOW_MINUTES) {
        AVAILABILITY_UNSTABLE
    } else {
        AVAILABILITY_OFFLINE
    }
}

/// 该任务状态是否把成员计入 `working`（上游的 `dispatched || running`）。
pub fn is_working_task_status(status: &str) -> bool {
    matches!(status, TASK_STATUS_DISPATCHED | TASK_STATUS_RUNNING)
}

/// 该任务状态是否算「在飞」——`ListSquadMemberStatusRows` 的
/// `status IN ('dispatched','running','waiting_local_directory')` 那一支。
///
/// `waiting_local_directory` 保留在行集里只为让它的 issue 可见，**不**进 working 桶
/// （squad 状态词表没有 queued）。
pub fn is_in_flight_task_status(status: &str) -> bool {
    matches!(
        status,
        TASK_STATUS_DISPATCHED | TASK_STATUS_RUNNING | TASK_STATUS_WAITING_LOCAL_DIRECTORY
    )
}

/// 上游 `deriveSquadMemberStatus`：runtime + task 信号 → 成员状态桶。
///
/// 判定顺序**逐字**照搬上游：`archived` → `working` → availability
/// （`online` 折叠成 `idle`，其余原样返回）。
pub fn derive_member_status(
    archived: bool,
    runtime_status: Option<&str>,
    last_seen_at: Option<DateTime<Utc>>,
    has_working_task: bool,
    now: DateTime<Utc>,
) -> &'static str {
    if archived {
        return MEMBER_STATUS_ARCHIVED;
    }
    if has_working_task {
        return MEMBER_STATUS_WORKING;
    }
    match derive_runtime_availability(runtime_status, last_seen_at, now) {
        AVAILABILITY_ONLINE => MEMBER_STATUS_IDLE,
        other => {
            // availability 的另两态与成员状态同名，直接透传（上游也是直接 return）。
            match other {
                AVAILABILITY_UNSTABLE => MEMBER_STATUS_UNSTABLE,
                _ => MEMBER_STATUS_OFFLINE,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("fixed timestamp")
    }

    fn ago(minutes: i64) -> DateTime<Utc> {
        now() - Duration::minutes(minutes)
    }

    #[test]
    fn availability_matches_upstream_branches() {
        // 有 runtime 行且在线 ⇒ online（不论 last_seen 多旧）。
        assert_eq!(
            derive_runtime_availability(Some("online"), None, now()),
            AVAILABILITY_ONLINE
        );
        assert_eq!(
            derive_runtime_availability(Some("online"), Some(ago(600)), now()),
            AVAILABILITY_ONLINE
        );
        // 没有 runtime 行 ⇒ offline（等价上游 !runtimeStatus.Valid）。
        assert_eq!(
            derive_runtime_availability(None, None, now()),
            AVAILABILITY_OFFLINE
        );
        // 离线但心跳在 5 分钟内 ⇒ unstable。
        assert_eq!(
            derive_runtime_availability(Some("offline"), Some(ago(1)), now()),
            AVAILABILITY_UNSTABLE
        );
        // 刚好 5 分钟 ⇒ 不满足 `< 5min` ⇒ offline（边界，上游同上）。
        assert_eq!(
            derive_runtime_availability(Some("offline"), Some(ago(5)), now()),
            AVAILABILITY_OFFLINE
        );
        // 离线且没有心跳 ⇒ offline。
        assert_eq!(
            derive_runtime_availability(Some("offline"), None, now()),
            AVAILABILITY_OFFLINE
        );
        // 未知状态串走心跳分支（上游只对字面量 "online" 短路）。
        assert_eq!(
            derive_runtime_availability(Some("degraded"), Some(ago(1)), now()),
            AVAILABILITY_UNSTABLE
        );
    }

    #[test]
    fn member_status_precedence_archived_then_workload_then_availability() {
        // archived 压过一切，连 online runtime + 在飞任务都不例外。
        assert_eq!(
            derive_member_status(true, Some("online"), Some(now()), true, now()),
            MEMBER_STATUS_ARCHIVED
        );
        // workload 压过 runtime 健康：runtime 掉线也在 working。
        assert_eq!(
            derive_member_status(false, Some("offline"), Some(ago(30)), true, now()),
            MEMBER_STATUS_WORKING
        );
        // online 且无任务 ⇒ idle（不是 online）。
        assert_eq!(
            derive_member_status(false, Some("online"), None, false, now()),
            MEMBER_STATUS_IDLE
        );
        assert_eq!(
            derive_member_status(false, Some("offline"), Some(ago(1)), false, now()),
            MEMBER_STATUS_UNSTABLE
        );
        assert_eq!(
            derive_member_status(false, None, None, false, now()),
            MEMBER_STATUS_OFFLINE
        );
    }

    #[test]
    fn task_status_buckets_split_waiting_from_working() {
        assert!(is_working_task_status("dispatched"));
        assert!(is_working_task_status("running"));
        assert!(!is_working_task_status("waiting_local_directory"));
        assert!(!is_working_task_status("queued"));
        // 在飞集合 = working 两态 + waiting_local_directory。
        for status in ["dispatched", "running", "waiting_local_directory"] {
            assert!(is_in_flight_task_status(status), "{status} 应在飞");
        }
        for status in ["queued", "completed", "failed", "cancelled"] {
            assert!(!is_in_flight_task_status(status), "{status} 不在飞");
        }
    }
}
