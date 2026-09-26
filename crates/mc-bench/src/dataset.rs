//! 固定数据集 —— **播种是 bench 的一部分**。
//!
//! 确定性 UUID + 确定性时间戳 ⇒ 换台机器也是同一份数据；全部落在一个固定 id 的 workspace 下，
//! 跑完即删（同一个库还要跑门 ⑥/⑧ 的真库用例）。
//!
//! ## 🔴 每次 target 都**重建**（不「有就复用」）
//!
//! `cargo bench -p mc-bench` 依次跑三个 target（`facets` → `inbox_cursor` → `issue_list`，字母序），
//! 每个 target 跑完清库。若播种做成「计数对得上就复用」，则**第一个 target 测的可能是别人留下的
//! 旧库**，而它后面的 target 测的是刚播种的新库 —— 两种物理形态（死元组 / 页填充 / 计划统计的
//! 新鲜度）不同，同一个 case 的 p95 实测差出 **29%**（`facets/table_facets_all5_with_total`：
//! 复用旧库 17.417ms vs 重建 12.371ms，见 `docs/32` §41.4）。那样的基线不可复现，相对阈值
//! 就变成了噪声闸门。
//!
//! 所以 [`prepare_dataset`] 每次都是 `reset → VACUUM (ANALYZE) → seed → ANALYZE`：
//! 三个 target 都从**同一形态**（无死元组、统计新鲜）出发，读数才可跨 run 比对。
//!
//! ## 🔴 库必须「干净」（[`assert_clean_database`]）
//!
//! 只有「无死元组」还不够：**别的 workspace 的行**会通过规划器统计把读数带跑。`inbox_item` 在
//! 本 dataset 里只有 1 个 `workspace_id` / 1 个 `recipient_id`（`n_distinct = 1`）；门 ⑥ 的 e2e
//! 用例在同一张表里留下几十行别的 workspace 后，`n_distinct` 变大，PostgreSQL 对**泛化计划**的
//! 估行从 ~5000 掉到 **7** ⇒ 改选 `idx_inbox_recipient_archived_created` 索引扇出，实际返回 5000 行：
//! `archived_page_first` 的 p95 从 **12.4ms 涨到 21.8ms**（buffers 850 → 30 511，实测见 `docs/32`
//! §42.4）。这是**两个不同的计划**，不是噪声 —— 基线一旦建在脏库上，相对阈值就比不了。
//! 所以 [`prepare_dataset`] 先跑一道闸：三个受测表里除本 dataset 外**不得有任何行**，否则 panic
//! 并给重建库的命令（与「缺库即红」同一款纪律 —— 默默拿一份不可复现的读数才是真正的失败）。

use serde::Serialize;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::config::{COMMENTS, INBOX_ITEMS, ISSUES, PROJECT_ID, USER_ID, WORKSPACE_ID};

/// 数据集描述（落进报告的 `dataset` 段）。
#[derive(Debug, Clone, Serialize)]
pub struct Dataset {
    /// 数据集的 workspace / user（见 [`WORKSPACE_ID`] / [`USER_ID`]）。
    pub workspace_id: Uuid,
    /// 见 `workspace_id`。
    pub user_id: Uuid,
    /// issue / comment / 归档 inbox item 行数（见 [`ISSUES`]）。
    pub issues: usize,
    /// 见 `issues`。
    pub comments: usize,
    /// 见 `issues`。
    pub inbox_items: usize,
}

impl Dataset {
    /// 默认规模的描述（不触库）。
    pub fn spec() -> Self {
        Self {
            workspace_id: WORKSPACE_ID,
            user_id: USER_ID,
            issues: ISSUES,
            comments: COMMENTS,
            inbox_items: INBOX_ITEMS,
        }
    }
}

/// 数据集的三个受测表（`VACUUM (ANALYZE)` 的作用面；`workspace` / `project` / `user` 各自只有
/// 一行，不值得多三条预算）。
const DATASET_TABLES: [&str; 3] = ["issue", "comment", "inbox_item"];

