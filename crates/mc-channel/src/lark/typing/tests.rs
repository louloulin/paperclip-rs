//! `typing.rs` 的用例（M7-13）：`Typing` 表情的生命周期 —— 贴 / 撤 / 太老跳过 / 快照回落。
//!
//! 时钟是注入的（[`ManualWallClock`]）⇒ "2 分钟边界"用**推进时间**钉住，**不睡真觉**。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mc_core::id::Id;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::lark::channel_store::InstallationLookup;
use crate::lark::feishu_channel::credentials::Decrypter;
use crate::lark::tests::support::{
    decrypter, inbound_message, installation, Call, FakeApi, MemoryStore,
};
use crate::lark::types::ChatType;

/// 纪元毫秒基准（夹具用它算 `create_time`）。
const NOW: i64 = 1_700_000_000_000;

fn session() -> Id {
    Id(Uuid::from_u128(0x2000))
}

fn manager(
    api: &Arc<FakeApi>,
    store: &Arc<MemoryStore>,
    clock: &Arc<ManualWallClock>,
) -> TypingIndicatorManager {
    let bridged = store.store();
    TypingIndicatorManager::new(
        Arc::clone(api) as Arc<dyn ApiClient>,
        decrypter(),
        Arc::clone(&bridged) as Arc<dyn TypingIndicatorQueries>,
    )
    .with_clock(Arc::clone(clock) as Arc<dyn WallClock>)
}

// ---------------------------------------------------------------------
// 「太老」的判据（纯函数）
// ---------------------------------------------------------------------

/// 空串 / 解析不出来 ⇒ **不**拦（上游逐字的回落：字段漂移不该静默吃掉所有指示）。
#[test]
fn age_check_fails_open_on_missing_or_unparsable_timestamps() {
    assert!(!is_message_too_old("", NOW));
    assert!(!is_message_too_old("not-a-number", NOW));
    assert!(!is_message_too_old("-1.5", NOW));
}

/// 边界：**恰好** 2 分钟**不**算太老（上游是 `>`，本仓逐字）。
#[test]
fn age_boundary_is_exclusive_at_two_minutes() {
    let exactly = (NOW - TYPING_INDICATOR_MAX_AGE_MILLIS).to_string();
    assert!(!is_message_too_old(&exactly, NOW));
    let one_more = (NOW - TYPING_INDICATOR_MAX_AGE_MILLIS - 1).to_string();
    assert!(is_message_too_old(&one_more, NOW));
    // 未来的时间戳（时钟偏移）**不**算太老。
    assert!(!is_message_too_old(&(NOW + 5_000).to_string(), NOW));
}

/// 2 分钟的常量在两个口径上一致（秒级 `Duration` 与毫秒判据是同一个界限）。
#[test]
fn the_two_minute_constant_is_consistent_across_both_units() {
    assert_eq!(
        i64::try_from(TYPING_INDICATOR_MAX_AGE.as_millis()).expect("fits"),
        TYPING_INDICATOR_MAX_AGE_MILLIS
    );
    assert_eq!(TYPING_INDICATOR_MAX_AGE_MILLIS, 120_000);
    assert_eq!(TYPING_EMOJI, "Typing");
}

// ---------------------------------------------------------------------
// 贴
// ---------------------------------------------------------------------

#[tokio::test]
async fn adding_a_reaction_records_the_state_with_the_installation_id() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    let manager = manager(&api, &store, &clock);

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    assert_eq!(
        api.log(),
        vec![Call::AddReaction {
            message_id: "om_1".to_string(),
            emoji_type: "Typing".to_string(),
        }]
    );
    assert_eq!(manager.tracked_reactions(session()), 1);
    assert_eq!(manager.tracked_session_count(), 1);
    assert_eq!(
        manager.tracked_reaction_ids(session()),
        vec!["re_1".to_string()]
    );
}

