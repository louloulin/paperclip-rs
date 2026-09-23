//! issue wakeup 调度 job：把到期的 `issue_wakeup`（`kind ∈ {at, every, cron}`）变成任务。
//!
//! - **写者**：M5-8。
//! - **上游**：`scheduler/jobs_issue_wakeup.go`21（22）—— 只是一个薄壳，真正的活在
//!   `service/issue_wakeup.go` 的 `Tick`35（本地在 `mc_autopilot::wakeup::service`）。
//! - **薄壳的纪律**：本文件只做「取到期行 → 交给 wakeup service → 推进 `next_fire_at`」，
//!   不要把校验 / 合并 / 派发逻辑抄进来（那会让 M5-6 与 M5-8 出现两份真值）。
//! - **`kind = event` 的 wakeup 不由本 job 驱动**（事件来自 ingress / 证据捕获）。
