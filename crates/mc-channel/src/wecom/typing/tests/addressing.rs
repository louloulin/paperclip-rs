//! 会话回查、`originOf` 的三格、**寻址规则的等价比对**，以及自动重试那条血缘。
//!
//! 本文件是 `typing/tests.rs` 的子模块（门 ⑩ 的 800 行硬限拆分，见 `docs/32` §38 的 D9）。

use super::*;

/// `sessionFor` 的信封优先 + 从 task 行回查的兜底（上游逐字：那个兜底留着的理由是"一个一直转圈
/// 没人收的气泡是一次**没人报告**的失败"）。
#[tokio::test]
async fn session_for_prefers_the_envelope_and_falls_back_to_the_row() {
    let row_session = Id::new();
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), None, Some(row_session)),
            false,
        ))
        .build();
    let envelope_session = Id::new();

    // 信封上有 ⇒ 直接用（**不**读库）。
    harness.tasks.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .indicator
            .session_for(&TaskEvent::queued(
                TASK_ID,
                Some(envelope_session.to_string())
            ))
            .await,
        Some(envelope_session)
    );
    // 信封上没有 ⇒ 回查那一行。
    harness.tasks.fail.store(false, Ordering::SeqCst);
    assert_eq!(
        harness
            .indicator
            .session_for(&TaskEvent::queued(TASK_ID, None::<String>))
            .await,
        Some(row_session)
    );
    // 读库失败 ⇒ 没有会话（调用方按"没有气泡"处理）。
    harness.tasks.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .indicator
            .session_for(&TaskEvent::queued(TASK_ID, None::<String>))
            .await,
        None
    );
}

/// `originOf` 的三格，逐格独立可判（上游 `originVerdict` 的"三格各要不同的动作"）。
#[tokio::test]
async fn origin_of_returns_three_distinct_verdicts() {
    let session = Id::new();
    let task_id = Id::new();

    // 没有 task 面 ⇒ Unknown（拒绝宣告，也**不**释放）。
    assert_eq!(
        origin_of(None, Some(session), &task_id.to_string()).await,
        OriginVerdict::Unknown
    );
    // 空 task id ⇒ Unknown。
    assert_eq!(
        origin_of(None, Some(session), "").await,
        OriginVerdict::Unknown
    );
    // 解不出的 id ⇒ Unknown。
    let tasks = Arc::new(FakeTasks::with_task(
        task_row(task_id, None, Some(session)),
        false,
    ));
    let dynamic: Arc<dyn TaskQueries> = Arc::clone(&tasks) as Arc<dyn TaskQueries>;
    assert_eq!(
        origin_of(Some(dynamic.as_ref()), Some(session), "not-a-uuid").await,
        OriginVerdict::Unknown
    );
    // `chat_input_task_id` 是 NULL ⇒ **默认投递**，而且**不花第二次读**（判据自己那条短路：
    // 一个 NULL 所有者是"封口之前的渠道任务"）。读计数器是这条主张的证据。
    assert_eq!(
        origin_of(Some(dynamic.as_ref()), Some(session), &task_id.to_string()).await,
        OriginVerdict::Ours
    );
    assert_eq!(
        tasks.ingested_reads.load(Ordering::SeqCst),
        0,
        "NULL 所有者不许去问那个批次"
    );
    // 有批次 + 批次里有渠道消息 ⇒ Ours；没有 ⇒ NotOurs。
    let root = Id::new();
    let tasks = Arc::new(FakeTasks::with_task(
        task_row(task_id, Some(root), Some(session)),
        true,
    ));
    let dynamic: Arc<dyn TaskQueries> = Arc::clone(&tasks) as Arc<dyn TaskQueries>;
    assert_eq!(
        origin_of(Some(dynamic.as_ref()), Some(session), &task_id.to_string()).await,
        OriginVerdict::Ours
    );
    tasks.ingested.store(false, Ordering::SeqCst);
    assert_eq!(
        origin_of(Some(dynamic.as_ref()), Some(session), &task_id.to_string()).await,
        OriginVerdict::NotOurs
    );
    assert_eq!(
        tasks.ingested_reads.load(Ordering::SeqCst),
        2,
        "有批次时每一次判定都要花那一次读（Ours / NotOurs）"
    );
    // 读库失败 ⇒ Unknown（不是 NotOurs：一次够不着的库不是证据）。
    tasks.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        origin_of(Some(dynamic.as_ref()), Some(session), &task_id.to_string()).await,
        OriginVerdict::Unknown
    );
    // 没有那一行 ⇒ Unknown。
    let empty = Arc::new(FakeTasks::default());
    let dynamic: Arc<dyn TaskQueries> = Arc::clone(&empty) as Arc<dyn TaskQueries>;
    assert_eq!(
        origin_of(Some(dynamic.as_ref()), Some(session), &task_id.to_string()).await,
        OriginVerdict::Unknown
    );
}