/// 空 `message_id` ⇒ **什么也不做**（上游第一行就是它）。
#[tokio::test]
async fn adding_is_skipped_for_an_empty_message_id() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    manager
        .add_now(
            &installation(0x3000, "cli_a"),
            session(),
            "",
            &NOW.to_string(),
        )
        .await;
    assert!(api.log().is_empty());
    assert_eq!(manager.tracked_session_count(), 0);
}

/// 太老的消息 ⇒ 不贴（WS 重连重放的旧事件不该长出"正在处理"）。
#[tokio::test]
async fn adding_is_skipped_for_a_stale_message() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    let stale = (NOW - TYPING_INDICATOR_MAX_AGE_MILLIS - 1).to_string();

    manager
        .add_now(&installation(0x3000, "cli_a"), session(), "om_old", &stale)
        .await;

    assert!(api.log().is_empty());
    assert_eq!(manager.tracked_session_count(), 0);
}

/// 平台的贴失败 ⇒ **只记日志**、不记状态（上游：*Errors are logged and swallowed*）。
#[tokio::test]
async fn a_failed_add_records_no_state() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    api.fail_reaction_with(crate::lark::tests::support::transport_error());

    manager
        .add_now(
            &installation(0x3000, "cli_a"),
            session(),
            "om_1",
            &NOW.to_string(),
        )
        .await;

    assert_eq!(api.log().len(), 1, "调用发生过");
    assert_eq!(manager.tracked_session_count(), 0, "失败不记状态");
}

/// 没有解密器 ⇒ 失败关闭（**不**发请求）。
#[tokio::test]
async fn adding_without_a_decrypter_fails_closed() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let bridged = store.store();
    let manager = TypingIndicatorManager::new(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        decrypter(),
        bridged as Arc<dyn TypingIndicatorQueries>,
    )
    .with_clock(Arc::clone(&clock) as Arc<dyn WallClock>)
    .without_decrypter();

    manager
        .add_now(
            &installation(0x3000, "cli_a"),
            session(),
            "om_1",
            &NOW.to_string(),
        )
        .await;

    assert!(api.log().is_empty());
    assert_eq!(manager.tracked_session_count(), 0);
}

/// 同一条消息贴两次 ⇒ **两条**状态（上游逐字：*simply appends another state entry*）。
#[tokio::test]
async fn adding_twice_appends_two_state_entries() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    let inst = installation(0x3000, "cli_a");

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    assert_eq!(manager.tracked_reactions(session()), 2);
    assert_eq!(
        manager.tracked_reaction_ids(session()),
        vec!["re_1".to_string(), "re_2".to_string()]
    );
}

// ---------------------------------------------------------------------
// 撤
// ---------------------------------------------------------------------

/// 撤掉该会话的**全部**指示（两条都要撤）。
#[tokio::test]
async fn clearing_removes_every_tracked_reaction_of_the_session() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    let inst = installation(0x3000, "cli_a");
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    manager.clear_now(session()).await;

    let deletions: Vec<Call> = api
        .log()
        .into_iter()
        .filter(|call| matches!(call, Call::DeleteReaction { .. }))
        .collect();
    assert_eq!(deletions.len(), 2);
    assert_eq!(manager.tracked_session_count(), 0, "状态被取走");
}

/// 幂等：没有跟踪状态的会话 ⇒ no-op（上游逐字）。
#[tokio::test]
async fn clearing_an_untracked_session_is_a_noop() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);

    manager.clear_now(session()).await;
    manager.clear_now(session()).await;

    assert!(api.log().is_empty());
}

/// 第二次 `clear` 是 no-op（状态在第一次就被取走了）。
#[tokio::test]
async fn clearing_twice_only_deletes_once() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    let inst = installation(0x3000, "cli_a");
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    manager.clear_now(session()).await;
    manager.clear_now(session()).await;

    let deletions = api
        .log()
        .into_iter()
        .filter(|call| matches!(call, Call::DeleteReaction { .. }))
        .count();
    assert_eq!(deletions, 1);
}

