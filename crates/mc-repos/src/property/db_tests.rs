//! `PropertyRepo` 的 PG 集成测试（需要真库；`MULTICA_TEST_DATABASE_URL`）。
//!
//! 从 `tests.rs` 拆出（R7 单文件 800 行上限）：纯校验单测留在 `tests.rs`，
//! 真库用例在这里。

use super::tests::select_config;
use super::*;
use crate::issue::{IssueRepo, NewIssue};
use serde_json::json;
use std::env;
struct Fixture {
    db: Db,
    workspace_id: Id,
    user_id: Id,
    issue_id: Id,
}

async fn setup() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1).await.ok()?;
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m2e-prop', $1) RETURNING id",
    )
    .bind(format!("itest-m2e-p-{}", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .ok()?;
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m2e-prop', $1) RETURNING id"#,
    )
    .bind(format!("itest-m2e-p-{}@example.com", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .ok()?;
    let workspace_id = Id::from(workspace_id);
    let user_id = Id::from(user_id);
    let issue = IssueRepo::new(db.clone())
        .create(NewIssue::new(
            workspace_id,
            "itest property issue",
            user_id.0.to_string(),
        ))
        .await
        .ok()?;
    Some(Fixture {
        db,
        workspace_id,
        user_id,
        issue_id: issue.id(),
    })
}

macro_rules! fixture {
    () => {
        match setup().await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

async fn teardown(fx: &Fixture) {
    let _ = sqlx::query("DELETE FROM issue_property WHERE workspace_id = $1")
        .bind(fx.workspace_id.0)
        .execute(fx.db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fx.workspace_id.0)
        .execute(fx.db.pool())
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(fx.user_id.0)
        .execute(fx.db.pool())
        .await;
}

fn new_prop(name: &str, property_type: &str, config: JsonValue) -> NewProperty {
    NewProperty {
        name: name.to_string(),
        property_type: property_type.to_string(),
        description: String::new(),
        icon: String::new(),
        config,
    }
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_property_create_list_update_archive() {
    let fx = fixture!();
    let repo = PropertyRepo::new(fx.db.clone());

    let first = repo
        .create(fx.workspace_id, &new_prop("Estimate", "number", json!({})))
        .await
        .expect("create");
    assert_eq!(first.property_type, "number");
    assert!((first.position - 1.0).abs() < f64::EPSILON);
    assert!(!first.is_archived());

    // 重名（大小写不敏感）→ 23505 → Conflict → 路由 409。
    let dup = repo
        .create(fx.workspace_id, &new_prop("estimate", "text", json!({})))
        .await
        .expect_err("dup");
    assert!(
        matches!(dup, PropertyError::Repo(RepoError::Conflict)),
        "{dup:?}"
    );

    // position 递增。
    let second = repo
        .create(
            fx.workspace_id,
            &new_prop("Severity", "select", select_config()),
        )
        .await
        .expect("create select");
    assert!((second.position - 2.0).abs() < f64::EPSILON);

    let listed = repo.list(fx.workspace_id, false).await.expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].usage_count, 0);
    assert_eq!(parse_config(&listed[1].config).options.len(), 2);

    let updated = repo
        .update(
            fx.workspace_id,
            first.id(),
            &PropertyUpdate {
                description: Some("points".into()),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.description, "points");
    assert_eq!(updated.property_type, "number", "type 不可变");

    // 归档 ⇒ 默认列表看不到，include_archived 能看到。
    let archived = repo
        .update(
            fx.workspace_id,
            first.id(),
            &PropertyUpdate {
                archived: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("archive");
    assert!(archived.is_archived());
    assert_eq!(repo.list(fx.workspace_id, false).await.unwrap().len(), 1);
    assert_eq!(repo.list(fx.workspace_id, true).await.unwrap().len(), 2);
    assert_eq!(repo.active_count(fx.workspace_id).await.unwrap(), 1);

    // 取消归档：active_count 重新计入。
    let restored = repo
        .update(
            fx.workspace_id,
            first.id(),
            &PropertyUpdate {
                archived: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("unarchive");
    assert!(!restored.is_archived());

    // 未命中 / 跨 workspace → NotFound → 路由 404。
    assert!(matches!(
        repo.update(fx.workspace_id, Id::new(), &PropertyUpdate::default())
            .await,
        Err(PropertyError::Repo(RepoError::NotFound))
    ));

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_active_cap_is_20_and_holds_under_concurrency() {
    let fx = fixture!();
    let repo = PropertyRepo::new(fx.db.clone());
    let cap = i64::try_from(MAX_ACTIVE_PROPERTIES).unwrap_or(i64::MAX);
    for i in 0..MAX_ACTIVE_PROPERTIES {
        repo.create(
            fx.workspace_id,
            &new_prop(&format!("p{i}"), "text", json!({})),
        )
        .await
        .expect("create");
    }
    assert_eq!(repo.active_count(fx.workspace_id).await.unwrap(), cap);
    let err = repo
        .create(fx.workspace_id, &new_prop("p-over", "text", json!({})))
        .await
        .expect_err("cap");
    assert!(matches!(err, PropertyError::ActiveCap(20)), "{err:?}");

    // 归档一条后腾出名额。
    let rows = repo.list(fx.workspace_id, false).await.expect("list");
    repo.update(
        fx.workspace_id,
        rows[0].id.into(),
        &PropertyUpdate {
            archived: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("archive");
    repo.create(fx.workspace_id, &new_prop("p-freed", "text", json!({})))
        .await
        .expect("create after archive");

    // 并发 cap：腾出**恰好一个**名额（active = 19）后同时 create 两个，
    // 只有 advisory lock 串行化了 read-then-write 才能只过一个（上游 F5）。
    let rows = repo.list(fx.workspace_id, false).await.expect("list");
    repo.update(
        fx.workspace_id,
        rows[0].id.into(),
        &PropertyUpdate {
            archived: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("archive one");
    assert_eq!(
        repo.active_count(fx.workspace_id).await.unwrap(),
        cap - 1,
        "恰好空出一个名额"
    );
    let a = repo.clone();
    let b = repo.clone();
    let ws = fx.workspace_id;
    let payload_one = new_prop("race-a", "text", json!({}));
    let payload_two = new_prop("race-b", "text", json!({}));
    let (ra, rb) = tokio::join!(a.create(ws, &payload_one), b.create(ws, &payload_two));
    assert!(
        ra.is_ok() ^ rb.is_ok(),
        "advisory lock 应只放一个通过：{ra:?} / {rb:?}"
    );
    let loser = match ra {
        Err(err) => err,
        Ok(_) => rb.expect_err("至少一个 create 应撞 cap"),
    };
    assert!(
        matches!(loser, PropertyError::ActiveCap(20)),
        "落选者应是 cap 而不是别的错：{loser:?}"
    );
    assert_eq!(
        repo.active_count(fx.workspace_id).await.unwrap(),
        cap,
        "并发插入不得越上限"
    );

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_removing_in_use_option_conflicts() {
    let fx = fixture!();
    let repo = PropertyRepo::new(fx.db.clone());
    let def = repo
        .create(
            fx.workspace_id,
            &new_prop("Severity", "select", select_config()),
        )
        .await
        .expect("create");
    let config = parse_config(&def.config);
    let high = config.options[1].id.clone();

    // 未使用的选项可以删。
    let trimmed = json!({"options": [{"id": high, "name": "High", "color": "#ef4444"}]});
    repo.update(
        fx.workspace_id,
        def.id(),
        &PropertyUpdate {
            config: Some(trimmed.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("remove unused option");

    // 写一个值进 issue.properties（key = 定义 UUID 文本，与 `usage_count` 的普查口径
    // 一致）再试图删它在用的选项。
    IssueRepo::new(fx.db.clone())
        .set_property(
            fx.workspace_id,
            fx.issue_id,
            &def.id().to_string(),
            &json!(high),
        )
        .await
        .expect("set property value");
    let rows = repo.list(fx.workspace_id, false).await.expect("list");
    assert_eq!(rows[0].usage_count, 1, "usage_count 反映值袋");

    let empty = json!({"options": []});
    let err = repo
        .update(
            fx.workspace_id,
            def.id(),
            &PropertyUpdate {
                config: Some(empty),
                ..Default::default()
            },
        )
        .await
        .expect_err("in use");
    match err {
        PropertyError::OptionsInUse(message) => {
            assert!(
                message.starts_with("cannot remove options still in use: \"High\" (1 issues)"),
                "{message}"
            );
        }
        other => panic!("expected OptionsInUse, got {other:?}"),
    }

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_value_face_bridging() {
    let fx = fixture!();
    let repo = PropertyRepo::new(fx.db.clone());
    let def = repo
        .create(
            fx.workspace_id,
            &new_prop("Severity", "select", select_config()),
        )
        .await
        .expect("create");
    let config = parse_config(&def.config);
    let low = config.options[0].id.clone();

    // 正常路径：定义可解析 + 值合法。
    let resolved = repo
        .definition_for_value(fx.workspace_id, def.id())
        .await
        .expect("definition");
    assert_eq!(resolved.property_type, "select");
    let value =
        validate_value(&resolved.property_type, &resolved.config, &json!(low)).expect("value");
    assert_eq!(value, json!(low));

    // 未知定义 → NotFound（路由 404）。
    assert!(matches!(
        repo.definition_for_value(fx.workspace_id, Id::new()).await,
        Err(PropertyError::Repo(RepoError::NotFound))
    ));

    // 归档定义 → 400。
    repo.update(
        fx.workspace_id,
        def.id(),
        &PropertyUpdate {
            archived: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("archive");
    match repo.definition_for_value(fx.workspace_id, def.id()).await {
        Err(PropertyError::Archived(name)) => assert_eq!(name, "Severity"),
        other => panic!("expected Archived, got {other:?}"),
    }
    // 归档后仍允许清值（DELETE 面只要求定义存在）。
    assert!(repo
        .definition_exists(fx.workspace_id, def.id())
        .await
        .expect("exists"));

    // actor 引用解析：非成员 → 400；成员 → 通过。
    let stray = format!("member:{}", Uuid::new_v4());
    match repo
        .resolve_actor_refs(fx.workspace_id, std::slice::from_ref(&stray))
        .await
    {
        Err(PropertyError::Invalid(message)) => assert_eq!(
            message,
            format!("{stray:?} does not refer to a member of this workspace")
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(fx.workspace_id.0)
        .bind(fx.user_id.0)
        .execute(fx.db.pool())
        .await
        .expect("member");
    repo.resolve_actor_refs(fx.workspace_id, &[format!("member:{}", fx.user_id.0)])
        .await
        .expect("member resolves");

    teardown(&fx).await;
    fx.db.close().await;
}
