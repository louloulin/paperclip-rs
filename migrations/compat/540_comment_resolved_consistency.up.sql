-- 540_comment_resolved_consistency.up.sql —— 把上游 `comment_resolved_consistency`
-- 的三元组耦合（`resolved_at` / `resolved_by_type` / `resolved_by_id` 要么全空要么全满）
-- 降为「主体对要么全空要么全满」。
--
-- 背景：本仓的 resolve/unresolve 是**无主体**的布尔开关 ——
-- `CommentRepo::resolve(id)` / `unresolve(id)`（`crates/mc-repos/src/comment.rs`）
-- 只翻转 `resolved_at`，仓库 API 里没有 resolver 参数，DTO 也不回显
-- `resolved_by_type` / `resolved_by_id`（`CommentDto` 只带 `resolved_at`）。
-- 上游 `ResolveComment` 由调用方传入 resolver，所以三元组在本仓没有第三个值可写。
--
-- 放宽后的语义：
--   * `(resolved_by_type IS NULL) = (resolved_by_id IS NULL)` —— 主体对仍然保持一致（写一半会被拒）；
--   * `resolved_at` 不再强制要求主体，允许「已解决但未记录 resolver」。
-- 这是**弱化**了一条上游不变量，属于已登记 deviation（`contracts/schema-deviations.tsv`，
-- 类别 `differs`，承接 LUM-1387）。若后续把 resolver 接进 `CommentRepo`（需要动
-- `routes/comments.rs` 的调用签名），应回填主体并删除本条 compat。
--
-- 幂等：`DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT` 可重复执行（失败文件会整份重跑）。

ALTER TABLE comment DROP CONSTRAINT IF EXISTS comment_resolved_consistency;
ALTER TABLE comment ADD CONSTRAINT comment_resolved_consistency CHECK (((resolved_by_type IS NULL) = (resolved_by_id IS NULL)));
