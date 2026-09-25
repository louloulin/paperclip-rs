//! M8-5（`LUM-1802`）的宿主用例：装配判据（零数据库）+ **存储端口的真库用例**。
//!
//! # 跑法
//!
//! ```text
//! cargo test -p mc-server                        # 只跑装配判据（不需要库）
//! MULTICA_TEST_DATABASE_URL=postgres://… \
//!   cargo test -p mc-server -- --ignored --test-threads=1
//! ```
//!
//! 前置：`MULTICA_DATABASE_URL=… cargo run -p mc-migrate -- run --dir migrations`。
//! 门禁 ⑥（`bash scripts/gates.sh --with-db`）会带上 `-p mc-server` 与 `--ignored`。
//!
//! # 为什么种子是裸 SQL
//!
//! 与 `apps/mc-server/src/scheduler/tests.rs` 同判：`apps/mc-server` 是**纯二进制**（没有 lib
//! target），`mc-http/tests/**/support.rs` 那份 `pub(crate)` 夹具导不进来 ⇒ 这里自带最小夹具
//! （`workspace` + `github_pull_request` 两条 INSERT）。
//!
//! # 清理的坑（本仓实测）
//!
//! `github_pull_request_check_run`（迁移 `222`）**没有**外键，删 PR 行**不会**级联清掉它
//! ⇒ [`cleanup`] 必须显式删。`github_pull_request` 有 `workspace_id → workspace(id)` 的
//! `ON DELETE CASCADE`，所以删 workspace 能把 PR 行带走。

use std::time::Duration as StdDuration;

use sqlx::postgres::PgPool;
use sqlx::Row as _;
use uuid::Uuid;

use mc_core::id::Id;
use mc_http::state::integrations::GithubKeys;
use mc_vcs_github::ghsnapshot::refresh::{Address, PrRowRef, PrSnapshot, SnapshotStore as _};
use mc_vcs_github::ghsnapshot::snapshot::SnapshotCheck;

use super::{start_with, McSnapshotStore, HOST_POOL_MAX_CONNECTIONS};

// ---------------------------------------------------------------------------
// 装配判据（零数据库）
// ---------------------------------------------------------------------------

fn keys() -> GithubKeys {
    GithubKeys::from_env_with(|name| match name {
        "GITHUB_APP_ID" => Some("123".to_string()),
        "GITHUB_APP_PRIVATE_KEY" => Some("-----BEGIN PRIVATE KEY-----\nx\n".to_string()),
        "GITHUB_WEBHOOK_SECRET" => Some("shh".to_string()),
        _ => None,
    })
}

/// 无 App 凭据 ⇒ 不装配、不报错（**正常**路径；「能连接」与「能浏览仓库」是两个判据）。
#[test]
fn no_app_keys_means_nothing_is_assembled() {
    let handles = start_with(&GithubKeys::default(), Some(BOGUS_URL.to_string()));
    assert!(!handles.is_app_configured());
    assert!(!handles.is_wired());
    assert!(handles.pr_refresh().is_none());
}

/// 有凭据但**没有库 URL** ⇒ 明说未接线（不假装、不 panic、不起 worker）。
#[test]
fn app_keys_without_a_database_url_report_unwired() {
    let handles = start_with(&keys(), None);
    assert!(handles.is_app_configured());
    assert!(!handles.is_wired());
    assert!(handles.pr_refresh().is_none());
}

