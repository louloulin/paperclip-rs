-- 538_actor_type_vocabulary.up.sql —— 把上游 7 条「主体类型词表」CHECK 放宽为
-- 「上游词表 ∪ 本仓词表」的并集。
--
-- 背景（docs/11-M2-ISSUE.md §5、docs/26-W0-SCHEMA-SWITCHOVER.md §4）：
-- 本仓 REST 契约里「人」写作 `user`（`mc-core::AssigneeType::User`、
-- `CommentAuthorType::User`、`creator_type = "user"`、`inbox_item.recipient_type = "user"`），
-- 且这个取值被 `crates/mc-http/tests/*` 断言（`author_type == "user"`、`creator_type == "user"`）。
-- 上游同一概念写作 `member`（`member` 表 + `*_type = 'member'`）。
-- 输入侧本仓已经把上游写法归一化掉（`routes/issues.rs::normalize_assignee_type`：
-- `member` → `user`），落库与回显统一 `user` —— 这是 M2 起就写进文档的**有意偏离**，
-- 该词汇贯穿 filter（`issue_table/sql.rs` 的 `assignee_type = ANY(...)`）、facet 值
-- （`issue_table/repo.rs` 的 `assignee_type || ':' || assignee_id`）与 DTO。
--
-- 因此这里**不放宽可空性、不删约束**，只把「允许集合」扩成并集：上游自己写的
-- `member` 仍然合法（未来若与上游数据互通不会撞 CHECK），本仓写入的 `user` 也合法。
-- 代价是数据库层同时接受两种词表：本仓的读/筛/分组只看 `user`，故**不要**在本地代码里
-- 写 `member`。方向是单向的（docs/26 §4 有说明），已逐条登记在
-- `contracts/schema-deviations.tsv`（类别 `differs`，承接 LUM-1387）。
--
-- 幂等：`DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT` 可重复执行（失败文件会整份重跑）。

ALTER TABLE comment DROP CONSTRAINT IF EXISTS comment_author_type_check;
ALTER TABLE comment ADD CONSTRAINT comment_author_type_check CHECK (author_type = ANY (ARRAY['user', 'member', 'agent', 'system', 'plugin', 'squad', 'autopilot']));

ALTER TABLE comment_reaction DROP CONSTRAINT IF EXISTS comment_reaction_actor_type_check;
ALTER TABLE comment_reaction ADD CONSTRAINT comment_reaction_actor_type_check CHECK (actor_type = ANY (ARRAY['user', 'member', 'agent']));

ALTER TABLE issue_reaction DROP CONSTRAINT IF EXISTS issue_reaction_actor_type_check;
ALTER TABLE issue_reaction ADD CONSTRAINT issue_reaction_actor_type_check CHECK (actor_type = ANY (ARRAY['user', 'member', 'agent']));

ALTER TABLE issue DROP CONSTRAINT IF EXISTS issue_assignee_type_check;
ALTER TABLE issue ADD CONSTRAINT issue_assignee_type_check CHECK (assignee_type = ANY (ARRAY['user', 'member', 'agent', 'squad', 'autopilot']));

ALTER TABLE issue DROP CONSTRAINT IF EXISTS issue_creator_type_check;
ALTER TABLE issue ADD CONSTRAINT issue_creator_type_check CHECK (creator_type = ANY (ARRAY['user', 'member', 'agent', 'system']));

ALTER TABLE inbox_item DROP CONSTRAINT IF EXISTS inbox_item_recipient_type_check;
ALTER TABLE inbox_item ADD CONSTRAINT inbox_item_recipient_type_check CHECK (recipient_type = ANY (ARRAY['user', 'member', 'agent']));

ALTER TABLE issue_subscriber DROP CONSTRAINT IF EXISTS issue_subscriber_user_type_check;
ALTER TABLE issue_subscriber ADD CONSTRAINT issue_subscriber_user_type_check CHECK (user_type = ANY (ARRAY['user', 'member', 'agent']));
