//! `OnboardingRepo` —— onboarding 的 user 列读写（**写者 M9-3** / `docs/62` §3.3）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格（`crates/mc-repos/src/lib.rs` 的 `pub mod onboarding;`）。
//! M9-3 原地填充：它不该回来改 `lib.rs`（那是 anchor 冻结的共享文件）。
//!
//! # M9-3 要填什么
//!
//! 上游 `internal/handler/onboarding.go`（372 行）+ `onboarding_shim.go`（623 行）的
//! 仓储面，只碰 [`mc_core::onboarding::ONBOARDING_USER_COLUMNS`] 那 **5 个**列：
//!
//! | 列 | 迁移 | 本 Repo 的动作 |
//! | --- | --- | --- |
//! | `onboarded_at` | `050` | `complete` 的 `COALESCE(onboarded_at, now())`（**幂等**：重复调用保留第一次） |
//! | `onboarding_questionnaire` | `051` + `094` | `PATCH /api/me/onboarding` 的读改（v2 形状） |
//! | `cloud_waitlist_email` | `052` | `cloud-waitlist` 的**覆盖**写（重复调用覆盖 email + reason） |
//! | `cloud_waitlist_reason` | `052` | 同上（空串 ⇒ `NULL`） |
//! | `starter_content_state` | `054` + `095` | shim 的 `provision` 链读它决定要不要走 starter content |
//!
//! # 三条纪律（`docs/62` §9.7）
//!
//! 1. 🔴 **只读不改** `"user".onboarding_state`（**本地独有列**，
//!    `migrations/compat/537_local_only_columns.up.sql:43`）—— 既有 `crate::user` 在读它，
//!    本片不得把它变成第二处写者；
//! 2. **`cloud-waitlist` 的对齐断言必须直读列**（`SELECT cloud_waitlist_email,
//!    cloud_waitlist_reason FROM "user" WHERE id = $1`）—— 走 API 回显有「handler 自己拼出来」
//!    的假绿风险；
//! 3. **`OnboardingProfile` 的默认值不是"全 `all`"**：未答过问卷 ⇒
//!    `onboarding_questionnaire = '{}'`（列默认），读回来是
//!    [`mc_core::onboarding::QuestionnaireAnswers::default`]。
//!
//! # 与既有实现的交集（**禁改**清单，`docs/62` §9.7 的表）
//!
//! `crates/mc-chat/src/onboarding.rs`、`crates/mc-repos/src/chat_task/onboarding.rs`、
//! `crates/mc-http/src/routes/chat/task/dispatch.rs`、`crates/mc-repos/src/user.rs`
//! —— 四处**只读**。

use mc_db::Db;

use crate::RepoWithDb;

/// onboarding 的 user 列读写（**M9-3 填充**）。
#[derive(Clone)]
pub struct OnboardingRepo {
    db: Db,
}

impl OnboardingRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for OnboardingRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
