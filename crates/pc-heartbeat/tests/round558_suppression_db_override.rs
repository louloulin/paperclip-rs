//! R558: Heartbeat 抑制 DB override 完整装载链路集成测试。
//!
//! 验证 `build_suppression_inputs` 从 DB experimental 标志
//! (`enableWorktreeRunExecution`) 正确装载到 `SuppressionInputs.db_worktree_override_armed`,
//! 与 env vars 合并,并通过 `resolve_suppression` 产生正确的决策。
//!
//! 注意：使用 `jsonb_set` / `jsonb -` 显式赋值/删除,避免 `||` merge 在并发测试下产生
//! 读写竞争。

use pc_heartbeat::wake_dedup::{
    build_suppression_inputs, resolve_suppression, SuppressionReason,
};
use pc_repos::Db;
use std::collections::HashMap;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
const FLAG_KEY: &str = "enableWorktreeRunExecution";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

/// 显式设置 / 清除 experimental 标志。
/// - `value: Some(v)`: 用 `jsonb_set` 写入具体值
/// - `value: None`: 用 `experimental - 'flag'` 删除键
async fn set_flag(db: &Db, value: Option<bool>) {
    let _ = sqlx::query(
        "INSERT INTO instance_settings (singleton_key) VALUES ('default') \
         ON CONFLICT (singleton_key) DO NOTHING",
    )
    .execute(db.pool())
    .await
    .unwrap();
    match value {
        Some(b) => {
            let v = serde_json::json!(b);
            let sql = format!(
                "UPDATE instance_settings SET \
                 experimental = jsonb_set(experimental, '{{{FLAG_KEY}}}', $1::jsonb, true), \
                 updated_at = now() \
                 WHERE singleton_key = 'default'"
            );
            sqlx::query(&sql)
                .bind(&v)
                .execute(db.pool())
                .await
                .unwrap();
        }
        None => {
            sqlx::query(
                "UPDATE instance_settings SET \
                 experimental = experimental - $1, \
                 updated_at = now() \
                 WHERE singleton_key = 'default'",
            )
            .bind(FLAG_KEY)
            .execute(db.pool())
            .await
            .unwrap();
        }
    }
}

#[tokio::test]
async fn r558_db_override_armed_lifts_worktree_suppression() {
    let db = connect().await;
    set_flag(&db, Some(true)).await;
    let env: HashMap<String, String> =
        HashMap::from([("PAPERCLIP_IN_WORKTREE".into(), "true".into())]);
    let inputs = build_suppression_inputs(&db, &env).await;
    assert!(
        inputs.db_worktree_override_armed,
        "DB flag=true should arm override"
    );
    let decision = resolve_suppression(&inputs);
    assert!(
        !decision.suppressed,
        "worktree suppression should be lifted by DB override"
    );
    assert_eq!(decision.reason, SuppressionReason::None);
    set_flag(&db, None).await;
}

#[tokio::test]
async fn r558_db_override_disabled_keeps_worktree_suppression() {
    let db = connect().await;
    set_flag(&db, Some(false)).await;
    let env: HashMap<String, String> =
        HashMap::from([("PAPERCLIP_IN_WORKTREE".into(), "true".into())]);
    let inputs = build_suppression_inputs(&db, &env).await;
    assert!(
        !inputs.db_worktree_override_armed,
        "DB flag=false should not arm override"
    );
    let decision = resolve_suppression(&inputs);
    assert!(
        decision.suppressed,
        "worktree should be suppressed without override"
    );
    assert_eq!(decision.reason, SuppressionReason::WorktreeInstance);
    set_flag(&db, None).await;
}

#[tokio::test]
async fn r558_db_override_missing_treated_as_false() {
    let db = connect().await;
    set_flag(&db, None).await;
    let env: HashMap<String, String> =
        HashMap::from([("PAPERCLIP_IN_WORKTREE".into(), "true".into())]);
    let inputs = build_suppression_inputs(&db, &env).await;
    assert!(!inputs.db_worktree_override_armed);
    let decision = resolve_suppression(&inputs);
    assert!(decision.suppressed);
    assert_eq!(decision.reason, SuppressionReason::WorktreeInstance);
}

#[tokio::test]
async fn r558_db_restore_in_progress_overrides_db_worktree_override() {
    let db = connect().await;
    set_flag(&db, Some(true)).await;
    let env: HashMap<String, String> = HashMap::from([
        ("PAPERCLIP_IN_WORKTREE".into(), "true".into()),
        (
            "PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS".into(),
            "true".into(),
        ),
    ]);
    let inputs = build_suppression_inputs(&db, &env).await;
    assert!(inputs.db_worktree_override_armed);
    assert!(inputs.database_restore_in_progress);
    let decision = resolve_suppression(&inputs);
    assert!(
        decision.suppressed,
        "DB restore should win over worktree override"
    );
    assert_eq!(decision.reason, SuppressionReason::DatabaseRestoreInProgress);
    set_flag(&db, None).await;
}

#[tokio::test]
async fn r558_non_worktree_with_db_override_unaffected() {
    let db = connect().await;
    set_flag(&db, Some(true)).await;
    let env: HashMap<String, String> = HashMap::new();
    let inputs = build_suppression_inputs(&db, &env).await;
    assert!(!inputs.in_worktree);
    assert!(inputs.db_worktree_override_armed);
    let decision = resolve_suppression(&inputs);
    assert!(
        !decision.suppressed,
        "non-worktree should not be suppressed regardless of override"
    );
    assert_eq!(decision.reason, SuppressionReason::None);
    set_flag(&db, None).await;
}
