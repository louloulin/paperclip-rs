-- 537_local_only_columns.up.sql —— 上游完全没有建模、而本地 SQL 仍在读写的列。
--
-- 只列「代码真的用到」的列（每条都在 `contracts/schema-deviations.tsv` 登记，归属 LUM-1387）；
-- 上游已有等价概念的地方一律改 SQL 对齐上游列名，而不是在这里造重复列：
--   * `comment.body`          → 上游 `content`（本地 SQL 写 `content`，`content AS body` 读出）
--   * `inbox_item.user_id`    → 上游 `recipient_id` + `recipient_type`
--   * `inbox_item.category`   → 上游 `type`
--   * `workspace_invitation.email`            → 上游 `invitee_email`
--   * `workspace_invitation.invited_by_user_id` → 上游 `inviter_id`
--   * `verification_code.code_hash`           → 上游 `code`
--
-- 上游**没有**的语义（本文件负责）：
--   * `issue.identifier` / `status_name` / `origin` / `origin_task_id` / `source_context_id`
--     —— 本地把 issue 的人可读编号、自定义状态显示名、来源与来源任务直接落盘；
--     上游用 `workspace.issue_prefix` + `issue.number`，来源用 `origin_type` / `origin_id`（语义不同，不映射）。
--   * `comment.routing_escalation`：本地评论的路由升级标记。
--   * `inbox_item.read_at` / `archived_at`：上游只有 `read` / `archived` 布尔；本地 API 要时间线，
--     两个布尔由代码双写（见 `mc-repos/src/inbox.rs`）。
--   * `member.updated_at`：上游 member 无 updated_at（本地成员管理接口读它）。
--   * `workspace.archived_at`：上游 workspace 无归档概念（本地归档/列表过滤用它）。
--   * `"user".email_verified_at` / `onboarding_state`：上游未建模（本地引导流程读它们）。
--   * `agent.starter_prompts` / `runtime_profile.*` / `autopilot.*` / `skill.*` / `squad.*` 等
--     上游没有的本地列**不在这里补**——本地没有任何 SQL 触碰它们，留空即可（迁移只建上游 schema）。
--
-- 幂等：全部 `ADD COLUMN IF NOT EXISTS`。

ALTER TABLE issue ADD COLUMN IF NOT EXISTS identifier TEXT NOT NULL DEFAULT '';
ALTER TABLE issue ADD COLUMN IF NOT EXISTS status_name TEXT;
ALTER TABLE issue ADD COLUMN IF NOT EXISTS origin TEXT;
ALTER TABLE issue ADD COLUMN IF NOT EXISTS origin_task_id UUID;
ALTER TABLE issue ADD COLUMN IF NOT EXISTS source_context_id UUID;

ALTER TABLE comment ADD COLUMN IF NOT EXISTS routing_escalation TEXT;

ALTER TABLE inbox_item ADD COLUMN IF NOT EXISTS read_at TIMESTAMPTZ;
ALTER TABLE inbox_item ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

ALTER TABLE member ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE workspace ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

ALTER TABLE "user" ADD COLUMN IF NOT EXISTS email_verified_at TIMESTAMPTZ;
ALTER TABLE "user" ADD COLUMN IF NOT EXISTS onboarding_state JSONB;
