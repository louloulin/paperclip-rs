//! `autopilot_trigger` 仓储。
//!
//! - **写者**：M5-3（**W**；`docs/44` §3.2）。M5-5 读（webhook ingress 按 token 解析 trigger）。
//! - **上游 SQL**：`db/queries/autopilot.sql` 的 trigger 查询。
//! - **要落的写点**：create / update / delete + `webhook_token` 轮换 + `signing_secret` 写入。
//! - **token 唯一性**：token 唯一冲突要能被上层识别并重试（`createWebhookTriggerWithMintedToken`56）
//!   ⇒ 错误映射要区分「唯一冲突」与其它 DB 错误。
//! - **列注意**：`kind ∈ {schedule, webhook, api}`、`provider ∈ {generic, github}`；
//!   `timezone TEXT NULL DEFAULT 'UTC'`（**可空**，与 `issue_wakeup.timezone` 的 NOT NULL 不同）；
//!   `created_by_*` / `published_by_*` **没有 CHECK**（约定 `member|agent`）。