/// 有凭据 + 有库 URL ⇒ **真的接线**：worker 起来了、端口注入槽拿到了句柄
/// （webhook / 页面访问两条入队路径因此活了）、停机是干净的。
///
/// ⚠️ 本用例用一条**懒拨号**的不可达 URL：`Db::connect_lazy` 不会立刻连接，而用例不发起
/// 任何查询 ⇒ 不需要真库。这也正是生产路径的形状（池按需拨号）。
#[tokio::test]
async fn app_keys_with_a_url_wire_the_host_and_shutdown_is_clean() {
    let handles = start_with(&keys(), Some(BOGUS_URL.to_string()));
    assert!(handles.is_app_configured());
    assert!(handles.is_wired());
    let manager = handles.pr_refresh().expect("宿主必须已装配");
    assert!(manager.enabled());

    // 端口注入槽：`mc-http` 侧读到的就是这一个句柄。
    let port = mc_http::routes::github::webhook::pr_refresh_port();
    assert!(port.enabled(), "注入槽必须拿到已启用的端口");

    // 入队是**非阻塞**的（不摸数据库）。
    let request = mc_vcs_github::port::PrRefreshRequest {
        workspace_id: Id::new(),
        repo_owner: "acme".into(),
        repo_name: "api".into(),
        pr_number: 1,
        head_sha: None,
        reason: mc_vcs_github::port::RefreshReason::Webhook,
    };
    port.enqueue(request);

    handles.shutdown().await;
    mc_http::routes::github::webhook::reset_pr_refresh_port();
    assert!(!mc_http::routes::github::webhook::pr_refresh_port().enabled());
}

// 池上限（偏离 D5 的代价）钉在一个常量上，便于 M8-7 收口 —— 它是 `const`，不需要用例。
const _: () = assert!(
    HOST_POOL_MAX_CONNECTIONS > 0 && HOST_POOL_MAX_CONNECTIONS <= 16,
    "本宿主自建的池必须小"
);

/// SQL 常量的**承重不变式**（防「顺手改坏」）：守卫、批替换、sweep 的三个条件。
#[test]
fn store_sql_keeps_its_load_bearing_invariants() {
    let update = super::UPDATE_SNAPSHOT;
    assert!(update.contains("AND head_sha = $4"), "head-SHA 守卫不能丢");
    assert!(
        update.contains("snapshot_head_sha = $4"),
        "守卫与写入必须是同一个值"
    );
    assert!(super::DELETE_CHECK_RUNS.contains("WHERE pr_id = $1"));
    assert!(super::INSERT_CHECK_RUN.contains(
        "(pr_id, head_sha, ordinal, name, status, conclusion, details_url, is_status_context)"
    ));
    let sweep = super::LIST_STALE_UNDECIDED;
    assert!(sweep.contains("state IN ('open', 'draft')"), "只扫开着的");
    assert!(sweep.contains("cr.status <> 'completed'"), "未决判据不能丢");
    assert!(sweep.contains("DESC"), "游标之后的行必须排前");
    assert!(super::RESOLVE_INSTALLATION.contains("workspace_id = $1"));
}

/// 不可达但**语法合法**的库 URL（懒拨号 ⇒ 用例不会真的连它）。
const BOGUS_URL: &str = "postgres://mc_m8_5:unused@127.0.0.1:1/definitely_absent";

// ---------------------------------------------------------------------------
// 真库用例（`#[ignore]`；门禁 ⑥ 带 `--ignored` 跑）
// ---------------------------------------------------------------------------

/// 测试库（没有 URL 就显式失败，不静默跳过 —— 仓库统一口径）。
async fn pool() -> PgPool {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    PgPool::connect(&url).await.expect("connect test db")
}

