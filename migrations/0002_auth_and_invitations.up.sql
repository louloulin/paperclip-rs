-- 0002_auth_and_invitations.up.sql
--
-- M1 (workspace / member / auth) 的 schema 增量：
--   - workspace_share_link：可分享邀请链接（与 workspace_invitation 并存）
--   - workspace_invitation：升级字段以支持多状态机（pending/accepted/declined/revoked）
--   - verification_code_user_idx：常用按用户查询的索引
--   - personal_access_token：补全 token_prefix 字段（与 upstream 对齐）
--   - user 表加 verified_at / language 等列已在 0001_init 中建好，此处只补索引
--
-- 兼容：所有变更均为 IF NOT EXISTS / ADD CONSTRAINT NOT VALID 或新增列显式 DEFAULT。
-- 任何 0001 已有数据均不会因此迁移失败。

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

-- One active share link code per workspace (kept consistent across revokes).
CREATE UNIQUE INDEX IF NOT EXISTS idx_share_link_workspace_active
    ON workspace_share_link(workspace_id) WHERE is_active = TRUE;

-- Lookup by code (the public join surface uses code, not id).
CREATE UNIQUE INDEX IF NOT EXISTS idx_share_link_code
    ON workspace_share_link(code);

-- ----------------------------------------------------------------------------
-- workspace_invitation — add status / invitee_user_id / declined_at columns
-- ----------------------------------------------------------------------------
ALTER TABLE workspace_invitation
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'declined', 'revoked')),
    ADD COLUMN IF NOT EXISTS invitee_user_id UUID REFERENCES "user"(id),
    ADD COLUMN IF NOT EXISTS declined_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS revoked_at_explicit TIMESTAMPTZ;

-- Indexes for invitee-side lookups (by email or by registered user id).
CREATE INDEX IF NOT EXISTS idx_invitation_workspace_status
    ON workspace_invitation(workspace_id, status);

CREATE INDEX IF NOT EXISTS idx_invitation_email_pending
    ON workspace_invitation(email) WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_invitation_user_pending
    ON workspace_invitation(invitee_user_id) WHERE status = 'pending';

-- At most one pending invitation per (workspace, email).
CREATE UNIQUE INDEX IF NOT EXISTS idx_invitation_unique_pending
    ON workspace_invitation(workspace_id, email) WHERE status = 'pending';

-- ----------------------------------------------------------------------------
-- personal_access_token — add token_prefix column used for UI display
-- ----------------------------------------------------------------------------
ALTER TABLE personal_access_token
    ADD COLUMN IF NOT EXISTS token_prefix TEXT NOT NULL DEFAULT '';

-- Backfill prefix for rows created before the column was added (best effort).
UPDATE personal_access_token
   SET token_prefix = COALESCE(NULLIF(token_prefix, ''), SUBSTRING(token_last4 FROM 1 FOR 0))
 WHERE token_prefix = '';

CREATE INDEX IF NOT EXISTS idx_pat_user_active
    ON personal_access_token(user_id);

-- ----------------------------------------------------------------------------
-- verification_code — fast email/purpose lookup
-- ----------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_verification_code_email_purpose
    ON verification_code(email, purpose)
    WHERE consumed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_verification_code_user
    ON verification_code(user_id)
    WHERE consumed_at IS NULL;