/// 某一条撤失败 ⇒ **不中断**循环（后面的照撤）。
#[tokio::test]
async fn a_failed_delete_does_not_abort_the_remaining_reactions() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    let inst = installation(0x3000, "cli_a");
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager
        .add_now(&inst, session(), "om_2", &NOW.to_string())
        .await;
    api.fail_delete_with(crate::lark::tests::support::transport_error());

    manager.clear_now(session()).await;

    let deletions = api
        .log()
        .into_iter()
        .filter(|call| matches!(call, Call::DeleteReaction { .. }))
        .count();
    assert_eq!(deletions, 2, "第二次照样发出去");
}

/// 数安装行查询次数的替身（插在 [`MemoryStore`] 前面）。
struct CountingStore {
    inner: Arc<MemoryStore>,
    lookups: AtomicUsize,
}

#[async_trait::async_trait]
impl InstallationLookup for CountingStore {
    async fn get(
        &self,
        id: Id,
    ) -> Result<Option<crate::lark::resolvers::LarkInstallation>, mc_repos::RepoError> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.inner.get(id).await
    }
}

/// 只暴露安装行查询的窄端口（**不是**整条桥）。
struct QueriesOnly(Arc<CountingStore>);

#[async_trait::async_trait]
impl TypingIndicatorQueries for QueriesOnly {
    async fn installation(
        &self,
        id: Id,
    ) -> EngineResult<Option<crate::lark::resolvers::LarkInstallation>> {
        InstallationLookup::get(&*self.0, id)
            .await
            .map_err(|error| crate::engine::resolvers::EngineError::infra(error.to_string()))
    }
}

/// 同一会话的两条指示共享一个安装 ⇒ 安装行查询**只查一次**（上游的备忘）。
#[tokio::test]
async fn credentials_are_resolved_once_per_installation() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let inst = installation(0x3000, "cli_a");
    store.put_installation(&inst);
    let counting = Arc::new(CountingStore {
        inner: Arc::clone(&store),
        lookups: AtomicUsize::new(0),
    });

    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = TypingIndicatorManager::new(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        decrypter(),
        Arc::new(QueriesOnly(Arc::clone(&counting))) as Arc<dyn TypingIndicatorQueries>,
    )
    .with_clock(Arc::clone(&clock) as Arc<dyn WallClock>);

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager
        .add_now(&inst, session(), "om_2", &NOW.to_string())
        .await;
    manager.clear_now(session()).await;

    assert_eq!(
        counting.lookups.load(Ordering::SeqCst),
        1,
        "同一安装的凭据只解一次"
    );
    assert_eq!(
        api.log()
            .into_iter()
            .filter(|call| matches!(call, Call::DeleteReaction { .. }))
            .count(),
        2
    );
}

/// 安装行在撤的时候**已经没了**（运行时拆除）⇒ 回落**快照**（上游 *FALLBACK, never the
/// primary*）。
#[tokio::test]
async fn clearing_falls_back_to_the_snapshot_when_the_installation_row_is_gone() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    store.put_installation(&inst);
    let manager = manager(&api, &store, &clock);
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    // 运行时拆除：安装行没了。
    *store.installation_gone.lock().expect("poisoned") = true;
    manager.clear_now(session()).await;

    let deletions: Vec<Call> = api
        .log()
        .into_iter()
        .filter(|call| matches!(call, Call::DeleteReaction { .. }))
        .collect();
    assert_eq!(deletions.len(), 1, "快照仍然能把表情摘下来");
}

/// 没接安装行查询的形态（[`TypingIndicatorManager::with_snapshot_only`]）同样能撤。
#[tokio::test]
async fn snapshot_only_mode_clears_from_the_snapshot() {
    let api = FakeApi::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    let manager = TypingIndicatorManager::with_snapshot_only(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        decrypter(),
    )
    .with_clock(Arc::clone(&clock) as Arc<dyn WallClock>);

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager.clear_now(session()).await;

    assert_eq!(
        api.log()
            .into_iter()
            .filter(|call| matches!(call, Call::DeleteReaction { .. }))
            .count(),
        1
    );
}