/// 🔴 **寻址规则只有一条**：本片落的自由函数 `task_address`（上游 `typing_indicator.go` 用的那个）
/// 与 M7-17 落成方法的 `Outbound::task_address` 对**同一批行**必须给出同一个答案。
///
/// 上游两者在**同一个包**里共用同一个自由函数；本仓分在两片 ⇒ 这条等价比对就是"不会漂成两种读法"
/// 的证据（收敛的票登记在 `docs/32` §38 的 D10）。
#[tokio::test]
async fn the_addressing_rule_agrees_with_outbound() {
    let installation = Id::new();
    let shapes: Vec<(&str, FakeDeliveries, Arc<dyn OutboundQueries>)> = vec![
        (
            "没有行",
            FakeDeliveries::default(),
            Arc::new(QueriesOver(FakeDeliveries::default())),
        ),
        (
            "别的平台的行",
            FakeDeliveries::with_foreign_row(),
            Arc::new(QueriesOver(FakeDeliveries::with_foreign_row())),
        ),
        (
            "被撤销的安装",
            FakeDeliveries::with_revoked_row(installation),
            Arc::new(QueriesOver(FakeDeliveries::with_revoked_row(installation))),
        ),
        (
            "活的 WeCom 行",
            FakeDeliveries::with_live_row(installation, "room"),
            Arc::new(QueriesOver(FakeDeliveries::with_live_row(
                installation,
                "room",
            ))),
        ),
    ];
    for (name, mine, theirs) in shapes {
        let task_id = Id::new();
        let outbound = Outbound::new(Arc::clone(&theirs), None);
        let expected = outbound.task_address(task_id).await.expect("读得出来");
        let actual = crate::wecom::typing::events::task_address(&mine, task_id)
            .await
            .expect("读得出来");
        assert_eq!(actual.addr, expected.addr, "{name}：地址");
        assert_eq!(actual.ours, expected.ours, "{name}：是不是我们的");
        assert_eq!(
            actual.skip.is_some(),
            expected.skip.is_some(),
            "{name}：有没有一个值得计数的跳过原因"
        );
    }
}

/// 一个把 [`FakeDeliveries`] 顶成宽 [`OutboundQueries`] 的适配器（只为了让上面那条等价比对能调
/// `Outbound::task_address`）。
struct QueriesOver(FakeDeliveries);

#[async_trait]
impl OutboundQueries for QueriesOver {
    async fn get_task_delivery(&self, task_id: Id) -> Result<Option<TaskDelivery>, String> {
        self.0.task_delivery(task_id).await
    }

    async fn get_agent_task(&self, _task_id: Id) -> Result<Option<AgentTask>, String> {
        Ok(None)
    }

    async fn task_has_channel_ingested_messages(&self, _task_id: Id) -> Result<bool, String> {
        Ok(false)
    }

