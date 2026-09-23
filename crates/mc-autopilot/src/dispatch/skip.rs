//! 跳过派发的判定与记账。
//!
//! - **写者**：M5-4。
//! - **上游**：`shouldSkipDispatch`98 + `handleDispatchSkip`33 + `recordSkippedRun`58。
//! - **这是「重复抑制 / 并发策略」的真值来源**：上游对应 ⑨ 里
//!   `TestDispatchAutopilotForPlanIsIdempotent` 那一族。
//! - **口径修正（必须按本行，不要按旧桩）**：`autopilot` 表里**没有** `concurrency_policy`
//!   （`043` 已 DROP）、也**没有** `priority`（`058` 已 DROP）⇒ 上游那两个枚举的落地形态要在本文件
//!   按 `90e0bdf` 的实际代码重新推（`docs/44` §5.1 的字段表列它们是对的、列**表列**是错的）。
//! - **跳过也落 run**：跳过不是「无记录」——`recordSkippedRun`(58) 写一行 `status = 'skipped'`。