/// 一个隔离的 workspace（结束时连同它的一切一起删）。
async fn seed_workspace(pool: &PgPool) -> Uuid {
    sqlx::query("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
        .bind("itest-m8-5-ws")
        .bind(format!("itest-m8-5-{}", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("insert workspace")
        .get("id")
}

/// 一行 `github_pull_request`（只给非空列 —— 其余全是 `DEFAULT` / 可空）。
#[allow(clippy::too_many_arguments)]
async fn seed_pr(
    pool: &PgPool,
    workspace: Uuid,
    installation_id: i64,
    number: i32,
    state: &str,
    head_sha: &str,
    fetched_at: Option<&str>,
    mergeable: Option<&str>,
    rollup: Option<&str>,
) -> Uuid {
    sqlx::query(
        "INSERT INTO github_pull_request \
           (workspace_id, installation_id, repo_owner, repo_name, pr_number, title, state, \
            html_url, pr_created_at, pr_updated_at, head_sha, mergeable_state, \
            snapshot_fetched_at, api_mergeable, checks_rollup_state) \
         VALUES ($1, $2, 'acme', 'api', $3, 't', $4, 'https://example.invalid/pr', now(), now(), \
                 $5, NULL, $6::timestamptz, $7, $8) \
         RETURNING id",
    )
    .bind(workspace)
    .bind(installation_id)
    .bind(number)
    .bind(state)
    .bind(head_sha)
    .bind(fetched_at)
    .bind(mergeable)
    .bind(rollup)
    .fetch_one(pool)
    .await
    .expect("insert pull request")
    .get("id")
}

/// `github_pull_request_check_run` **没有外键** ⇒ 必须显式删（见模块头）。
async fn cleanup(pool: &PgPool, workspace: Uuid) {
    let _ = sqlx::query(
        "DELETE FROM github_pull_request_check_run WHERE pr_id IN \
         (SELECT id FROM github_pull_request WHERE workspace_id = $1)",
    )
    .bind(workspace)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM github_pull_request WHERE workspace_id = $1")
        .bind(workspace)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace)
        .execute(pool)
        .await;
}

fn check(
    name: &str,
    status: &str,
    conclusion: Option<&str>,
    is_status_context: bool,
) -> SnapshotCheck {
    SnapshotCheck {
        name: name.into(),
        status: status.into(),
        conclusion: conclusion.map(str::to_string),
        details_url: Some(format!("https://example.invalid/{name}")),
        is_status_context,
    }
}

fn snapshot(head_sha: &str, checks: Vec<SnapshotCheck>) -> PrSnapshot {
    PrSnapshot {
        head_sha: head_sha.into(),
        mergeable: Some("MERGEABLE".into()),
        merge_state_status: Some("CLEAN".into()),
        rollup_state: Some("SUCCESS".into()),
        has_checks: true,
        checks,
    }
}

/// `(api_mergeable, api_merge_state_status, checks_rollup_state, snapshot_head_sha, 快照时刻)`。
async fn snapshot_columns(
    pool: &PgPool,
    pr_id: Uuid,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<i64>,
) {
    let row = sqlx::query(
        "SELECT api_mergeable, api_merge_state_status, checks_rollup_state, snapshot_head_sha, \
                (EXTRACT(EPOCH FROM snapshot_fetched_at))::BIGINT AS fetched \
         FROM github_pull_request WHERE id = $1",
    )
    .bind(pr_id)
    .fetch_one(pool)
    .await
    .expect("reload snapshot columns");
    (
        row.get("api_mergeable"),
        row.get("api_merge_state_status"),
        row.get("checks_rollup_state"),
        row.get("snapshot_head_sha"),
        row.get("fetched"),
    )
}

async fn check_run_rows(
    pool: &PgPool,
    pr_id: Uuid,
) -> Vec<(i32, String, String, Option<String>, bool)> {
    sqlx::query(
        "SELECT ordinal, name, status, conclusion, is_status_context \
         FROM github_pull_request_check_run WHERE pr_id = $1 ORDER BY ordinal",
    )
    .bind(pr_id)
    .fetch_all(pool)
    .await
    .expect("select check runs")
    .into_iter()
    .map(|row| {
        (
            row.get("ordinal"),
            row.get("name"),
            row.get("status"),
            row.get("conclusion"),
            row.get("is_status_context"),
        )
    })
    .collect()
}

/// **head-SHA 守卫 + 原子批次替换**（上游验收判据 1 的「写成功」一半）：四列 + 快照头 +
/// 快照时刻都落库，逐 check 行按 `ordinal` 全插。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn apply_snapshot_writes_guarded_columns_and_check_rows() {
    let pool = pool().await;
    let workspace = seed_workspace(&pool).await;
    let pr = seed_pr(&pool, workspace, 55, 1, "open", "sha-A", None, None, None).await;
    let store = McSnapshotStore::from_pool(pool.clone());

    let applied = store
        .apply_snapshot(
            Id(pr),
            &snapshot(
                "sha-A",
                vec![
                    check("backend", "completed", Some("failure"), false),
                    check("vercel", "completed", Some("success"), true),
                ],
            ),
            1_700_000_000,
        )
        .await
        .expect("apply");
    assert!(applied, "head 相同 ⇒ 该写");

    let columns = snapshot_columns(&pool, pr).await;
    assert_eq!(columns.0.as_deref(), Some("MERGEABLE"));
    assert_eq!(columns.1.as_deref(), Some("CLEAN"));
    assert_eq!(columns.2.as_deref(), Some("SUCCESS"));
    assert_eq!(columns.3, "sha-A");
    assert_eq!(
        columns.4,
        Some(1_700_000_000),
        "写的是注入的时刻，不是库时钟"
    );

    let rows = check_run_rows(&pool, pr).await;
    assert_eq!(rows.len(), 2, "两条 context 各一行");
    assert_eq!(
        rows[0],
        (
            0,
            "backend".into(),
            "completed".into(),
            Some("failure".into()),
            false
        )
    );
    assert_eq!(
        rows[1],
        (
            1,
            "vercel".into(),
            "completed".into(),
            Some("success".into()),
            true
        )
    );

    cleanup(&pool, workspace).await;
}

/// **head-SHA 守卫的「作废」一半**：head 前进 ⇒ 整条响应丢弃（含逐 check 行），
/// 新 head 的快照才做批次替换（2 行 → 1 行）。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn advanced_head_discards_the_response_and_batches_replace() {
    let pool = pool().await;
    let workspace = seed_workspace(&pool).await;
    let pr = seed_pr(&pool, workspace, 55, 1, "open", "sha-A", None, None, None).await;
    let store = McSnapshotStore::from_pool(pool.clone());
    let two_checks = vec![
        check("backend", "completed", Some("failure"), false),
        check("vercel", "completed", Some("success"), true),
    ];
    assert!(store
        .apply_snapshot(Id(pr), &snapshot("sha-A", two_checks), 1_700_000_000)
        .await
        .expect("apply"));

    // head 前进（模拟 webhook 推来的新 commit）⇒ 旧 head 的快照**一行都不写**。
    sqlx::query("UPDATE github_pull_request SET head_sha = 'sha-B' WHERE id = $1")
        .bind(pr)
        .execute(&pool)
        .await
        .expect("advance head");
    let stale = store
        .apply_snapshot(
            Id(pr),
            &snapshot(
                "sha-A",
                vec![check("ci", "completed", Some("success"), false)],
            ),
            1_700_000_100,
        )
        .await
        .expect("apply stale");
    assert!(!stale, "head 已前进 ⇒ 整批作废");
    assert_eq!(
        check_run_rows(&pool, pr).await.len(),
        2,
        "逐 check 行一行都没动"
    );
    assert_eq!(snapshot_columns(&pool, pr).await.3, "sha-A", "快照列也不动");

    // 新 head 的快照：批次替换（旧的 2 行被 1 行取代）。
    let fresh = store
        .apply_snapshot(
            Id(pr),
            &snapshot(
                "sha-B",
                vec![check("ci", "completed", Some("success"), false)],
            ),
            1_700_000_200,
        )
        .await
        .expect("apply fresh");
    assert!(fresh);
    let rows = check_run_rows(&pool, pr).await;
    assert_eq!(rows.len(), 1, "全删全插（批次替换）");
    assert_eq!(rows[0].1, "ci");
    assert_eq!(snapshot_columns(&pool, pr).await.4, Some(1_700_000_200));

    cleanup(&pool, workspace).await;
}