    async fn get_installation(
        &self,
        installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String> {
        self.0.installation_record(installation_id).await
    }

    async fn find_binding_for_member(
        &self,
        _workspace_id: Id,
        _multica_user_id: Id,
    ) -> Result<Option<crate::wecom::outbound::MemberBinding>, String> {
        Ok(None)
    }

    async fn workspace_slug(&self, _workspace_id: Id) -> Result<Option<String>, String> {
        Ok(None)
    }
}

// =====================================================================
// 同步接缝
// =====================================================================

/// 上游 §34.4 **H4** 那一条的落点：[`TaskLookupRoots`] 就是"把 task id 解回它输入批次的**所有者**"
/// 那一次读（自动重试的 clone 继承父亲的 `chat_input_task_id`）。
#[tokio::test]
async fn the_root_resolver_reads_the_input_batch() {
    let root = Id::new();
    let clone = Id::new();
    let tasks = Arc::new(FakeTasks::with_task(
        task_row(clone, Some(root), None),
        true,
    ));
    let resolver = TaskLookupRoots::new(Arc::clone(&tasks) as Arc<dyn TaskQueries>);
    assert_eq!(
        resolver.root_task_id(&clone.to_string()).await,
        Some(root.to_string())
    );
    // 解不出的 id / 没有那一行 / 读库失败 ⇒ 查不着（上游只记一条 debug：那是**正常**形态）。
    assert_eq!(resolver.root_task_id("not-a-uuid").await, None);
    assert_eq!(resolver.root_task_id("").await, None);
    let empty = Arc::new(FakeTasks::default());
    let resolver = TaskLookupRoots::new(Arc::clone(&empty) as Arc<dyn TaskQueries>);
    assert_eq!(resolver.root_task_id(&clone.to_string()).await, None);
    tasks.fail.store(true, Ordering::SeqCst);
    let resolver = TaskLookupRoots::new(Arc::clone(&tasks) as Arc<dyn TaskQueries>);
    assert_eq!(resolver.root_task_id(&clone.to_string()).await, None);
}

/// 🔴 **血缘**那一格：`retry_unbind` 之后那一轮记下的是"它在等**谁的名字**"，而一次**具名尝试**的
/// 收尾由那条血缘（`chat_input_task_id`）解开 —— 因为 clone 的 `task:queued` 与一次新问题的 run
/// 逐字节相同。
#[tokio::test]
async fn a_retry_clone_finds_its_round_through_the_blood_line() {
    let parent = Id::new();
    let clone = Id::new();
    let router = Arc::new(FakeRouter::default());
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(clone, Some(parent), Some(Id::new())),
            // 批次里**有**渠道递进来的消息 ⇒ `Ours` ⇒ 取消那条路才走到封口。
            true,
        ))
        .roots(Arc::new(crate::wecom::typing::TaskLookupRoots::new(
            Arc::new(FakeTasks::with_task(
                task_row(clone, Some(parent), Some(Id::new())),
                false,
            )) as Arc<dyn TaskQueries>,
        )))
        .router(Arc::clone(&router))
        .build();
    let _ = harness.open_round(&parent.to_string()).await;

    // 平台对一次可重试失败的答案是造一个 clone：气泡**留着**，那一轮交出父 id。
    harness
        .indicator
        .handle_task_failed(
            &TaskEvent::failed(
                parent.to_string(),
                Some(harness.session.to_string()),
                Some("boom"),
            )
            .with_retry_pending(true),
        )
        .await;
    assert_eq!(harness.indicator.depth(), 1, "气泡留着");
    assert!(rounds_task_ids(&harness.streams, harness.session)[0].is_empty());

    // clone 自己的收尾（它的 id 从没被绑过）由血缘解开 ⇒ 封在**那一轮**上，而不是一个普通消息。
    harness
        .indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            clone.to_string(),
            Some(harness.session.to_string()),
        ))
        .await;
    let frames = harness.senders.stream_frames();
    assert_eq!(frames.len(), 2, "开场帧 + 收尾帧");
    assert_eq!(frames[1].1, copy_for(Locale::ZhHans).stream_cancelled);
    assert!(frames[1].2);
    assert_eq!(harness.indicator.depth(), 0);
    assert!(router.frames().is_empty(), "本地就封掉了，不该走中继");
}
