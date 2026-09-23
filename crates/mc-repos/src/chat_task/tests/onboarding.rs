//! `start_mika_onboarding` 的真库语义（M4-4 / LUM-1601）。
//!
//! Mika 引路写**两行**：一条隐藏的 kickoff（`role = 'user'`，无 task）与一条开场白
//! （`role = 'assistant'`）。两行的 `created_at` 差**正好 1µs** —— 这不是风格问题：会话列表 /
//! 最后一条消息都按 `created_at DESC LIMIT 1` 取，同值会让开场白随机输给 kickoff。

use uuid::Uuid;

use super::{insert_message, new_session, setup, teardown, Fixture, MessageSeed};
use crate::chat_task::StartOnboardingOutcome;

async fn user_message_count(fixture: &Fixture, session_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM chat_message WHERE chat_session_id = $1 AND role = 'user'",
    )
    .bind(session_id)
    .fetch_one(fixture.pool())
    .await
    .expect("count user messages")
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn onboarding_writes_the_kickoff_and_the_opening_one_microsecond_later() {
    let Some(fixture) = setup().await else {
        println!(
            "skip onboarding_writes_the_kickoff_and_the_opening_one_microsecond_later: no env"
        );
        return;
    };
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    let before = super::raw_session(&fixture, session).await.updated_at;

    let outcome = fixture
        .repo()
        .start_mika_onboarding(session, "hello mika", "hi, I am mika")
        .await
        .expect("start onboarding");
    let StartOnboardingOutcome::Started(result) = outcome else {
        panic!("expected Started, got something else");
    };

    assert_eq!(result.kickoff.role, "user");
    assert_eq!(result.kickoff.message_kind, "onboarding_kickoff");
    assert_eq!(result.kickoff.content, "hello mika");
    assert_eq!(
        result.kickoff.task_id, None,
        "kickoff 是产品写的行，不属任何任务"
    );
    assert!(!result.kickoff.channel_ingested);

    assert_eq!(result.opening.role, "assistant");
    assert_eq!(result.opening.message_kind, "onboarding_opening");
    assert_eq!(result.opening.content, "hi, I am mika");
    assert_eq!(result.opening.task_id, None);
    assert_eq!(
        result.opening.chat_session_id,
        result.kickoff.chat_session_id
    );
    assert_eq!(
        result.opening.created_at,
        result.kickoff.created_at + chrono::Duration::microseconds(1),
        "1µs 是本设计的全部内容：同值会让开场白在 ORDER BY created_at DESC LIMIT 1 里随机输"
    );

    // 那两条 `ORDER BY created_at DESC LIMIT 1` 的实际胜者必须是开场白。
    let latest: Uuid = sqlx::query_scalar(
        "SELECT id FROM chat_message WHERE chat_session_id = $1 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session)
    .fetch_one(fixture.pool())
    .await
    .expect("latest message");
    assert_eq!(
        latest, result.opening.id,
        "最新一条必须是开场白（不是 kickoff）"
    );

    // kickoff 是 role='user' ⇒ 「已经开过门」的幂等闸与「有 user 消息」都因此为真。
    assert!(fixture
        .repo()
        .chat_session_has_user_message(session)
        .await
        .expect("has user message"));
    assert!(
        fixture
            .repo()
            .session_has_public_user_message(session)
            .await
            .expect("has public user message"),
        "kickoff 只被 ListChatMessages 的 message_kind 过滤挡住，公开 user 判定里算数"
    );
    assert!(
        super::raw_session(&fixture, session).await.updated_at > before,
        "开门要 touch 会话"
    );
    assert_eq!(user_message_count(&fixture, session).await, 1);

    teardown(&fixture).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn onboarding_is_already_started_after_any_user_message() {
    let Some(fixture) = setup().await else {
        println!("skip onboarding_is_already_started_after_any_user_message: no env");
        return;
    };
    let repo = fixture.repo();
    let session = new_session(&fixture, fixture.agent_id, "active").await;

    let first = repo
        .start_mika_onboarding(session, "hello mika", "hi, I am mika")
        .await
        .expect("first onboarding");
    assert!(matches!(first, StartOnboardingOutcome::Started(_)));

    // 第二次：kickoff 那一行自己就是 role='user' ⇒ 幂等闸命中，且**不**写任何新行。
    let second = repo
        .start_mika_onboarding(session, "hello again", "hi again")
        .await
        .expect("second onboarding");
    assert!(matches!(second, StartOnboardingOutcome::AlreadyStarted));

    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chat_message WHERE chat_session_id = $1")
            .bind(session)
            .fetch_one(fixture.pool())
            .await
            .expect("count messages");
    assert_eq!(total, 2, "重复开门不许落第三行");

    // 普通用户消息之后也一样（`message_kind` 不过滤，只看 role）。
    let plain = new_session(&fixture, fixture.agent_id, "active").await;
    insert_message(&fixture, plain, MessageSeed::user("just a turn")).await;
    let outcome = repo
        .start_mika_onboarding(plain, "hello mika", "hi, I am mika")
        .await
        .expect("onboarding after a plain turn");
    assert!(matches!(outcome, StartOnboardingOutcome::AlreadyStarted));
    assert_eq!(user_message_count(&fixture, plain).await, 1);

    teardown(&fixture).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn onboarding_reports_session_archived_for_missing_or_archived_sessions() {
    let Some(fixture) = setup().await else {
        println!(
            "skip onboarding_reports_session_archived_for_missing_or_archived_sessions: no env"
        );
        return;
    };
    let repo = fixture.repo();

    // 会话不存在：上游在这里也报 `ErrChatSessionArchived`（不是 404）—— 两种结局对调用方同形。
    let missing = repo
        .start_mika_onboarding(Uuid::new_v4(), "hello mika", "hi, I am mika")
        .await
        .expect("onboarding on a missing session");
    assert!(matches!(missing, StartOnboardingOutcome::SessionArchived));

    let archived = new_session(&fixture, fixture.agent_id, "archived").await;
    let outcome = repo
        .start_mika_onboarding(archived, "hello mika", "hi, I am mika")
        .await
        .expect("onboarding on an archived session");
    assert!(matches!(outcome, StartOnboardingOutcome::SessionArchived));
    assert_eq!(
        user_message_count(&fixture, archived).await,
        0,
        "归档会话不许落 kickoff（锁内重读 status，不是调用方快照）"
    );

    teardown(&fixture).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn onboarding_reads_the_profile_and_the_workspace_name() {
    let Some(fixture) = setup().await else {
        println!("skip onboarding_reads_the_profile_and_the_workspace_name: no env");
        return;
    };
    let repo = fixture.repo();

    let profile = repo
        .user_onboarding_profile(fixture.user_id)
        .await
        .expect("profile")
        .expect("row exists");
    assert_eq!(profile.timezone.as_deref(), Some("Asia/Shanghai"));
    assert_eq!(profile.onboarding_questionnaire["role"], "architect");
    assert!(repo
        .user_onboarding_profile(Uuid::new_v4())
        .await
        .expect("profile of a stranger")
        .is_none());

    assert_eq!(
        repo.workspace_name(fixture.workspace_id)
            .await
            .expect("workspace name")
            .as_deref(),
        Some("itest-lum1601-chat")
    );
    assert!(repo
        .workspace_name(Uuid::new_v4())
        .await
        .expect("workspace name of a stranger")
        .is_none());

    // 没写过的会话：`chat_session_has_user_message` 为 false（与开门后的 true 成对）。
    let session = new_session(&fixture, fixture.agent_id, "active").await;
    assert!(!repo
        .chat_session_has_user_message(session)
        .await
        .expect("has user message"));

    teardown(&fixture).await;
}