/// `statusCheckRollup == null` ⇒ `checks_rollup_state` 写 `NULL`、逐 check 行为零
/// （「没有 check」**不**等于「通过」）。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn no_checks_snapshot_writes_null_rollup_and_no_rows() {
    let pool = pool().await;
    let workspace = seed_workspace(&pool).await;
    let pr = seed_pr(&pool, workspace, 55, 7, "open", "sha-A", None, None, None).await;
    let store = McSnapshotStore::from_pool(pool.clone());

    let snapshot = PrSnapshot {
        head_sha: "sha-A".into(),
        mergeable: Some("CONFLICTING".into()),
        merge_state_status: Some("DIRTY".into()),
        rollup_state: None,
        has_checks: false,
        checks: Vec::new(),
    };
    assert!(store
        .apply_snapshot(Id(pr), &snapshot, 1_700_000_000)
        .await
        .expect("apply"));
    let row = sqlx::query(
        "SELECT checks_rollup_state, api_mergeable FROM github_pull_request WHERE id = $1",
    )
    .bind(pr)
    .fetch_one(&pool)
    .await
    .expect("reload");
    assert_eq!(row.get::<Option<String>, _>("checks_rollup_state"), None);
    assert_eq!(
        row.get::<Option<String>, _>("api_mergeable").as_deref(),
        Some("CONFLICTING")
    );
    assert!(check_run_rows(&pool, pr).await.is_empty());

    cleanup(&pool, workspace).await;
}

