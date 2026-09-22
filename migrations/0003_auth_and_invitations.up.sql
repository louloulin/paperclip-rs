-- 0003_auth_and_invitations.up.sql
--
-- M1 增量（来自 LUM-1335 `feat/multica-rs-m1` @ 3ad402e 的 `0002_auth_and_invitations`，
-- 由 M1-D / LUM-1347 集成时**重新编号为 0003**：`0002` 已被 B 切片的
-- `0002_pat_revoked_at.up.sql` 占用，mc-migrate 按文件名排序，0003 在 0002 之后执行）。
--
-- 内容：
--   - workspace_share_link：可分享邀请链接（与 workspace_invitation 并存）
--   - workspace_invitation：补状态机字段（本仓仅用 accepted_at / revoked_at，
--     新增列保留给 M2+ 与上游对齐）
--   - personal_access_token：补 token_prefix（UI 展示用；本仓 M1 写 token_last4）
--   - verification_code：常用查询索引
--
-- 兼容：全部 IF NOT EXISTS / 新增列显式 DEFAULT，0001 已有数据不会失败。
--
-- 与 LUM-1335 原迁移的**有意偏差**（详见 LUM-1347 集成报告）：
--   1. 不建 `idx_invitation_unique_pending`（`(workspace_id, email) WHERE status='pending'`
--      的部分唯一索引）：本仓 invitation 仓储走 `accepted_at` / `revoked_at` 状态机，
--      **从不写 `status`**，所有行恒为默认 'pending' → 该约束会永久阻止同一 email 的
--      合法重邀请（revoke 后无法再邀）。等 M2+ 真正引入 status 写入时再补。
--   2. 不建 `idx_invitation_user_pending`：`invitee_user_id` 本仓从不写入，索引无意义。
--   3. 去掉原迁移里的 `UPDATE personal_access_token SET token_prefix = ... SUBSTRING(...)`
--      回填语句：`SUBSTRING(token_last4 FROM 1 FOR 0)` 恒返回空串，是 no-op。

-- ----------------------------------------------------------------------------
-- workspace_share_link
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS workspace_share_link (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    code          TEXT NOT NULL,
    created_by    UUID NOT NULL REFERENCES "user"(id),
    role          TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('admin', 'member')),
    expires_at    TIMESTAMPTZ,
    max_uses      INT,
    use_count     INT NOT NULL DEFAULT 0,
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 每个 workspace 同时只有一条 active share link（与 `ShareLinkRepo::create`
-- 在事务里先置旧 link `is_active = FALSE` 的做法对齐）。
CREATE UNIQUE INDEX IF NOT EXISTS idx_share_link_workspace_active
    ON workspace_share_link(workspace_id) WHERE is_active = TRUE;

-- 公开加入面用 code 查询（不是 id）。
CREATE UNIQUE INDEX IF NOT EXISTS idx_share_link_code
    ON workspace_share_link(code);

-- ----------------------------------------------------------------------------
-- workspace_invitation — status / invitee_user_id / declined_at 列
-- ----------------------------------------------------------------------------
ALTER TABLE workspace_invitation
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'declined', 'revoked')),
    ADD COLUMN IF NOT EXISTS invitee_user_id UUID REFERENCES "user"(id),
    ADD COLUMN IF NOT EXISTS declined_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS revoked_at_explicit TIMESTAMPTZ;

-- 工作空间维度列表（`InvitationRepo::list_for_workspace`）。
CREATE INDEX IF NOT EXISTS idx_invitation_workspace_status
    ON workspace_invitation(workspace_id, status);

-- 按 email 反查（`InvitationRepo::list_for_user_email`）。
CREATE INDEX IF NOT EXISTS idx_invitation_email_pending
    ON workspace_invitation(email) WHERE status = 'pending';

-- ----------------------------------------------------------------------------
-- personal_access_token — token_prefix（UI 展示）+ user 维度索引
-- ----------------------------------------------------------------------------
ALTER TABLE personal_access_token
    ADD COLUMN IF NOT EXISTS token_prefix TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_pat_user_active
    ON personal_access_token(user_id);

-- ----------------------------------------------------------------------------
-- verification_code — 未消费记录的快速查询
-- ----------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_verification_code_email_purpose
    ON verification_code(email, purpose)
    WHERE consumed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_verification_code_user
    ON verification_code(user_id)
    WHERE consumed_at IS NULL;
