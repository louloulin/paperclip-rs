-- 535_pat_revoked_at.up.sql —— 复述已退役的 `0002_pat_revoked_at.up.sql`。
--
-- 上游 `personal_access_token` 用 `revoked BOOLEAN`（见 001_init），而本地 HTTP 契约
-- （`GET /api/pats`）要吐撤销**时间**，所以这里保留时间戳列：两边同时为真——写入路径是
-- 上游的 `revoked`，`revoked_at` 作为镜像给 API 读（见 `mc-repos/src/pat.rs`）。
--
-- 同样保留 0001 里上游没有的三个列：
--   * `token_last4`：本地 API 的展示片段（上游只有 `token_prefix`）；
--   * `scopes`：本地 PAT 作用域（上游未建模）；
--   * `token_prefix`：上游是 NOT NULL 且无默认值，本地 INSERT 从不写它。补默认值即可，
--     既不放松非空约束，也不用改 SQL；真实前缀由上游侧负责。
--
-- 幂等：`ADD COLUMN IF NOT EXISTS` + `SET DEFAULT` 可重复执行（失败文件会整份重跑）。

ALTER TABLE personal_access_token ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMPTZ;
ALTER TABLE personal_access_token ADD COLUMN IF NOT EXISTS token_last4 TEXT NOT NULL DEFAULT '';
ALTER TABLE personal_access_token ADD COLUMN IF NOT EXISTS scopes JSONB NOT NULL DEFAULT '[]'::jsonb;
ALTER TABLE personal_access_token ALTER COLUMN token_prefix SET DEFAULT '';

-- `issue_status.color`：上游是 NOT NULL 且无默认值，本地 `INSERT INTO issue_status`
-- （`mc-repos/src/issue_status.rs`、`mc-http/tests/inbox.rs` 等）从不写它。
-- 不能只补 `''`：上游同时有 `issue_status_color_check CHECK (color ~ '^#[0-9a-f]{6}$')`
-- ⇒ 默认值必须本身合法，否则省掉 color 的本地 INSERT 会直接撞 CHECK。这里取中性灰。
-- 同样不改任何 SQL、不放松非空约束、不动 CHECK。
ALTER TABLE issue_status ALTER COLUMN color SET DEFAULT '#6b7280';
