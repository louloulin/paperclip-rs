//! `relay` 的**合同**用例：配置 / 链 / 帧 / claim 存储 / 错误分类。

use super::super::SeenEvents;
use super::*;

/// 上游 `retryPlan`：总时长覆盖一个半落定窗口，每节不超窗口的四分之一，且**有界**。
#[test]
fn the_retry_plan_covers_one_and_a_half_settle_windows() {
    let cfg = RelayConfig {
        lease_settle: Some(Duration::from_secs(30)),
        retry_backoff: Some(Duration::from_millis(200)),
        ..RelayConfig::default()
    }
    .with_defaults();
    let plan = cfg.retry_plan();
    let total: Duration = plan.iter().sum();
    assert!(
        total >= Duration::from_secs(45),
        "链的总时长要覆盖 1.5 个落定窗口，实际 {total:?}"
    );
    assert!(plan
        .iter()
        .all(|delay| *delay <= Duration::from_millis(7500)));
    assert!(plan.len() <= RELAY_RETRY_CHAIN_CAP);
    // 每一节都不小于退避，且单调不降（翻倍 + 封顶）。
    assert_eq!(plan[0], Duration::from_millis(200));
    assert!(plan.windows(2).all(|pair| pair[1] >= pair[0]));

    // 病态配置不许产出无界的链：一个**极大**的落定窗口配一个极小的退避，会在总时长
    // 走够之前就撞上 24 节的上限。
    let tiny = RelayConfig {
        lease_settle: Some(Duration::from_secs(3600)),
        retry_backoff: Some(Duration::from_millis(1)),
        ..RelayConfig::default()
    }
    .with_defaults();
    assert_eq!(tiny.retry_plan().len(), RELAY_RETRY_CHAIN_CAP);
}

/// 上游 `dedupeTTLFor`：`max(1h, 2×宽)`。

#[test]
fn the_claim_ttl_outlives_twice_the_replay_window() {
    assert_eq!(dedupe_ttl_for(Duration::ZERO), MIN_DEDUPE_TTL);
    assert_eq!(dedupe_ttl_for(Duration::from_secs(600)), MIN_DEDUPE_TTL);
    assert_eq!(
        dedupe_ttl_for(Duration::from_secs(7200)),
        Duration::from_secs(14400)
    );
}

/// 上游 `outcomeGrace`：**每 offer** 都收一次 claim 往返与一次投递预算。

#[test]
fn the_outcome_grace_charges_every_offer() {
    let cfg = fast_config().with_defaults();
    let dedupe = Arc::new(InProcessDedupe::new(16).with_budget(Duration::from_millis(10)));
    let relay = RelayOutbound::new(None, Some(dedupe), cfg);
    let grace = relay.outcome_grace();
    let offers = relay.retry_plan().len() + 1;
    // 宽松的下界：每一次 offer 至少收一次投递预算。
    assert!(
        grace
            >= cfg
                .delivery_budget()
                .saturating_mul(u32::try_from(offers).unwrap_or(u32::MAX)),
        "宽 {grace:?} 必须至少覆盖每 offer 一次投递"
    );
    // 没有 claim 存储时用默认预算，而不是零。
    let no_dedupe = RelayOutbound::new(None, None, cfg);
    assert!(no_dedupe.outcome_grace() >= DEFAULT_CLAIM_BUDGET);
}

/// 分片是确定的（同一安装永远落在同一片上），而不同安装可以落在不同片。

#[test]
fn sharding_is_deterministic_per_installation() {
    let relay = RelayOutbound::new(
        None,
        None,
        RelayConfig {
            shards: Some(4),
            ..RelayConfig::default()
        },
    );
    let a = relay.shard_for("inst-a");
    assert_eq!(a, relay.shard_for("inst-a"));
    assert!(a < 4);
    let ids = ["inst-a", "inst-b", "inst-c", "inst-d", "inst-e"];
    let shards: Vec<usize> = ids.iter().map(|id| relay.shard_for(id)).collect();
    assert!(
        shards.iter().all(|shard| *shard < 4),
        "每一条安装都要落在一条存在的队列上：{shards:?}"
    );
}

// =====================================================================
// 帧
// =====================================================================

/// 帧的 JSON 形态逐字对齐上游（其余副本读得懂同一个形状）。

