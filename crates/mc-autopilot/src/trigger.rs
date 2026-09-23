//! autopilot trigger（`autopilot_trigger`）+ **cron 解析的唯一落点**。
//!
//! - **写者**：M5-3。
//! - **上游**：`CreateAutopilotTrigger`202 / `createWebhookTriggerWithMintedToken`56 /
//!   `isAllowedWebhookProvider`9 / `UpdateAutopilotTrigger`172 / `DeleteAutopilotTrigger`74 /
//!   `RotateAutopilotTriggerWebhookToken`69 / `SetAutopilotTriggerSigningSecret`65 +
//!   `computeNextRun`75 + `webhookPathForToken`4 + `service/cron.go`138（`NextOccurrenceAfterUTC` /
//!   `NextOccurrencesAfterUTC`）。
//! - **cron**：手写 5 字段解析器（`Minute|Hour|Dom|Month|Dow`，**无秒**），
//!   `dom` 与 `dow` 同时受限时按 Vixie **OR**；无下次触发返回 `None`（不是 `Err`）。
//!   为什么不引 `cron` crate：见 `src/lib.rs` 的选型记录（5 字段被拒 / 星期编号 1-based / AND 语义）。
//! - **`Timezone`**：本文件持有 `Timezone` newtype + IANA 校验（`chrono_tz::Tz::from_str`），
//!   对应上游 `resolveAutopilotTriggerTimezone`29。之所以不在 `mc-core`：见 `src/lib.rs` 的偏离说明。
//!   `autopilot_trigger.timezone` 是 `TEXT NULL DEFAULT 'UTC'`，`issue_wakeup.timezone` 是
//!   `TEXT NOT NULL DEFAULT 'UTC'` ⇒ 两边的 NULL 语义不同，别共用同一个解包。
//! - **token 形态**：`createWebhookTriggerWithMintedToken`(56) 生成 token + 唯一性冲突重试，
//!   路径形态由 `webhookPathForToken`(4) 定 ⇒ 必须与 M5-5 的 ingress 路径参数逐字一致。
//! - **`provider` 只有 `{generic,github}`**；`created_by_type` / `published_by_*` **没有 CHECK**
//!   （约定 `member|agent`）⇒ 解码时按开放字符串处理。
