//! hook 引擎与 job 的**写**侧：`plugin_hook_schedule` + `plugin_invocation`。
//!
//! - **写者**：M6-8（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/service/plugin_hook*` + `internal/handler/plugin.go` 的 hook 段与
//!   `scheduler` 侧的 plugin-hook job。
//! - **12 列**（`399`）：`id, installation_id, workspace_id, hook_key, cron_expression, timezone,
//!   generation, activated_at, next_run_at, enabled, created_at, updated_at`。
//! - **三条硬语义**：
//!   1. `generation`（UUID）是**换代令牌**：改 cron / 停用 / 重装都要换一代 —— 老一代排出来的
//!      调用落到 `plugin_invocation` 时必须被判无效（不能打到新配置上）；
//!   2. `plugin_invocation.trigger='schedule'` 是 `399` 补的第五态（`362` 只允许四态；
//!      `402` 才 `VALIDATE` 约束）—— 写这个取值是合法的，别按老 CHECK 去绕；
//!   3. `delivery_id`（可空，1..128）与 `planned_at` 是幂等/对账用的：同一次计划投递重复执行时
//!      用 `delivery_id` 去重，**不要**靠 `created_at` 猜。
//! - **并发**：`attempt 1..10`，重试预算写在列上；租约/去重不要自创，走
//!   `mc_scheduler` + `mc_repos::scheduler` 的 `sys_cron_executions`（M5-7 已落地）。
//! - **不做什么**：不做 cron 解析（`mc-scheduler` 的 spec）、不做 HTTP 签名（`mc-plugin-host`）。
//!
//! **状态：M6-8 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 300 行以内。