/// 建好固定数据集（**每次都重建**，理由见模块头）：
/// `assert_clean_database → reset → VACUUM (ANALYZE) → seed → ANALYZE`。
///
/// `VACUUM (ANALYZE)` 走 [`sqlx::raw_sql`]（简单协议）：`VACUUM` 是 utility 语句，不能进扩展协议
/// 的 prepared statement。它做两件事：回收上一个 target 留下的死元组、把统计刷到刚播种后的状态 ——
/// 缺后者时计划取决于 autovacuum/autoanalyze 有没有碰巧跑过，读数会抖。
pub async fn prepare_dataset(pool: &PgPool) -> Dataset {
    assert_clean_database(pool).await;
    reset_dataset(pool).await;
    vacuum_analyze(pool).await;
    seed_dataset(pool).await;
    vacuum_analyze(pool).await;
    Dataset::spec()
}

/// 🔴 闸门：三个受测表里除本 dataset 外**不得有任何行**（理由与代价见模块头）。
///
/// 判据是行数而不是「表为空」：本 dataset 的行此时可能还在（上一轮没清成功），
/// 那不影响规划器统计 —— 只要它们同属一个 `workspace_id` / `recipient_id`。
pub async fn assert_clean_database(pool: &PgPool) {
    let mut foreign: Vec<String> = Vec::new();
    for table in DATASET_TABLES {
        let sql = format!("SELECT COUNT(*)::bigint FROM {table} WHERE workspace_id <> $1");
        let rows: i64 = sqlx::query_scalar(&sql)
            .bind(WORKSPACE_ID)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("cannot count foreign rows in {table}: {e}"));
        if rows > 0 {
            foreign.push(format!("{table} = {rows}"));
        }
    }
    assert!(
        foreign.is_empty(),
        "mc-bench 需要一个**干净**的库：issue / comment / inbox_item 里除本 dataset（workspace \
         {WORKSPACE_ID}）外不得有别的 workspace 的行，实际发现 [{}]。\n  \
         外来的行会通过规划器统计把读数带跑：`inbox_item` 的 n_distinct 变大 ⇒ 泛化计划改选索引\
         扇出 ⇒ archived_page 的 p95 从 12.4ms 变成 21.8ms（两个不同的计划，见 docs/32 §42.4）。\n  \
         重建库后重跑（例）：\n    \
         sudo -u postgres psql -c 'DROP DATABASE <db>' -c 'CREATE DATABASE <db> OWNER <role>'\n    \
         MULTICA_DATABASE_URL=… cargo run -p mc-migrate -- run --dir migrations",
        foreign.join(", ")
    );
}

/// 对 [`DATASET_TABLES`] 逐表 `VACUUM (ANALYZE)`。
async fn vacuum_analyze(pool: &PgPool) {
    for table in DATASET_TABLES {
        sqlx::raw_sql(&format!("VACUUM (ANALYZE) {table}"))
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("cannot VACUUM (ANALYZE) {table}: {e}"));
    }
}

/// 删掉数据集（`workspace` 级联到 issue / comment / `inbox_item` / project）。
pub async fn reset_dataset(pool: &PgPool) {
    for (sql, id) in [
        ("DELETE FROM workspace WHERE id = $1", WORKSPACE_ID),
        ("DELETE FROM \"user\" WHERE id = $1", USER_ID),
    ] {
        sqlx::query(sql)
            .bind(id)
            .execute(pool)
            .await
            .expect("cannot reset the bench dataset");
    }
}

async fn seed_dataset(pool: &PgPool) {
    let (issues, comments, inbox) = (
        i32::try_from(ISSUES).expect("ISSUES fits in i32"),
        i32::try_from(COMMENTS).expect("COMMENTS fits in i32"),
        i32::try_from(INBOX_ITEMS).expect("INBOX_ITEMS fits in i32"),
    );
    // 每条 `SEED_*` 的绑定顺序在语句里逐条写死；绑定个数由各自的 `$n` 决定。
    macro_rules! seed {
        ($sql:expr $(, $bind:expr)* $(,)?) => {
            sqlx::query($sql) $(.bind($bind))* .execute(pool).await.expect("seed statement failed")
        };
    }
    seed!(SEED_USER, USER_ID);
    seed!(SEED_WORKSPACE, WORKSPACE_ID, issues);
    seed!(SEED_PROJECT, PROJECT_ID, WORKSPACE_ID);
    seed!(SEED_ISSUES, WORKSPACE_ID, PROJECT_ID, issues);
    seed!(SEED_COMMENTS, WORKSPACE_ID, USER_ID, comments);
    seed!(SEED_INBOX, WORKSPACE_ID, USER_ID, inbox);
}