/// `list_rows` 按地址**跨 workspace 扇出**（同一个 installation 绑两个 workspace 各镜像一行）。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn list_rows_fans_out_across_workspaces() {
    let pool = pool().await;
    let first = seed_workspace(&pool).await;
    let second = seed_workspace(&pool).await;
    seed_pr(&pool, first, 55, 9, "open", "sha-A", None, None, None).await;
    seed_pr(&pool, second, 55, 9, "draft", "sha-A", None, None, None).await;
    // 另一个号：不该出现在这次查询里。
    seed_pr(&pool, second, 55, 10, "open", "sha-A", None, None, None).await;
    let store = McSnapshotStore::from_pool(pool.clone());

    let rows = store
        .list_rows(&Address::new(55, "acme", "api", 9))
        .await
        .expect("list rows");
    assert_eq!(rows.len(), 2, "两个 workspace 各一行");
    let mut states: Vec<&str> = rows.iter().map(|row| row.state.as_str()).collect();
    states.sort_unstable();
    assert_eq!(states, vec!["draft", "open"]);
    assert!(rows.iter().all(PrRowRef::is_open_or_draft));

    assert!(
        store
            .list_rows(&Address::new(55, "acme", "api", 11))
            .await
            .expect("list rows")
            .is_empty(),
        "没有这一号 ⇒ 空集（不是错误）"
    );

    cleanup(&pool, first).await;
    cleanup(&pool, second).await;
}

/// `resolve_installation`：端口的请求键 → 地址（顺带带回快照时刻，view TTL 用）。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn resolve_installation_returns_the_installation_and_fetch_time() {
    let pool = pool().await;
    let workspace = seed_workspace(&pool).await;
    seed_pr(
        &pool,
        workspace,
        55,
        12,
        "open",
        "sha-A",
        Some("2023-11-14T22:13:20Z"),
        Some("MERGEABLE"),
        Some("SUCCESS"),
    )
    .await;
    let store = McSnapshotStore::from_pool(pool.clone());

    let target = store
        .resolve_installation(Id(workspace), "acme", "api", 12)
        .await
        .expect("resolve")
        .expect("a mirrored row exists");
    assert_eq!(target.installation_id, 55);
    assert_eq!(target.snapshot_fetched_at, Some(1_700_000_000));

    assert!(
        store
            .resolve_installation(Id(workspace), "acme", "api", 13)
            .await
            .expect("resolve")
            .is_none(),
        "没有镜像行 ⇒ None（不是错误）"
    );

    cleanup(&pool, workspace).await;
}

