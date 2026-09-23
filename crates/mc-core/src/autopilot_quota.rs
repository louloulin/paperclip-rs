//! Autopilot 配额领域类型（M5-0 anchor **新建**）。
//!
//! 上游表是 `352_autopilot_quota_execution` 建的两张 + `448` 加的一列，
//! 索引/主键由 `353` / `354` / `355` / `358` / `359`（纯 DDL，无列变化）补上：
//!
//! | 实体 | 表 | 迁移 | 列数 |
//! | --- | --- | --- | ---: |
//! | [`QuotaPeriod`] | `autopilot_quota_period` | `352` + `448` | 9 |
//! | [`QuotaReservation`] | `autopilot_quota_reservation` | `352` | 11 |
//!
//! `352` 的设计原话值得抄进脑子：**「Limits and period boundaries are supplied by Cloud at
//! runtime; no commercial defaults or calendar assumptions live in this schema.」**
//! ⇒ 库里**没有**额度上限、也**没有**「一个月」这种日历假设：
//! `period_start` / `period_end` 由准入方每次传入，`autopilot_quota_period` 的 PK 就是
//! `(workspace_id, period_start, period_end)`——**不同周期口径可以并存**。
//! 这也是本文件里 [`QuotaUsage::limit`] 是 `Option` 的原因：上限不在库里，在 entitlement 面。
//!
//! # 关键约束（逐条都在真库快照里，M5-1 的实现必须与它们对齐）
//!
//! - `autopilot_quota_reservation.state ∈ {reserved,consumed,released}`；
//!   `uq_autopilot_quota_reservation_key` 是**部分唯一索引**
//!   `(workspace_id, period_start, period_end, idempotency_key) WHERE state <> 'released'`
//!   ⇒ 同一幂等键**只能有一条未释放**的预留；**释放后键可再用**（这是重试语义，不是漏洞）。
//! - `idx_autopilot_quota_reservation_state` 是**部分索引** `(state, created_at) WHERE state='reserved'`
//!   ⇒ 扫「过期未终结的预留」走它（M5-1 的回收逻辑）。
//! - `autopilot_quota_period.used_count >= 0` 与 `reserved_count >= 0` 是 CHECK；
//!   `blocked_counts` 是 `jsonb`（**键 = 拒绝原因码**，值是次数；原因码是自由文本，见下）。
//! - `autopilot_quota_period.rejection_notified_at`（`448`）：拒绝通知的**一次性**标记，
//!   避免每个被拒的 run 都重复通知。
//!
//! # 约定
//!
//! - `source`（`352`）与 `autopilot_run.reason_code` / `webhook_delivery.reason_code` 一样是
//!   **无 CHECK 的自由文本**：本文件不用封闭枚举表达，别在解码时当不可能事件处理。
//! - `policy_revision` / `subscription_version` 是 entitlement 面的版本号，由调用方传入；
//!   本仓**不解释**它们的语义，只负责原样存回（对账需要）。
//! - 预留的**消费/释放**必须落在 `autopilot_run` 的终态上：`autopilot_run.quota_reservation_id`
//!   是**无外键**的应用层引用（`352` 有意不加 FK），所以「run 结束但预留还挂着」是可能状态，
//!   需要 sweeper（M5-1）兜底。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// `autopilot_quota_reservation.state` ∈ `{reserved,consumed,released}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState {
    /// 已占位，尚未确认。（`idx_autopilot_quota_reservation_state` 只索引这一态。）
    Reserved,
    /// 已消费（run 真的跑了）。
    Consumed,
    /// 已释放（run 没跑 / 被跳过的 run 不计数）。**释放后幂等键可再次使用。**
    Released,
}

impl ReservationState {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Consumed => "consumed",
            Self::Released => "released",
        }
    }

    /// 终态判定：只有 `reserved` 还能被消费/释放。
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Consumed | Self::Released)
    }
}

/// `autopilot_quota_period` 行（9 列）——按**调用方给定的周期**做的耐久账。
///
/// 注意它**没有** `id`：主键是 `(workspace_id, period_start, period_end)`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaPeriod {
    /// 所属工作区（PK 的一部分）。
    pub workspace_id: Id,
    /// 周期开始（PK 的一部分）。
    pub period_start: Timestamp,
    /// 周期结束（PK 的一部分；`CHECK (period_start < period_end)`）。
    pub period_end: Timestamp,
    /// 已消费计数（`>= 0`）。
    pub used_count: i64,
    /// 已占位计数（`>= 0`；`used_count` 之外的在飞量）。
    pub reserved_count: i64,
    /// 拒绝计数：`jsonb`，键 = 拒绝原因码（自由文本），值 = 次数。
    pub blocked_counts: serde_json::Value,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
    /// 最近一次发送「已拒绝」通知的时间（`448`；一次性通知的标记）。
    pub rejection_notified_at: Option<Timestamp>,
}

