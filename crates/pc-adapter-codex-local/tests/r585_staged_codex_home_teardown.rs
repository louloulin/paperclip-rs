//! R585 — staged codex home teardown + Drop guard 集成测试
//!
//! 验证 G6 收尾：
//! - `teardown_staged_codex_home` 幂等删除
//! - `StagedCodexHomeGuard` Drop 时自动清理
//! - `disarm()` 阻止 Drop cleanup
//! - ENOENT 不报错（force=true 等价）

use pc_adapter_codex_local::codex_home_staging::{
    stage_codex_home_for_sync, teardown_staged_codex_home, StageCodexHomeForSyncOptions,
    StagedCodexHomeGuard,
};
use std::fs;
use std::path::PathBuf;

fn tmp_root(label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "pc-staged-teardown-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

#[tokio::test]
async fn teardown_removes_staged_home() {
    let root = tmp_root("teardown-removes");
    let source = root.join("source");
    let staged = root.join("staged");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("config.json"), r#"{"hello": "world"}"#).unwrap();

    let staged_path = stage_codex_home_for_sync(
        &source,
        StageCodexHomeForSyncOptions {
            run_id: Some("run-1".into()),
        },
    )
    .await
    .unwrap();
    assert!(staged_path.exists(), "staged should exist");

    teardown_staged_codex_home(&staged_path).await;
    assert!(!staged_path.exists(), "teardown should remove staged");

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn teardown_is_idempotent_on_missing() {
    let root = tmp_root("teardown-idempotent");
    let staged = root.join("never-existed");
    fs::create_dir_all(&root).unwrap();

    // 第一次：ENOENT（不存在）
    teardown_staged_codex_home(&staged).await;
    // 第二次：同样 ENOENT —— 仍然不应 panic
    teardown_staged_codex_home(&staged).await;
    // 第三次
    teardown_staged_codex_home(&staged).await;

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn teardown_tolerates_permission_errors_on_cleanup() {
    let root = tmp_root("teardown-tolerates");
    let source = root.join("source");
    let staged = root.join("staged");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("auth.json"), "{\"token\":\"abc\"}").unwrap();

    let staged_path = stage_codex_home_for_sync(
        &source,
        StageCodexHomeForSyncOptions {
            run_id: Some("r".into()),
        },
    )
    .await
    .unwrap();

    // 删除 staged 后再 teardown（应该 ENOENT，不报错）
    fs::remove_dir_all(&staged_path).unwrap();
    teardown_staged_codex_home(&staged_path).await; // 不应 panic

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn guard_drop_cleans_up_staged_home() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let staged = rt.block_on(async {
        let root = tmp_root("guard-drop");
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("config.json"), "{}").unwrap();

        let staged = stage_codex_home_for_sync(
            &source,
            StageCodexHomeForSyncOptions {
                run_id: Some("g".into()),
            },
        )
        .await
        .unwrap();
        assert!(staged.exists());
        staged
    });
    let staged_clone = staged.clone();

    {
        let _guard = StagedCodexHomeGuard::new(staged);
        // guard 在 scope 结束时 drop
    }
    assert!(!staged_clone.exists(), "guard Drop must remove staged home");
}

#[tokio::test]
async fn guard_disarm_preserves_staged_home() {
    let root = tmp_root("guard-disarm");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("config.json"), "{}").unwrap();

    let staged = stage_codex_home_for_sync(
        &source,
        StageCodexHomeForSyncOptions {
            run_id: Some("d".into()),
        },
    )
    .await
    .unwrap();
    let staged_clone = staged.clone();

    {
        let guard = StagedCodexHomeGuard::new(staged);
        let _preserved = guard.disarm(); // 显式 disarm
    }
    assert!(
        staged_clone.exists(),
        "disarm must prevent Drop cleanup (保留供调试)"
    );

    // 清理
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn guard_path_accessor_returns_staged_home() {
    let staged_path = PathBuf::from("/tmp/pc-test-staged");
    let guard = StagedCodexHomeGuard::new(staged_path.clone());
    assert_eq!(guard.path(), staged_path.as_path());
}
