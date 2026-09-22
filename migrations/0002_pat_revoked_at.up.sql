-- M1-B (LUM-1345): PAT 撤销状态。
-- Issue 要求 `PatRepo::revoke(id)` 通过设置 `revoked_at` 实现，
-- 但 0001_init 中 personal_access_token 缺少该列（上游用 `revoked BOOLEAN`）。
-- 采用语义等价的 `revoked_at TIMESTAMPTZ`：NULL = 有效，非 NULL = 已撤销。
ALTER TABLE personal_access_token ADD COLUMN revoked_at TIMESTAMPTZ;
