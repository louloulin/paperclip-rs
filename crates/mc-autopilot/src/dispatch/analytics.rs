//! 派发/运行的分析位（analytics）。
//!
//! - **写者**：M5-4。
//! - **上游**：`service/autopilot.go` 的分析 92 行。
//! - **归属说明**：`docs/44` §1.2 已判定 M5 的 "analytics" 落在 quota/usage（M5-1）与调度 job
//!   （M5-8）上 ⇒ **不建 `mc-analytics` crate**；本文件只放派发路径上必须记的那几个计数/时间戳。
