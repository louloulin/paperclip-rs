//! 跨 workspace 隔离 + 「可用性」判定（从 `tests.rs` 拆出，R7 单文件 800 行上限）。
//!
//! 路由层的 404（而不是 403）依赖这一层「一切按 workspace 过滤」；无主 runtime
//! 不可用则决定 `canUseRuntimeForAgent` 的口径。

use mc_core::Id;

use super::super::profiles::UpdateRuntimeProfile;
use super::setup;
use crate::runtime::{ProfileDeleteError, RuntimeListFilter};
use crate::RepoError;

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn profiles_and_runtimes_are_workspace_scoped() {
    let Some(fx) = setup().await else {
        return;
    };
    let other_ws = Id::new();
    sqlx::query("INSERT INTO workspace(id, name, slug) VALUES ($1, 'other', $2)")
        .bind(other_ws.as_uuid())
        .bind(format!("lum1427-other-{}", other_ws.as_uuid().simple()))
        .execute(&fx.pool)
        .await
        .unwrap();

    let profile = fx.profiles.create(fx.profile("Scoped")).await.unwrap();
    let rt = fx
        .runtimes
        .create(fx.runtime("scoped", "codex", Some(profile.id)))
        .await
        .unwrap();

    assert!(fx.profiles.list(other_ws).await.unwrap().is_empty());
    assert!(fx
        .profiles
        .get(other_ws, profile.id)
        .await
        .unwrap()
        .is_none());
    assert!(fx
        .runtimes
        .list(other_ws, RuntimeListFilter::All)
        .await
        .unwrap()
        .is_empty());

    let cross = fx
        .profiles
        .update(
            other_ws,
            profile.id,
            UpdateRuntimeProfile {
                display_name: Some("hijacked".to_owned()),
                ..UpdateRuntimeProfile::default()
            },
        )
        .await;
    assert!(matches!(cross, Err(RepoError::NotFound)), "got {cross:?}");
    let cross_delete = fx.profiles.delete_cascade(other_ws, profile.id).await;
    assert!(
        matches!(cross_delete, Err(ProfileDeleteError::NotFound)),
        "got {cross_delete:?}"
    );
    assert!(fx.profiles.get(fx.ws, profile.id).await.unwrap().is_some());
    assert!(fx.runtimes.get(rt.id).await.unwrap().is_some());

    // 无主 runtime 不可用（upstream `canUseRuntimeForAgent`：task claim 要 owner 签 token）。
    let mut orphan = fx.runtime("orphan", "claude", None);
    orphan.owner_id = None;
    let orphan = fx.runtimes.create(orphan).await.unwrap();
    assert!(!orphan.usable_by(fx.owner), "无主 runtime 对谁都不可用");

    sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other_ws.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.cleanup().await;
}
