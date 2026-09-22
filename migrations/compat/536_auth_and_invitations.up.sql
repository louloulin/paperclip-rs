-- 536_auth_and_invitations.up.sql —— 复述已退役的 `0003_auth_and_invitations.up.sql` 中
-- 上游没有对应物的部分。
--
-- 上游已经覆盖的（`status` / `invitee_user_id` / `token_prefix` / `workspace_share_link` /
-- 各索引）不再复述，直接用上游版本；这里只补本地认证链路真正依赖、而上游未建模的三样：
--
--   * `workspace_invitation.token`：本地用一次性不透明 token 兑换邀请（上游用 id + 状态机）；
--   * `workspace_invitation.accepted_at` / `revoked_at`：本地 API 的幂等判据与时间戳镜像；
--   * `verification_code.purpose` / `consumed_at` / `user_id`：本地验证码区分用途（邮箱验证 /
--     重置密码 / 二次验证 / 邀请），上游只有 `used BOOLEAN`。`consumed_at` 与上游 `used`
--     双写，`used` 是权威（见 `mc-repos/src/verification_code.rs`）。
--
-- `verification_code.code_hash` 不在 compat 里：本地上游列名就是 `code`（纯文本），
-- 本地 SQL 已改为写 `code`、以 `code AS code_hash` 读出（本地只保存哈希）。

ALTER TABLE workspace_invitation ADD COLUMN IF NOT EXISTS token TEXT NOT NULL DEFAULT '';
ALTER TABLE workspace_invitation ADD COLUMN IF NOT EXISTS accepted_at TIMESTAMPTZ;
ALTER TABLE workspace_invitation ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMPTZ;

ALTER TABLE verification_code ADD COLUMN IF NOT EXISTS purpose TEXT NOT NULL DEFAULT '';
ALTER TABLE verification_code ADD COLUMN IF NOT EXISTS consumed_at TIMESTAMPTZ;
ALTER TABLE verification_code ADD COLUMN IF NOT EXISTS user_id UUID REFERENCES "user"(id) ON DELETE CASCADE;
