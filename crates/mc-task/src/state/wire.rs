//! `TaskEventKind` 的字符串形态与 serde 实现。
//!
//! 放在子模块里是为了让 `state.rs` 贴住 800 行尺寸门（gate ⑩），同时把
//! 「事件名的唯一真值」集中到一处：线上 9 个 `task:` 常量逐字照抄
//! `server/pkg/protocol/events.go:32-42`，三个服务端内部运维事件**不带**
//! `task:` 前缀（上游没有给它们线上名，本仓也不许自造）。
//!
//! serde 直接以 [`TaskEventKind::as_str`] 为准，所以
//! `serde_json::to_string(&kind)` 与 `kind.wire_name()` 永远一致 —— 不会出现
//! `#[serde(rename_all = "snake_case")]` 那种把 `task:queued` 变成 `"queued"`
//! 的静默漂移（前端按前缀订阅，漂了就收不到）。

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use super::TaskEventKind;
use crate::error::TaskError;

/// 内部运维事件字符串形态的前缀（明确区别于线上的 `task:`）。
///
/// 用它而不是 `task:`：M3-7 发布帧时若误把内部运维当线上事件发出去，
/// 前缀会立刻暴露错误，而不是发出一个“看起来合法”的假事件名。
pub const INTERNAL_EVENT_PREFIX: &str = "internal.";

impl TaskEventKind {
    /// 字符串形态：线上事件 = `task:<name>`，内部运维 = `internal.<name>`。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "task:queued",
            Self::Dispatch => "task:dispatch",
            Self::Running => "task:running",
            Self::WaitingLocalDirectory => "task:waiting_local_directory",
            Self::Progress => "task:progress",
            Self::Message => "task:message",
            Self::Completed => "task:completed",
            Self::Failed => "task:failed",
            Self::Cancelled => "task:cancelled",
            Self::Deferred => "internal.deferred",
            Self::Reclaim => "internal.reclaim",
            Self::RequeueAfterClaimFailure => "internal.requeue_after_claim_failure",
        }
    }

    /// 解析字符串形态；不认识的值**必须**报错，不做静默回退。
    ///
    /// # Errors
    ///
    /// [`TaskError::UnknownEvent`]：不是任何已知事件名（含大小写不符、
    /// 缺少 `task:` 前缀的裸名）。
    pub fn parse(value: &str) -> Result<Self, TaskError> {
        match value {
            "task:queued" => Ok(Self::Queued),
            "task:dispatch" => Ok(Self::Dispatch),
            "task:running" => Ok(Self::Running),
            "task:waiting_local_directory" => Ok(Self::WaitingLocalDirectory),
            "task:progress" => Ok(Self::Progress),
            "task:message" => Ok(Self::Message),
            "task:completed" => Ok(Self::Completed),
            "task:failed" => Ok(Self::Failed),
            "task:cancelled" => Ok(Self::Cancelled),
            "internal.deferred" => Ok(Self::Deferred),
            "internal.reclaim" => Ok(Self::Reclaim),
            "internal.requeue_after_claim_failure" => Ok(Self::RequeueAfterClaimFailure),
            got => Err(TaskError::UnknownEvent {
                got: got.to_owned(),
            }),
        }
    }
}

impl Serialize for TaskEventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TaskEventKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EventVisitor;

        impl de::Visitor<'_> for EventVisitor {
            type Value = TaskEventKind;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a `task:` event name (or an `internal.` lifecycle op)")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                TaskEventKind::parse(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(EventVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_events_agree_with_wire_name() {
        for kind in TaskEventKind::WIRE_EVENTS {
            assert_eq!(Some(kind.as_str()), kind.wire_name());
            assert_eq!(TaskEventKind::parse(kind.as_str()).unwrap(), kind);
        }
    }

    #[test]
    fn internal_ops_never_claim_a_task_prefix() {
        for kind in [
            TaskEventKind::Deferred,
            TaskEventKind::Reclaim,
            TaskEventKind::RequeueAfterClaimFailure,
        ] {
            assert!(kind.wire_name().is_none());
            assert!(kind.as_str().starts_with(INTERNAL_EVENT_PREFIX));
            assert_eq!(TaskEventKind::parse(kind.as_str()).unwrap(), kind);
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_string_form() {
        for kind in TaskEventKind::ALL {
            assert_eq!(TaskEventKind::parse(kind.as_str()).unwrap(), kind);
            // serde 形态必须与 as_str 完全一致（否则线上前缀就漂了）。
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
            assert_eq!(
                serde_json::from_str::<TaskEventKind>(&serde_json::to_string(&kind).unwrap())
                    .unwrap(),
                kind
            );
        }
    }

    #[test]
    fn unknown_names_are_rejected_loudly() {
        for bogus in [
            "queued",
            "task:Queued",
            "task:",
            "",
            "task:reclaim",
            "reclaim",
        ] {
            assert_eq!(
                TaskEventKind::parse(bogus).unwrap_err(),
                TaskError::UnknownEvent {
                    got: bogus.to_owned()
                }
            );
            assert!(serde_json::from_str::<TaskEventKind>(&format!("\"{bogus}\"")).is_err());
        }
    }
}