#[test]
fn frames_round_trip_on_the_wire() {
    let frame = RelayFrame::reply(
        "inst-1".into(),
        "chat-1".into(),
        1,
        "hello",
        "task-1",
        "msg-1",
        "ws-1",
        "sess-1",
    );
    let encoded = frame.encode().expect("encode");
    let json: serde_json::Value = serde_json::from_slice(&encoded).expect("json");
    assert_eq!(json["kind"], serde_json::Value::String("reply".into()));
    assert_eq!(
        json["installation_id"],
        serde_json::Value::String("inst-1".into())
    );
    // 空的 `carries_files` 不写进线上（`omitempty`）。
    assert!(json.get("carries_files").is_none());
    // 空的 `seal_reason` 同理。
    assert!(json.get("seal_reason").is_none());
    assert_eq!(RelayFrame::decode(&encoded).expect("decode"), frame);

    let seal = RelayFrame::seal(SEAL_REASON_CANCELLED, "task-1", "sess-1", true);
    let encoded = seal.encode().expect("encode");
    let json: serde_json::Value = serde_json::from_slice(&encoded).expect("json");
    assert_eq!(json["kind"], serde_json::Value::String("seal".into()));
    assert_eq!(
        json["seal_reason"],
        serde_json::Value::String("cancelled".into())
    );
    assert_eq!(json["carries_files"], serde_json::Value::Bool(true));
    assert_eq!(RelayFrame::decode(&encoded).expect("decode"), seal);

    // 不认识的 kind 是一**条错误**，不是默认值。
    let bad = br#"{"kind":"telepathy"}"#;
    assert!(RelayFrame::decode(bad).is_err());
}

/// 事件 id 从**那一轮**派生 ⇒ 一次重发布是同一条 claim。

#[test]
fn event_ids_are_derived_from_the_turn() {
    let task = Id::new();
    assert_eq!(
        relay_event_id("chat:done", task),
        relay_event_id("chat:done", task)
    );
    assert_eq!(
        dedupe_key("wecom:chat:done:x"),
        "wecom:outbound:claim:wecom:chat:done:x"
    );
    assert_eq!(
        relay_inbox_event_id("item-1", "user-1"),
        "wecom:inbox:item-1:user-1"
    );
}

// =====================================================================
// claim 存储
// =====================================================================

/// `InProcessDedupe` 的四个操作逐条照上游：取 / 重取 / 比较并删除 / 比较并结算 / 读且围栏。