/// 没接解密器 ⇒ **两侧都失败关闭**：贴不发、撤也不发，且状态表保持干净。
#[tokio::test]
async fn without_a_decrypter_nothing_touches_the_network() {
    let api = FakeApi::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    let manager = TypingIndicatorManager::with_snapshot_only(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        decrypter(),
    )
    .with_clock(Arc::clone(&clock) as Arc<dyn WallClock>)
    .without_decrypter();

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager.clear_now(session()).await;

    assert!(api.log().is_empty());
    assert_eq!(manager.tracked_session_count(), 0);
}

/// 密钥与密文不匹配 ⇒ 贴不动（失败关闭），状态表保持干净。
#[tokio::test]
async fn an_unusable_decrypter_fails_closed() {
    let api = FakeApi::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    let manager = TypingIndicatorManager::with_snapshot_only(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::secret_box(
            mc_secrets::secretbox::SecretBox::new(&[0x22; 32]).expect("key size"),
        ),
    )
    .with_clock(Arc::clone(&clock) as Arc<dyn WallClock>);

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    assert!(api.log().is_empty(), "解不开凭据 ⇒ 贴都不发");
    assert_eq!(manager.tracked_session_count(), 0);
}

// ---------------------------------------------------------------------
// engine 端口（同步接缝 → 脱离任务）
// ---------------------------------------------------------------------

/// `on_ingested` 从 `raw` 里读 `create_time`、从信封里读安装投影，异步把表情贴上。
#[tokio::test]
async fn the_notifier_port_reads_create_time_and_the_installation_from_the_envelope() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    let manager = Arc::new(manager(&api, &store, &clock));

    let message = inbound_message("oc_main", ChatType::Group, "om_1", "", &NOW.to_string());
    let resolved = crate::lark::tests::support::resolved(&inst);
    TypingNotifier::on_ingested(&*manager, &resolved, &message, session());

    // 脱离任务 ⇒ 让出一次调度再断言。
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        api.log(),
        vec![Call::AddReaction {
            message_id: "om_1".to_string(),
            emoji_type: "Typing".to_string(),
        }]
    );
    assert_eq!(manager.tracked_reactions(session()), 1);
}

/// `on_settled` 撤该会话的指示（也是脱离任务）。
#[tokio::test]
async fn the_notifier_settle_port_clears_the_session() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = installation(0x3000, "cli_a");
    let manager = Arc::new(manager(&api, &store, &clock));
    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;

    TypingNotifier::on_settled(&*manager, session());
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    assert_eq!(manager.tracked_session_count(), 0);
    assert_eq!(
        api.log()
            .into_iter()
            .filter(|call| matches!(call, Call::DeleteReaction { .. }))
            .count(),
        1
    );
}

/// `ResolvedInstallation` 里没有本 adapter 的安装投影 ⇒ `on_ingested` **降级跳过**。
#[tokio::test]
async fn the_notifier_port_skips_a_foreign_installation_envelope() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = Arc::new(manager(&api, &store, &clock));

    let foreign = crate::engine::resolvers::ResolvedInstallation::new(
        Id(Uuid::from_u128(0x3000)),
        Id(Uuid::from_u128(0x9000)),
        Id(Uuid::from_u128(0x9100)),
        Id(Uuid::from_u128(0x9200)),
        crate::lark::resolvers::TYPE_LARK,
        true,
    );
    let message = inbound_message("oc_main", ChatType::Group, "om_1", "", &NOW.to_string());
    TypingNotifier::on_ingested(&*manager, &foreign, &message, session());
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    assert!(api.log().is_empty());
}