/// `autopilot_quota_reservation` 行（11 列）——一次准入占位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaReservation {
    /// 主键。
    pub id: Id,
    /// 所属工作区（部分唯一索引的一部分）。
    pub workspace_id: Id,
    /// 周期开始（部分唯一索引的一部分）。
    pub period_start: Timestamp,
    /// 周期结束（部分唯一索引的一部分）。
    pub period_end: Timestamp,
    /// entitlement 面的策略版本（本仓不解释语义）。
    pub policy_revision: i64,
    /// entitlement 面的订阅版本（本仓不解释语义）。
    pub subscription_version: i64,
    /// 预留来源（**自由文本，无 CHECK**；上游写 run 的来源词，但别当封闭枚举）。
    pub source: String,
    /// 幂等键（`WHERE state <> 'released'` 下唯一；释放后可再用）。
    pub idempotency_key: String,
    /// 预留状态。
    pub state: ReservationState,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 终结时间（`consumed` / `released` 时有值）。
    pub finalized_at: Option<Timestamp>,
}

/// 配额用量**投影**（**不是表**）——`GET /api/autopilots/usage` 的读模型。
///
/// `limit` 来自 entitlement 面（`352` 原话：上限由 Cloud 运行时下发，库里没有），
/// 所以它是 `Option`：`None` = 当前拿不到额度定义（此时**不能**把用量当「已用满」）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaUsage {
    /// 所属工作区。
    pub workspace_id: Id,
    /// 周期开始。
    pub period_start: Timestamp,
    /// 周期结束。
    pub period_end: Timestamp,
    /// 额度上限（entitlement 面下发；`None` = 未知）。
    pub limit: Option<i64>,
    /// 已消费计数（来自 [`QuotaPeriod::used_count`]）。
    pub used_count: i64,
    /// 已占位计数（来自 [`QuotaPeriod::reserved_count`]）。
    pub reserved_count: i64,
    /// 拒绝计数明细（来自 [`QuotaPeriod::blocked_counts`]）。
    pub blocked_counts: serde_json::Value,
}

/// 准入决策（M5-1 `mc-autopilot::quota` 的返回值投影）。
///
/// 三个分支对应三种**可观察**的下游行为：
///
/// - [`QuotaDecision::Reserved`]：新占一个位，调用方接着建 `autopilot_run`
///   （把 `quota_reservation_id` 写进它）。
/// - [`QuotaDecision::Replayed`]：幂等命中——同一 `idempotency_key` 已有未释放的预留，
///   **复用**它，不要再建 run（否则一次请求会消耗两个额度）。
/// - [`QuotaDecision::Denied`]：不占位。`reason_code` 写进
///   `autopilot_quota_period.blocked_counts` 与 `autopilot_run.reason_code`；
///   是否发通知由 `rejection_notified_at` 决定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum QuotaDecision {
    /// 新预留。
    Reserved {
        /// 预留 id。
        reservation_id: Id,
    },
    /// 幂等命中，复用既有预留。
    Replayed {
        /// 既有预留 id。
        reservation_id: Id,
    },
    /// 拒绝（原因码是自由文本，无 CHECK）。
    Denied {
        /// 机器可读原因码。
        reason_code: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_strings_match_db_checks() {
        assert_eq!(ReservationState::Reserved.as_str(), "reserved");
        assert_eq!(ReservationState::Released.as_str(), "released");
        assert!(!ReservationState::Reserved.is_final());
        assert!(ReservationState::Consumed.is_final());
    }

    /// 决策枚举的 JSON 形状（HTTP 面要对齐，先钉住）。
    #[test]
    fn decision_serializes_with_tag() {
        let denied = QuotaDecision::Denied {
            reason_code: "quota_exceeded".to_string(),
        };
        let v = serde_json::to_value(&denied).expect("serialize");
        assert_eq!(v["decision"], "denied");
        assert_eq!(v["reason_code"], "quota_exceeded");
    }
}
