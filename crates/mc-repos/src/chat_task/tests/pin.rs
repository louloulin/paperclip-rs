//! `chat_pinned_agent` 仓储的真库语义（M4-4 / LUM-1601，`docs/45` §3 G9）。
//!
//! 四条查询里两处容易被「看着也对」的写法骗过：
//!
//! * `GetMaxChatPinnedAgentPosition` 是 `COALESCE(MAX(position), 0)::float8`（**不是 -1**）
//!   ⇒ 第一条 pin 落在 `1.0`，`next_position` 的 `+1` 才成立；
//! * `CreateChatPinnedAgent` 是 `ON CONFLICT ... DO UPDATE SET position = 现有值`
//!   ⇒ 重复 pin **保持原槽位**（换成 `DO NOTHING` 会让 `:one` 命中 no-rows 报错）。

use uuid::Uuid;

use super::{new_agent, setup, teardown};
use crate::chat_pinned_agent::{ChatPinnedAgentRepo, ChatPinnedAgentRow};

/// 浮点相等（`clippy::float_cmp` 不喜欢 `==`，仓内既有写法）。
fn assert_position(row: &ChatPinnedAgentRow, expected: f64) {
    let pos = row.position;
    assert!(
        (pos - expected).abs() < f64::EPSILON,
        "position {pos} != {expected}"
    );
}

fn assert_max(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON,
        "max position {actual} != {expected}"
    );
}

/// 位置从 1.0 起算、重复 pin 幂等、列表按 `position ASC, created_at ASC`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn pin_positions_start_at_one_and_repeated_pins_keep_the_original_slot() {
    let Some(fixture) = setup().await else {
        println!(
            "skip pin_positions_start_at_one_and_repeated_pins_keep_the_original_slot: no env"
        );
        return;
    };
    let repo = ChatPinnedAgentRepo::new(fixture.db.clone());
    let other_agent = new_agent(&fixture).await;
    let (workspace, user) = (fixture.workspace_id, fixture.user_id);

    // 空栏 ⇒ 0.0（不是 -1.0）：调用方 `+1` 得到第一条的 1.0。
    assert_max(repo.max_position(workspace, user).await.expect("max"), 0.0);
    assert!(repo.list(workspace, user).await.expect("list").is_empty());

    let first = repo
        .create(
            workspace,
            user,
            fixture.agent_id,
            repo.max_position(workspace, user).await.expect("max") + 1.0,
        )
        .await
        .expect("pin first");
    assert_position(&first, 1.0);
    assert_eq!(first.agent_id, fixture.agent_id);
    assert_eq!(first.workspace_id, workspace);
    assert_eq!(first.user_id, user);
    assert_eq!(first.agent_id(), mc_core::Id::from(fixture.agent_id));
    assert_max(repo.max_position(workspace, user).await.expect("max"), 1.0);

    // 重复 pin：同一行、同一槽位、同一 `created_at`（`DO UPDATE` 写回现有值）。
    let again = repo
        .create(workspace, user, fixture.agent_id, 99.0)
        .await
        .expect("pin again");
    assert_eq!(again.id, first.id, "`ON CONFLICT` 命中既有行");
    assert_position(&again, 1.0);
    assert_eq!(again.created_at, first.created_at);

    let second = repo
        .create(workspace, user, other_agent, 2.0)
        .await
        .expect("pin second");
    assert_position(&second, 2.0);
    assert_max(repo.max_position(workspace, user).await.expect("max"), 2.0);

    let listed = repo.list(workspace, user).await.expect("list");
    assert_eq!(
        listed.iter().map(|row| row.agent_id).collect::<Vec<_>>(),
        vec![fixture.agent_id, other_agent]
    );
    assert_position(&listed[0], 1.0);
    assert_position(&listed[1], 2.0);

    // 快捷栏是**每用户私有**的，查询同时带租户与用户两个条件。
    assert!(repo
        .list(workspace, Uuid::new_v4())
        .await
        .expect("other user")
        .is_empty());
    assert!(repo
        .list(Uuid::new_v4(), user)
        .await
        .expect("other workspace")
        .is_empty());
    assert_max(
        repo.max_position(Uuid::new_v4(), user).await.expect("max"),
        0.0,
    );

    // 删除按三元组生效、幂等；`delete` 的错三元组（错 user）不动别人的行。
    assert_eq!(
        repo.delete(workspace, Uuid::new_v4(), fixture.agent_id)
            .await
            .expect("wrong user"),
        0
    );
    assert_eq!(
        repo.delete(workspace, user, other_agent)
            .await
            .expect("unpin"),
        1
    );
    assert_eq!(
        repo.delete(workspace, user, other_agent)
            .await
            .expect("unpin again"),
        0
    );

    // 表上没有 `position` 唯一约束 ⇒ 槽位可以并列；并列时按 `created_at ASC` 定序。
    let third = repo
        .create(workspace, user, other_agent, 1.0)
        .await
        .expect("re-pin at the same slot");
    assert_position(&third, 1.0);
    let listed = repo.list(workspace, user).await.expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].agent_id, fixture.agent_id, "同名次时先创建的在前");
    assert_eq!(listed[1].agent_id, other_agent);

    teardown(&fixture).await;
}