/// bench 结束后清库（**必须**做：同一个库还要跑门 ⑥/⑧ 的真库用例）。
pub async fn cleanup_dataset(pool: &PgPool) {
    reset_dataset(pool).await;
}

const SEED_USER: &str =
    "INSERT INTO \"user\"(id, name, email) VALUES ($1, 'mc-bench', 'mc-bench@multica.local')";

const SEED_WORKSPACE: &str = "INSERT INTO workspace(id, name, slug, issue_prefix, issue_counter) \
     VALUES ($1, 'mc-bench', 'mc-bench', 'MCB', $2)";

const SEED_PROJECT: &str =
    "INSERT INTO project(id, workspace_id, title) VALUES ($1, $2, 'mc-bench project')";

/// `$1` = workspace、`$2` = project、`$3` = 行数。
///
/// 分布是**确定性**的：status 7 值轮转、priority 5 值轮转、`assignee` 每 3 行空一次（4 个 id）、
/// `creator` 2 值轮转、`project` 每 5 行空一次；`updated_at` 用 `g + g % 97` ⇒ 排序有并列，
/// 顺带压到 `ORDER BY updated_at DESC, number DESC` 的 tie-break。`number` 必须唯一
/// （`uq_issue_workspace_number`）。
const SEED_ISSUES: &str = "INSERT INTO issue \
     (id, workspace_id, number, identifier, title, description, status, priority, \
      assignee_type, assignee_id, creator_type, creator_id, project_id, position, created_at, updated_at) \
     SELECT ('b0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            $1, g + 1, 'MCB-' || (g + 1), 'mc-bench issue ' || g, 'mc-bench issue body ' || g, \
            CASE g % 7 WHEN 0 THEN 'backlog' WHEN 1 THEN 'todo' WHEN 2 THEN 'in_progress' \
                       WHEN 3 THEN 'in_review' WHEN 4 THEN 'done' WHEN 5 THEN 'blocked' \
                       ELSE 'cancelled' END, \
            CASE g % 5 WHEN 0 THEN 'urgent' WHEN 1 THEN 'high' WHEN 2 THEN 'medium' \
                       WHEN 3 THEN 'low' ELSE 'none' END, \
            CASE WHEN g % 3 = 0 THEN NULL ELSE 'user' END, \
            CASE WHEN g % 3 = 0 THEN NULL \
                 ELSE ('b0e0c4a0-0000-4000-8000-' || lpad(to_hex(100 + g % 4), 12, '0'))::uuid END, \
            'user', \
            ('b0e0c4a0-0000-4000-8000-' || lpad(to_hex(200 + g % 2), 12, '0'))::uuid, \
            CASE WHEN g % 5 = 0 THEN NULL ELSE $2::uuid END, \
            g::float8, \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second'), \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + ((g + g % 97) * INTERVAL '1 second') \
     FROM generate_series(0, $3 - 1) AS g";

/// `$1` = workspace、`$2` = author、`$3` = 行数。
const SEED_COMMENTS: &str = "INSERT INTO comment \
     (id, issue_id, workspace_id, author_type, author_id, content, type, created_at, updated_at) \
     SELECT ('c0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            ('b0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            $1, 'user', $2, 'mc-bench comment ' || g, 'comment', \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second'), \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second') \
     FROM generate_series(0, $3 - 1) AS g";

/// `$1` = workspace、`$2` = recipient / actor、`$3` = 行数。
///
/// 全部 `archived_at IS NOT NULL` 且每个 issue 组只有一行 ⇒ 归档视图的 `newest` CTE 恰好
/// 命中 5000 个组；`read_at` 只写一半（`read` 布尔与之双写，与 `mc_repos::inbox` 的口径一致）。
const SEED_INBOX: &str = "INSERT INTO inbox_item \
     (id, workspace_id, recipient_type, recipient_id, type, severity, issue_id, title, body, \
      read, archived, read_at, archived_at, actor_type, actor_id, created_at) \
     SELECT ('d0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            $1, 'user', $2, 'issue_assigned', 'info', \
            ('b0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            'mc-bench inbox ' || g, 'mc-bench inbox body ' || g, \
            (g % 2 = 0), TRUE, \
            CASE WHEN g % 2 = 0 \
                 THEN TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second') \
                 ELSE NULL END, \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second'), \
            'user', $2, \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second') \
     FROM generate_series(0, $3 - 1) AS g";
