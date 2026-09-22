-- M2 anchor scaffold（M1-D / LUM-1347）：reactions + subscribers 三张表。
--
-- 上游对应：multica `server/migrations/026_comment_reactions.up.sql`、
--           `027_issue_reactions.up.sql`、`015_issue_subscriber.up.sql`。
-- 本仓 `0001_init.up.sql` 缺这三张表，M2-A（issue reactions）、M2-B（comment
-- reactions）、M2-C（subscribers）都会用到——所以在 M1-D 的 anchor scaffold 里
-- 一次建好，避免两个 M2 分支各自加同号迁移。
--
-- 编号说明：docs/09-M1-INTEGRATION.md §6 写的是 `0003_reactions_and_subscribers`，
-- 但 0003 已被 M1-D 的 cherry-pick 增量占用（`0003_auth_and_invitations`，
-- workspace_share_link / invitation 状态列 / PAT token_prefix），故顺延为 0004。
-- `mc-db` 的 migrator 按文件名升序执行，0004 会在 0003 之后运行。
--
-- 与上游的**有意偏离**（写代码前先读这条）：
--   上游 026/027 用 `actor_type TEXT CHECK IN ('member','agent')` + `actor_id UUID`；
--   本仓 0001 的同类列一律是 `TEXT CHECK IN ('user','agent',…)` + `actor_id TEXT`
--   （见 `comment.author_type` / `comment.author_id`、`issue.assignee_type`/`assignee_id`、
--   `inbox_item.actor_type`/`actor_id`）。这里跟随**本仓** 0001 的词汇表，
--   即人类主体写 `'user'`（不是 `'member'`），id 存 uuid 字符串。
--   M2 的 handler 直接复用 `comment.author_type` / `inbox_item.actor_type` 的
--   取值域，不需要 member↔user 映射。

-- ============================================================
-- 1. comment_reaction（上游 026）
-- ============================================================
CREATE TABLE comment_reaction (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    comment_id UUID NOT NULL REFERENCES comment(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'agent', 'system')),
    actor_id TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 同一个人对同一条评论的同一个 emoji 只能有一条（重复 POST 幂等）
    UNIQUE (comment_id, actor_type, actor_id, emoji)
);
CREATE INDEX comment_reaction_comment_idx ON comment_reaction(comment_id);

-- ============================================================
-- 2. issue_reaction（上游 027）
-- ============================================================
CREATE TABLE issue_reaction (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issue_id UUID NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'agent', 'system')),
    actor_id TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issue_id, actor_type, actor_id, emoji)
);
CREATE INDEX issue_reaction_issue_idx ON issue_reaction(issue_id);

-- ============================================================
-- 3. issue_subscriber（上游 015）
-- ============================================================
-- 订阅者既可能是人（member/user）也可能是 agent，故这里保留上游的 UUID 列
-- （本仓 `member.user_id` / `agent.id` 都是 UUID），不加外键——agent 与 user
-- 分属两张表，单一 FK 表达不了。
CREATE TABLE issue_subscriber (
    issue_id UUID NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    user_type TEXT NOT NULL CHECK (user_type IN ('user', 'agent')),
    user_id UUID NOT NULL,
    reason TEXT NOT NULL CHECK (
        reason IN ('creator', 'assignee', 'commenter', 'mentioned', 'manual')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, user_type, user_id)
);
CREATE INDEX issue_subscriber_user_idx ON issue_subscriber(user_type, user_id);