/// sweep 的六行夹具（每行一个「该不该入选」的理由）：
///
/// | # | 形状 | 判定 |
/// | --- | --- | --- |
/// | ① | open + 新鲜 | 排除（未过期） |
/// | ② | open + 陈旧 + 可合并性 + 终态 rollup + 无未完成 check | 排除（**已决**） |
/// | ③ | open + 陈旧 + rollup 在跑 | 入选 |
/// | ④ | 另一个 installation + draft + 从未抓过 | 入选 |
/// | ⑤ | merged + 陈旧 | 排除（关掉了） |
/// | ⑥ | open + 陈旧 + 一条未完成的 check_run | 入选 |
async fn seed_sweep_rows(pool: &PgPool, workspace: Uuid) -> Address {
    let fresh = "2999-01-01T00:00:00Z";
    let stale = "2000-01-01T00:00:00Z";
    seed_pr(
        pool,
        workspace,
        55,
        1,
        "open",
        "a",
        Some(fresh),
        Some("MERGEABLE"),
        Some("SUCCESS"),
    )
    .await;
    let decided = seed_pr(
        pool,
        workspace,
        55,
        2,
        "open",
        "a",
        Some(stale),
        Some("MERGEABLE"),
        Some("SUCCESS"),
    )
    .await;
    seed_pr(
        pool,
        workspace,
        55,
        3,
        "open",
        "a",
        Some(stale),
        Some("MERGEABLE"),
        Some("PENDING"),
    )
    .await;
    seed_pr(pool, workspace, 56, 4, "draft", "a", None, None, None).await;
    seed_pr(
        pool,
        workspace,
        55,
        5,
        "merged",
        "a",
        Some(stale),
        None,
        None,
    )
    .await;
    let running = seed_pr(
        pool,
        workspace,
        55,
        6,
        "open",
        "a",
        Some(stale),
        Some("MERGEABLE"),
        Some("SUCCESS"),
    )
    .await;

    // ② 的「已决」靠这条判定走默认分支（rollup 终态 + 无未完成 check）⇒ 显式钉住列值。
    sqlx::query("UPDATE github_pull_request SET api_mergeable = 'MERGEABLE', checks_rollup_state = 'SUCCESS' WHERE id = $1")
        .bind(decided)
        .execute(pool)
        .await
        .expect("decided pr");
    sqlx::query(
        "INSERT INTO github_pull_request_check_run \
           (pr_id, head_sha, ordinal, name, status, conclusion, details_url, is_status_context) \
         VALUES ($1, 'a', 0, 'ci', 'in_progress', NULL, NULL, false)",
    )
    .bind(running)
    .execute(pool)
    .await
    .expect("running check run");

    // 语序即期望语序（installation, owner, repo, number 升序）。
    Address::new(55, "acme", "api", 3)
}

/// 陈旧阈值的筛法：**只**回 open/draft、陈旧**且**未决的地址。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn sweep_excludes_fresh_decided_and_closed_rows() {
    let pool = pool().await;
    let workspace = seed_workspace(&pool).await;
    let first = seed_sweep_rows(&pool, workspace).await;
    let store = McSnapshotStore::from_pool(pool.clone());

    let rows = store
        .list_stale_undecided(1_700_000_000, &Address::default(), 200)
        .await
        .expect("sweep");
    assert_eq!(
        rows,
        vec![
            first,
            Address::new(55, "acme", "api", 6),
            Address::new(56, "acme", "api", 4),
        ],
        "只回陈旧且未决的 open/draft 地址"
    );

    cleanup(&pool, workspace).await;
}

/// sweep 的**有界 + 游标回绕**：`max_rows` 截断；游标之后的行排前、其余回绕到尾部
/// （否则一个反复失败的首页会永远占着 LIMIT）。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn sweep_cursor_rotates_and_is_bounded() {
    let pool = pool().await;
    let workspace = seed_workspace(&pool).await;
    let first = seed_sweep_rows(&pool, workspace).await;
    let store = McSnapshotStore::from_pool(pool.clone());

    let one = store
        .list_stale_undecided(1_700_000_000, &Address::default(), 1)
        .await
        .expect("sweep");
    assert_eq!(one, vec![first.clone()], "max_rows 有界");

    let rotated = store
        .list_stale_undecided(1_700_000_000, &first, 200)
        .await
        .expect("sweep");
    assert_eq!(
        rotated,
        vec![
            Address::new(55, "acme", "api", 6),
            Address::new(56, "acme", "api", 4),
            first,
        ],
        "游标之后的行排前、其余回绕到尾部"
    );

    cleanup(&pool, workspace).await;
    // 停一下再退出，避免 `pool` 的懒回收把清理语句挂住（与 M5-9 用例同款收尾）。
    tokio::time::sleep(StdDuration::from_millis(1)).await;
}
