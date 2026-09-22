-- 539_status_and_role_vocabulary.up.sql —— 把上游「状态类别」与「角色」词表 CHECK
-- 放宽为「上游词表 ∪ 本仓词表」的并集（与 538 同一套路，只是对象不是主体类型）。
--
-- 1) `issue_status.category`：上游 `unstarted|started|done|closed`（4 值，见
--    `339_seed_issue_status_catalog.up.sql`），本仓只有 `open|closed` 二分
--    （`mc-core::StatusCategory`，`mc-repos/src/issue_status.rs::category_str` 写 `open`/`closed`，
--    `parse_category` 只认这 4 个拼写）。本仓的 `open` 覆盖上游 `unstarted`+`started`，
--    是**有意的粗粒度**：`GET /api/issue-statuses` 回显 `category`，映射到 4 值会改变
--    REST 契约，故取并集而不是映射。
--
-- 2) 角色：本仓 `WorkspaceRole = owner|admin|member|guest`（`mc-core/src/workspace.rs`），
--    且 `routes/invitations.rs::parse_role_or_default` 与 `share_link.rs`
--    （`.bind(input.role.as_str())`）会把整个本仓角色写入。上游
--    `workspace_invitation_role_check` / `workspace_share_link_role_check` 只有 `admin|member`，
--    `member_role_check` 只有 `owner|admin|member` —— 本仓的 `guest`（以及邀请/分享链接上的
--    `owner`）在上游没有对应取值，属于**本仓独有语义**，故取并集。
--    注意：本仓不写 `owner` 邀请/分享链接（只在 `member` 表里出现 owner），
--    取并集只是让 CHECK 与 `WorkspaceRole` 定义域一致。
--
-- 只放宽「允许集合」，不放宽可空性、不删约束；逐条登记在
-- `contracts/schema-deviations.tsv`（类别 `differs`，承接 LUM-1387）。
--
-- 幂等：`DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT` 可重复执行（失败文件会整份重跑）。

ALTER TABLE issue_status DROP CONSTRAINT IF EXISTS issue_status_category_check;
ALTER TABLE issue_status ADD CONSTRAINT issue_status_category_check CHECK (category = ANY (ARRAY['open', 'unstarted', 'started', 'done', 'closed']));

ALTER TABLE workspace_invitation DROP CONSTRAINT IF EXISTS workspace_invitation_role_check;
ALTER TABLE workspace_invitation ADD CONSTRAINT workspace_invitation_role_check CHECK (role = ANY (ARRAY['owner', 'admin', 'member', 'guest']));

ALTER TABLE workspace_share_link DROP CONSTRAINT IF EXISTS workspace_share_link_role_check;
ALTER TABLE workspace_share_link ADD CONSTRAINT workspace_share_link_role_check CHECK (role = ANY (ARRAY['owner', 'admin', 'member', 'guest']));

ALTER TABLE member DROP CONSTRAINT IF EXISTS member_role_check;
ALTER TABLE member ADD CONSTRAINT member_role_check CHECK (role = ANY (ARRAY['owner', 'admin', 'member', 'guest']));
