//! Issue rewake throttle 业务服务（原 `pc-issue-rewake-throttle` 已下沉）。
//!
//! 对应 Node `services/issue-rewake-throttle.ts`（177 行 — PAP-13775）。
//!
//! 本 crate 提供：
//!
//! - **常量**：
//!   - `ISSUE_REWAKE_NO_PROGRESS_THRESHOLD` —— 触发节流的连续 no-progress 次数
//!   - `ISSUE_REWAKE_BASE_COOLDOWN_MS` —— 基础 cooldown（120s）
//!   - `ISSUE_REWAKE_MAX_COOLDOWN_MS` —— cooldown 上限（30min）
//!   - `ISSUE_REWAKE_LOOKBACK_MS` —— 历史回看窗口
//!   - `ISSUE_REWAKE_RUN_SAMPLE_LIMIT` —— 采样数
//!   - `THROTTLED_ISSUE_REWAKE_REASONS` —— 受节流的 wake reasons
//!   - `ISSUE_PROGRESS_ACTIVITY_ACTIONS` —— issue-visible progress 活动类型
//!   - `ISSUE_NEW_INPUT_ACTIVITY_ACTIONS` —— 新输入活动类型
//! - **纯函数**：
//!   - `is_throttle_candidate(input)` —— 候选判定
//!   - `compute_cooldown_ms(streak)` —— cooldown 计算
//!   - `evaluate_throttle(input)` —— 主决策
//! - **DTO / 决策类型**：`IssueRewakeCandidateInput` / `RecentIssueRunSample` /
//!   `IssueRewakeThrottleInput` / `IssueRewakeThrottleDecision`
//! - **Service 层 API**（`IssueRewakeThrottleService`）：封装 + Hook
//! - **Hook 系统**：`IssueRewakeThrottleHook` trait（4 回调）
//!
//! 设计原则：
//! - **高内聚**：所有 throttle 决策集中在本 crate。
//! - **低耦合**：上游 HTTP / heartbeat 只需调 service。
//! - **薄封装**：核心逻辑走 `service` 模块，本 crate 负责 Hook 集成。

mod hook;
mod service;
mod types;

pub use hook::{
    IssueRewakeThrottleHook, IssueRewakeThrottleHookEvent, NoopIssueRewakeThrottleHook,
    RecordingIssueRewakeThrottleHook,
};
pub use service::{
    compute_issue_rewake_cooldown_ms, evaluate_issue_rewake_throttle,
    is_throttle_candidate_issue_rewake, IssueRewakeThrottleService,
};
pub use types::{
    IssueRewakeCandidateInput, IssueRewakeThrottleDecision, IssueRewakeThrottleInput,
    RecentIssueRunSample, ISSUE_NEW_INPUT_ACTIVITY_ACTIONS, ISSUE_PROGRESS_ACTIVITY_ACTIONS,
    ISSUE_REWAKE_BASE_COOLDOWN_MS, ISSUE_REWAKE_LOOKBACK_MS, ISSUE_REWAKE_MAX_COOLDOWN_MS,
    ISSUE_REWAKE_NO_PROGRESS_THRESHOLD, ISSUE_REWAKE_RUN_SAMPLE_LIMIT,
    THROTTLED_ISSUE_REWAKE_REASONS,
};