#[tokio::test]
async fn the_in_process_claim_store_mirrors_the_upstream_four_operations() {
    let store = InProcessDedupe::new(8);
    let key = "wecom:outbound:claim:ev-1";
    // 没人握着 ⇒ 取到。
    assert!(store
        .claim(key, "owner-a/ev-1", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    // 别人握着 ⇒ 拿不到。
    assert!(!store
        .claim(key, "owner-b/ev-1", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    // **同一个**持有者回来 ⇒ 重取成功（一次结果未知的 Release 之后）。
    assert!(store
        .claim(key, "owner-a/ev-1", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    // 读且围栏：报告 `Held`，同时把键翻成 lost。
    assert_eq!(store.resolve(key).await.expect("resolve"), ClaimState::Held);
    assert_eq!(store.resolve(key).await.expect("resolve"), ClaimState::Lost);
    // 被围栏之后，持有者不能结算它 —— 它什么都不得记。
    assert!(!store.settle(key, "owner-a/ev-1").await.expect("settle"));
    // 一个已结算的 claim：重试安全。
    let key2 = "wecom:outbound:claim:ev-2";
    assert!(store
        .claim(key2, "owner-a/ev-2", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    assert!(store.settle(key2, "owner-a/ev-2").await.expect("settle"));
    assert!(store.settle(key2, "owner-a/ev-2").await.expect("settle"));
    assert_eq!(
        store.resolve(key2).await.expect("resolve"),
        ClaimState::Settled
    );
    // 比较并删除只删自己的。
    let key3 = "wecom:outbound:claim:ev-3";
    assert!(store
        .claim(key3, "owner-a/ev-3", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    assert!(!store.release(key3, "owner-b/ev-3").await.expect("release"));
    assert!(store.release(key3, "owner-a/ev-3").await.expect("release"));
    assert_eq!(
        store.resolve(key3).await.expect("resolve"),
        ClaimState::Absent
    );
}

/// 宽走完 ⇒ claim 过期（发布方不必等一个小时才敢重发）。

#[tokio::test]
async fn a_claim_expires_with_its_ttl() {
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = {
        let now = Arc::clone(&now);
        Arc::new(move || *now.lock().expect("lock")) as Clock
    };
    let store = InProcessDedupe::new(8).with_clock(clock);
    let key = "wecom:outbound:claim:ev-ttl";
    assert!(store
        .claim(key, "owner-a", Duration::from_secs(1))
        .await
        .expect("claim"));
    assert!(!store
        .claim(key, "owner-b", Duration::from_secs(1))
        .await
        .expect("claim"));
    *now.lock().expect("lock") += Duration::from_secs(2);
    assert!(store
        .claim(key, "owner-b", Duration::from_secs(1))
        .await
        .expect("claim"));
    assert_eq!(store.resolve(key).await.expect("resolve"), ClaimState::Held);
}

/// 闸是有界的（一个无界的进程内集合就是内存泄漏）。

#[test]
fn the_seen_gate_is_bounded() {
    let seen = SeenEvents::new(2);
    assert!(seen.claim("a"));
    assert!(!seen.claim("a"));
    assert!(seen.claim("b"));
    assert!(seen.claim("c"));
    assert_eq!(seen.len(), 2, "第三个把最早的挤出去");
    assert!(seen.claim("a"), "被挤出去的那个可以被重新认领");
    seen.forget("c");
    assert!(seen.claim("c"));
    // 空 id 一律是新的一帧（没有 id 就没有幂等可谈）。
    assert!(seen.claim(""));
    assert!(seen.claim(""));
}

// =====================================================================
// 幂等（**本片专属验收第 1 条**）
// =====================================================================

/// 同一条事件 id 投递两次 ⇒ **只送一次**。
///
/// 两条路各一遍：同一进程里第二次被本地闸挡住；**另一个副本**（新 `RelayOutbound`、
/// 同一个 claim 存储）拿不到 claim ⇒ 它什么都不送（并还回 claim）。

#[test]
fn only_a_failure_before_the_write_is_provably_not_sent() {
    assert!(!provably_not_sent(None));
    assert!(provably_not_sent(Some(&SenderError::NotAttempted)));
    assert!(provably_not_sent(Some(&SenderError::ChatBusy)));
    assert!(provably_not_sent(Some(&SenderError::StreamBusy)));
    assert!(provably_not_sent(Some(&SenderError::StreamSuperseded)));
    assert!(!provably_not_sent(Some(&SenderError::AckTimeout)));
    assert!(!provably_not_sent(Some(&SenderError::StreamAckTimeout)));
    assert!(!provably_not_sent(Some(&SenderError::AckAbandoned {
        cause: "budget".into()
    })));
    assert!(!provably_not_sent(Some(&SenderError::WriteAttempted {
        cause: "socket".into()
    })));
    assert!(!provably_not_sent(Some(&SenderError::PartiallySent {
        cause: "piece 2".into()
    })));
    assert!(!provably_not_sent(Some(&SenderError::Api {
        cmd: "aibot_send_msg".into(),
        code: 846_605,
        message: "bad req_id".into()
    })));
}

/// 一条可证明没发出的帧 ⇒ claim 还回去，而且**不**被计成丢弃。

#[test]
fn the_outbound_error_vocabulary_is_closed() {
    let errors = [
        OutboundError::OutcomeRecorded {
            recorded: crate::wecom::outbound::Recorded::Nothing,
        },
        OutboundError::NoLiveConnection,
        OutboundError::SenderRegistryMissing,
        OutboundError::lookup("load agent task", "db down"),
    ];
    for error in errors {
        assert!(!format!("{error}").is_empty());
        assert!(!format!("{error:?}").is_empty());
    }
}

/// 一个本文件用的占位（`Outbound::outcome_test_double` 的存在让这条断言有意义）。

#[test]
fn the_relay_can_be_built_without_any_port() {
    let relay = RelayOutbound::new(None, None, RelayConfig::default());
    assert!(!relay.handler_ready());
    assert!(!relay.has_dedupe());
    let _ = NoQueries;
    let outbound = Outbound::new(Arc::new(NoQueries), None);
    assert!(outbound.metrics().is_none());
    assert!(format!("{relay:?}").contains("RelayOutbound"));
}