// ---------------------------------------------------------------------
// raw 读取（两条读者的契约）
// ---------------------------------------------------------------------

#[test]
fn create_time_is_read_from_the_raw_payload_and_defaults_to_empty() {
    let message = inbound_message("oc_main", ChatType::Group, "om_1", "", "1700000000000");
    assert_eq!(create_time_of(&message), "1700000000000");

    let mut without = message.clone();
    without.raw = json!({"message_id": "om_1"});
    assert_eq!(create_time_of(&without), "", "读不到 ⇒ 空串 ⇒ 不拦");
}

#[test]
fn reaction_target_prefers_the_dedup_message_id() {
    let message = inbound_message("oc_main", ChatType::Group, "om_1", "", "");
    assert_eq!(reaction_target_of(&message), "om_1");

    // 空 `message_id` ⇒ 空串（**不**回落事件 id：`add_now` 会因此直接返回）。
    let mut empty = message.clone();
    empty.message_id = String::new();
    assert_eq!(reaction_target_of(&empty), "");
}

#[test]
fn installation_of_reads_the_platform_projection() {
    let inst = installation(0x3000, "cli_a");
    let resolved = crate::lark::tests::support::resolved(&inst);
    assert_eq!(
        installation_of(&resolved).map(|found| found.app_id.as_str()),
        Some("cli_a")
    );

    let foreign = crate::engine::resolvers::ResolvedInstallation::new(
        inst.id,
        inst.workspace_id,
        inst.agent_id,
        inst.installer_user_id,
        crate::lark::resolvers::TYPE_LARK,
        true,
    );
    assert!(installation_of(&foreign).is_none());
}

// ---------------------------------------------------------------------
// 凭据面
// ---------------------------------------------------------------------

/// 状态条目的 `Debug` **不打印快照**（只报"有没有"），也不打印任何密钥字节。
#[test]
fn the_state_debug_never_prints_the_snapshot_or_the_ciphertext() {
    let state = TypingIndicatorState {
        message_id: "om_1".to_string(),
        reaction_id: "re_1".to_string(),
        installation_id: Id(Uuid::from_u128(0x3000)),
        installation_snapshot: installation(0x3000, "cli_a"),
    };
    let rendered = format!("{state:?}");
    assert!(rendered.contains("om_1"));
    assert!(rendered.contains("re_1"));
    assert!(rendered.contains("has_installation_snapshot: true"));
    assert!(
        !rendered.contains("app_secret_encrypted"),
        "状态条目的 Debug 不得展开安装投影: {rendered}"
    );
}

/// 管理器的 `Debug` 只有存在性与跟踪数（**没有**凭据）。
#[test]
fn the_manager_debug_is_credential_free() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let manager = manager(&api, &store, &clock);
    let rendered = format!("{manager:?}");
    assert!(rendered.contains("TypingIndicatorManager"));
    assert!(rendered.contains("tracked_sessions: 0"));
    assert!(!rendered.contains("secret"));
    assert!(!rendered.contains("ciphertext"));
}

/// 用**身份解密器**（`login_plaintext`，上游 `box == nil` 的语义）时贴与撤都走通 ——
/// 证明本片的生命周期不依赖密钥种类（上一条用例已经证明它**不绕过**解密器）。
#[tokio::test]
async fn the_lifecycle_works_with_a_plaintext_decrypter_too() {
    let api = FakeApi::new();
    let clock = Arc::new(ManualWallClock::new(NOW));
    let inst = crate::lark::tests::support::installation_plaintext(0x3000, "cli_a");
    let manager = TypingIndicatorManager::with_snapshot_only(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::login_plaintext(),
    )
    .with_clock(Arc::clone(&clock) as Arc<dyn WallClock>);

    manager
        .add_now(&inst, session(), "om_1", &NOW.to_string())
        .await;
    manager.clear_now(session()).await;

    assert_eq!(api.log().len(), 2, "贴一次 + 撤一次");
}
