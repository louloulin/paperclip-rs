//! R630 集成测试：user_profile.rs `UserProfileRepo::load` 回归保护。
//!
//! 覆盖 user-profiles 模块核心契约：
//! - resolve_company_user：slug 模糊匹配（name / email-prefix / email / principal_id）
//! - window_stats：3 个 windows × issue/comment/activity/cost 聚合
//! - daily_stats：14 天 daily breakdown
//! - top_agents / top_providers：cost 聚合
//!
//! 测试环境：复用 paperclip_repos DB，每测试 unique IDs。


use pc_db::Db;
use pc_repos::user_profile::UserProfileRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect paperclip_repos")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r630-{tag}-{}", Uuid::new_v4().simple()))
        .bind(format!("R630{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_fake_user(db: &Db, tag: &str) -> String {
    let id = format!("u_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, false, now(), now())",
    )
    .bind(&id)
    .bind(format!("r630-user-{tag}-{}", Uuid::new_v4().simple()))
    .bind(format!("r630-{}-{}@test.local", tag, Uuid::new_v4().simple()))
    .execute(db.pool())
    .await
    .expect("insert user");
    id
}

async fn add_owner_member(db: &Db, company_id: Uuid, user_id: &str) {
    sqlx::query(
        "INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, 'user', $2, 'active', 'owner')",
    )
    .bind(company_id)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert owner membership");
}

/// 1. resolve_company_user 通过 principal_id slug 找到 user
#[tokio::test(flavor = "current_thread")]
async fn resolve_via_principal_id_slug() {
    let db = db().await;
    let company_id = insert_company(&db, "via-pid").await;
    let user_id = insert_fake_user(&db, "via-pid").await;
    add_owner_member(&db, company_id, &user_id).await;

    let slug = user_id.clone();
    let profile = UserProfileRepo::new(&db)
        .load(company_id, &slug)
        .await
        .expect("load")
        .expect("profile found");

    assert_eq!(profile.user.id, user_id);
    assert!(!profile.user.slug.is_empty());
    assert_eq!(profile.user.membership_role.as_deref(), Some("owner"));
}

/// 2. window_stats 三个 windows 都返回（含 key/labels/0 默认值）
#[tokio::test(flavor = "current_thread")]
async fn window_stats_returns_three_windows() {
    let db = db().await;
    let company_id = insert_company(&db, "win").await;
    let user_id = insert_fake_user(&db, "win").await;
    add_owner_member(&db, company_id, &user_id).await;

    let profile = UserProfileRepo::new(&db)
        .load(company_id, &user_id)
        .await
        .expect("load")
        .expect("profile");

    assert_eq!(profile.stats.len(), 3);
    let keys: Vec<&str> = profile.stats.iter().map(|w| w.key.as_str()).collect();
    assert_eq!(keys, vec!["last7", "last30", "all"]);

    // 0 数据 → 全 0
    for w in &profile.stats {
        assert_eq!(w.touched_issues, 0);
        assert_eq!(w.completed_issues, 0);
        assert_eq!(w.cost_cents, 0);
    }
}

/// 3. daily 总是返回 14 天（含全 0）
#[tokio::test(flavor = "current_thread")]
async fn daily_returns_14_days() {
    let db = db().await;
    let company_id = insert_company(&db, "daily").await;
    let user_id = insert_fake_user(&db, "daily").await;
    add_owner_member(&db, company_id, &user_id).await;

    let profile = UserProfileRepo::new(&db)
        .load(company_id, &user_id)
        .await
        .expect("load")
        .expect("profile");

    assert_eq!(profile.daily.len(), 14);
    // daily 按 ISO date 升序
    let mut sorted = profile.daily.clone();
    sorted.sort_by(|a, b| a.date.cmp(&b.date));
    for (i, p) in sorted.iter().enumerate() {
        assert_eq!(p.date, profile.daily[i].date);
    }
}

/// 4. 创建 1 个 issue 后，touched_issues 至少 1（involvement = created_by_user_id）
#[tokio::test(flavor = "current_thread")]
async fn created_issue_increments_touched() {
    let db = db().await;
    let company_id = insert_company(&db, "touch").await;
    let user_id = insert_fake_user(&db, "touch").await;
    add_owner_member(&db, company_id, &user_id).await;

    // seed 1 个 issue（created_by_user_id = user）
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, created_by_user_id, updated_at) \
         VALUES ($1, $2, $3, 'r630-issue', 'todo', 'normal', $4, now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("R630-T{}", &issue_id.simple().to_string()[..4]))
    .bind(&user_id)
    .execute(db.pool())
    .await
    .expect("insert issue");

    let profile = UserProfileRepo::new(&db)
        .load(company_id, &user_id)
        .await
        .expect("load")
        .expect("profile");

    // all-time 一定有 touched; last7 / last30 也包含（issue 是现在创建的）
    let all_time = profile.stats.iter().find(|w| w.key == "all").expect("all");
    assert!(
        all_time.created_issues >= 1,
        "expected created_issues >= 1, got {}",
        all_time.created_issues
    );
    assert!(
        all_time.touched_issues >= 1,
        "expected touched_issues >= 1, got {}",
        all_time.touched_issues
    );

    // recent_issues 应包含
    assert!(
        profile.recent_issues.iter().any(|i| i.id == issue_id),
        "issue should be in recent_issues"
    );
}

/// 5. 不存在的 company → None
#[tokio::test(flavor = "current_thread")]
async fn load_returns_none_for_missing_company() {
    let db = db().await;
    let missing_company = Uuid::new_v4();
    let result = UserProfileRepo::new(&db)
        .load(missing_company, "u_nonexistent")
        .await
        .expect("load");
    assert!(result.is_none());
}

/// 6. empty slug → resolve 失败 → None
#[tokio::test(flavor = "current_thread")]
async fn empty_slug_returns_none() {
    let db = db().await;
    let company_id = insert_company(&db, "empty").await;
    let result = UserProfileRepo::new(&db).load(company_id, "").await.expect("load");
    assert!(result.is_none());
}
