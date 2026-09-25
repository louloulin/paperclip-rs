//! `telegram::delivery` 的用例（写者 M7-6）。
//!
//! 投递状态机是**表驱动**的：进程内替身（[`super::testing::FakeStore`]）把
//! "租约 CAS / 深度只前进 / phase 只前进 / unknown 不围栏 / 占位不是进度" 这几条变成
//! 可直接断言的对象。真库那半边（同一套语句的 PG 行为）在
//! `mc-repos/src/channel/delivery/tests.rs` 里另有用例。
//!
//! ⚠️ 替身替的是**存储**，不是**平台**：`docs/60` §4.2 第 1 条约束的是平台替身；
//! 端到端回路（真 DB + 真出站）在 `crates/mc-http/tests/channels/telegram.rs`。

use super::*;
use crate::telegram::delivery::testing::{ledger, target, FakeStore};

// ---------------------------------------------------------------------------
// 归类（纯函数，表驱动）
// ---------------------------------------------------------------------------

/// `classify_send` / 三个编辑谓词：**5xx 不算拒绝**是这里最容易被读错的一条。
#[test]
fn outcome_classification_is_table_driven() {
    let api = |code: u16, description: &str| ApiError::Api {
        method: "editMessageText",
        code,
        description: description.to_string(),
        retry_after: None,
    };
    let cases: &[(ApiError, bool, DeliveryOutcome)] = &[
        // 4xx = 平台回答并拒绝 ⇒ 聊天里什么都没有 ⇒ 可以再试。
        (
            api(400, "Bad Request: chat not found"),
            true,
            DeliveryOutcome::Refused,
        ),
        (
            api(403, "Forbidden: bot was blocked by the user"),
            true,
            DeliveryOutcome::Refused,
        ),
        (
            api(429, "Too Many Requests"),
            true,
            DeliveryOutcome::Refused,
        ),
        // 5xx = 平台可能已经收下了 ⇒ 不许当成"什么都没发"。
        (
            api(500, "Internal Server Error"),
            false,
            DeliveryOutcome::Unknown,
        ),
        (api(502, "Bad Gateway"), false, DeliveryOutcome::Unknown),
    ];
    for (error, definite, outcome) in cases {
        assert_eq!(is_definite_rejection(error), *definite, "{error:?}");
        assert_eq!(classify_send(Err(error)), *outcome, "{error:?}");
    }
    // 传输失败 / 解析失败 / 409：都不是"平台拒绝"的证据。
    for error in [
        ApiError::Transport {
            method: "sendMessage",
        },
        ApiError::Malformed {
            method: "sendMessage",
        },
        ApiError::Conflict,
    ] {
        assert!(!is_definite_rejection(&error), "{error:?}");
        assert_eq!(classify_send(Err(&error)), DeliveryOutcome::Unknown);
    }
    assert_eq!(classify_send(Ok(())), DeliveryOutcome::Accepted);
}

/// 三个编辑谓词各自只认自己的那一种 400。
#[test]
fn edit_predicates_are_table_driven() {
    let api = |code: u16, description: &str| ApiError::Api {
        method: "editMessageText",
        code,
        description: description.to_string(),
        retry_after: None,
    };
    let not_modified = api(400, "Bad Request: message is not modified");
    let missing = api(400, "Bad Request: message to edit not found");
    let markup = api(400, "Bad Request: can't parse entities");

    assert!(is_not_modified(&not_modified));
    assert!(!is_not_modified(&missing));
    // "目标没了"是**唯一**值得另发一条新消息的编辑失败。
    assert!(is_edit_target_missing(&missing));
    assert!(!is_edit_target_missing(&not_modified));
    assert!(!is_edit_target_missing(&markup), "markup 错不能另发");
    // 永久拒绝 = 重试修不好。它是那道**阶梯的最后一步**，而三种 400 共用状态码 ⇒
    // 调用方必须**先**问"是 not-modified 吗""目标没了吗"，剩下才算永久（上游注释逐字）。
    assert!(is_permanent_edit_rejection(&markup));
    assert!(
        is_permanent_edit_rejection(&not_modified),
        "共用 400，靠阶梯顺序区分"
    );
    assert!(
        is_permanent_edit_rejection(&missing),
        "共用 400，靠阶梯顺序区分"
    );
    assert!(is_permanent_edit_rejection(&api(403, "Forbidden")));
    assert!(is_permanent_edit_rejection(&api(401, "Unauthorized")));
    // 5xx 与传输失败都**不**是永久拒绝：它们连"平台回答了"都不成立。
    assert!(!is_permanent_edit_rejection(&api(
        500,
        "Internal Server Error"
    )));
    assert!(!is_permanent_edit_rejection(&ApiError::Transport {
        method: "editMessageText"
    }));
}

// ---------------------------------------------------------------------------
// 取租约的三分判决
// ---------------------------------------------------------------------------

/// 取租约的三分判决：`Acquired` / `Busy`（等） / `Closed`（停），一个表。
#[tokio::test]
async fn acquire_verdicts_are_table_driven() {
    // 1) 第一次取 ⇒ Acquired，且行是 streaming/none。
    let (store, ledger) = ledger();
    let turn = ReplyTurn {
        id: Id::new(),
        depth: 0,
    };
    let (lease, status) = ledger
        .acquire(&target(), turn, PHASE_STREAMING)
        .await
        .expect("acquire");
    assert_eq!(status, DeliveryStatus::Acquired);
    let lease = lease.expect("lease");
    assert_eq!(lease.send_state(), SEND_NONE);
    assert_eq!(lease.message_id(), 0, "还没有可编辑的消息");
    assert_eq!(lease.chunks_sent(), 0);

    // 2) 别人（活着）持有 ⇒ Busy。
    let (other, status) = ledger
        .acquire(&target(), turn, PHASE_STREAMING)
        .await
        .expect("busy");
    assert_eq!(status, DeliveryStatus::Busy);
    assert!(other.is_none(), "活着的不被抢");

    // 3) 过期 ⇒ 接管，且深度只前进。
    store.expire(turn.id);
    let deeper = ReplyTurn {
        id: turn.id,
        depth: 1,
    };
    let (successor, status) = ledger
        .acquire(&target(), deeper, PHASE_TERMINAL)
        .await
        .expect("takeover");
    assert_eq!(status, DeliveryStatus::Acquired);
    assert!(successor.is_some());

    // 4) 更浅的旧尝试再也拿不回来 ⇒ Closed（它是被**取代**了，不是"在等"）。
    store.expire(turn.id);
    let (stale, status) = ledger
        .acquire(&target(), turn, PHASE_TERMINAL)
        .await
        .expect("stale");
    assert_eq!(status, DeliveryStatus::Closed, "深度倒退 = 已被取代");
    assert!(stale.is_none());

    // 5) terminal 之后 streaming 不许夺回（最终答案已经接管）⇒ Closed。
    let (blocked, status) = ledger
        .acquire(&target(), deeper, PHASE_STREAMING)
        .await
        .expect("streaming after terminal");
    assert_eq!(status, DeliveryStatus::Closed, "terminal 之后不许重开占位");
    assert!(blocked.is_none());

    // 6) 收口 ⇒ 谁都不许再取。
    let _ = ledger.settle(&successor.expect("lease"), "delivered").await;
    let (_, status) = ledger
        .acquire(&target(), deeper, PHASE_TERMINAL)
        .await
        .expect("settled");
    assert_eq!(status, DeliveryStatus::Closed);
}

// ---------------------------------------------------------------------------
// 记账
// ---------------------------------------------------------------------------

/// **占位不是进度**：占位落地不动 `chunks_sent`；最终答案的片才动，且 `message_id` 不改指向。
#[tokio::test]
async fn placeholder_and_progress_are_recorded_separately() {
    let (store, ledger) = ledger();
    let turn = ReplyTurn {
        id: Id::new(),
        depth: 0,
    };
    let (lease, _) = ledger
        .acquire(&target(), turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    let lease = lease.expect("lease");

    // 发送前先公开 ⇒ 别的进程读到"有一条在飞"。
    assert!(ledger.claim_send(&lease).await.expect("claim"));
    ledger
        .record_send(&lease, true, 7, 0, DeliveryOutcome::Accepted)
        .await
        .expect("placeholder");
    let after_placeholder = store.row(turn.id);
    assert_eq!(after_placeholder.send_state, SEND_KNOWN);
    assert_eq!(after_placeholder.message_id, "7");
    assert_eq!(after_placeholder.chunks_sent, 0, "占位不是进度");

    // 第一片：`message_id` 已经指向占位消息 ⇒ 不漂移；进度前进。
    assert!(ledger.claim_send(&lease).await.expect("claim 2"));
    ledger
        .record_send(&lease, false, 99, 1, DeliveryOutcome::Accepted)
        .await
        .expect("chunk");
    let after_chunk = store.row(turn.id);
    assert_eq!(after_chunk.message_id, "7", "后续片不改编辑目标");
    assert_eq!(after_chunk.chunks_sent, 1);

    // 平台拒绝 ⇒ 回到可再试的状态（已有占位消息 ⇒ `known`，不是 `none`）。
    assert!(ledger.claim_send(&lease).await.expect("claim 3"));
    ledger
        .record_send(&lease, false, 0, 1, DeliveryOutcome::Refused)
        .await
        .expect("refused");
    assert_eq!(store.row(turn.id).send_state, SEND_KNOWN);

    // 结果未知 ⇒ 停在 `unknown`（重发无法被平台去重），并且**没有** owner 围栏。
    assert!(ledger.claim_send(&lease).await.expect("claim 4"));
    ledger
        .record_send(&lease, false, 0, 1, DeliveryOutcome::Unknown)
        .await
        .expect("unknown");
    let unknown = store.row(turn.id);
    assert_eq!(unknown.send_state, SEND_UNKNOWN);
    assert_eq!(
        store.writes().last().map(String::as_str),
        Some("mark_send_unknown")
    );
}

/// 接手一个前持有者死在发送中途的轮次：**不许**当成"什么都没发"。
#[tokio::test]
async fn an_inherited_in_flight_send_becomes_unknown() {
    let (store, ledger) = ledger();
    let turn = ReplyTurn {
        id: Id::new(),
        depth: 0,
    };
    let (first, _) = ledger
        .acquire(&target(), turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    let first = first.expect("lease");
    assert!(ledger.claim_send(&first).await.expect("claim"));
    // 持有者死在发送中途：租约过期，`send_state` 停在 `in_flight`。
    store.expire(turn.id);
    let deeper = ReplyTurn {
        id: turn.id,
        depth: 1,
    };
    let (successor, status) = ledger
        .acquire(&target(), deeper, PHASE_TERMINAL)
        .await
        .expect("takeover");
    assert_eq!(status, DeliveryStatus::Acquired);
    let successor = successor.expect("lease");
    assert_eq!(successor.send_state(), SEND_IN_FLIGHT);
    assert!(
        ledger.inherited_send(&successor).await.expect("inherit"),
        "在飞的发送必须记成未知"
    );
    assert_eq!(store.row(turn.id).send_state, SEND_UNKNOWN);
    // 已经是 `unknown` ⇒ 幂等（再读一次仍然说"有前情"）。
    assert!(ledger.inherited_send(&successor).await.expect("again"));
    // 干净的轮次什么都不用记。
    let (clean, _) = ledger
        .acquire(
            &target(),
            ReplyTurn {
                id: Id::new(),
                depth: 0,
            },
            PHASE_TERMINAL,
        )
        .await
        .expect("acquire");
    assert!(!ledger
        .inherited_send(&clean.expect("lease"))
        .await
        .expect("clean"));
}

// ---------------------------------------------------------------------------
// 轮次血缘 / 收口 / 时间预算
// ---------------------------------------------------------------------------

/// 轮次血缘：没有队列行的任务**就是自己的轮次、深度 0**（那是事实，不是失败）。
#[tokio::test]
async fn a_task_without_a_queue_row_is_its_own_turn() {
    let (store, ledger) = ledger();
    let orphan = Id::new();
    let turn = ledger.turn_for(orphan).await.expect("turn");
    assert_eq!(turn.id, orphan);
    assert_eq!(turn.depth, 0);

    let retry = Id::new();
    let root = Id::new();
    store.chain(retry, root, 1);
    let inherited = ledger.turn_for(retry).await.expect("turn");
    assert_eq!(inherited.id, root, "自动重试接着前一次开始的投递");
    assert_eq!(inherited.depth, 1);
}

/// 收口：没有答案的轮次也要建行（否则取消之后到的第一帧会开出一个没人收尾的占位消息）。
#[tokio::test]
async fn closing_an_empty_turn_creates_the_row() {
    let (store, ledger) = ledger();
    let turn = ReplyTurn {
        id: Id::new(),
        depth: 0,
    };
    assert!(ledger
        .close_turn(&target(), turn, "cancelled")
        .await
        .expect("close"));
    let stored = store.row(turn.id);
    assert_eq!(stored.phase, PHASE_SETTLED);
    assert_eq!(stored.settled_reason, "cancelled");
    assert_eq!(stored.send_state, SEND_NONE);

    // 已经收口 ⇒ **幂等**：仍然报"这一轮现在确实收口了"（上游 `closeTurn` 的回报语义就是
    // "结局是收口"，不是"这次调用关掉的"），但原因不被改写。
    assert!(ledger
        .close_turn(&target(), turn, "again")
        .await
        .expect("again"));
    assert_eq!(store.row(turn.id).settled_reason, "cancelled");

    // 被更深的尝试接管 ⇒ 认为"这一轮结束了"（等着只会把会话队列按住）。
    let deeper_turn = ReplyTurn {
        id: Id::new(),
        depth: 3,
    };
    let (deeper_lease, _) = ledger
        .acquire(&target(), deeper_turn, PHASE_TERMINAL)
        .await
        .expect("acquire deeper");
    assert!(deeper_lease.is_some());
    let stale = ledger
        .close_turn(
            &target(),
            ReplyTurn {
                id: deeper_turn.id,
                depth: 1,
            },
            "stale",
        )
        .await
        .expect("stale close");
    assert!(stale, "已被取代的尝试收口算完成");
    assert_ne!(
        store.row(deeper_turn.id).phase,
        PHASE_SETTLED,
        "而且**没有**真的收口别人正在投递的那一轮"
    );
}

/// 时间预算：一次调用必须装得进租约，且租约要长过一次 Telegram 往返的量级。
#[test]
fn the_call_budget_fits_inside_the_lease() {
    let store = Arc::new(FakeStore::default());
    let ledger = DeliveryLedger::new(store);
    assert!((ledger.lease_seconds() - 30.0).abs() < f64::EPSILON);
    assert_eq!(ledger.call_budget(), CALL_TIMEOUT);
    assert!(
        ledger.call_budget() + RECORD_TIMEOUT <= LEASE_TTL,
        "一次调用 + 记录结果必须装进租约"
    );
    // 租约被调短（用例检验接管）⇒ 调用预算跟着缩，否则被检验的关系不成立。
    let short = DeliveryLedger::new(Arc::new(FakeStore::default()))
        .with_lease_ttl(Duration::from_millis(600));
    assert_eq!(short.call_budget(), Duration::from_millis(200));
    assert!(short.call_budget() < short.lease_ttl);
    // 预算常量本身的关系（上游注释逐字：TTL 是"死进程最多挡住这一轮多久"）。
    // 两条常量关系：`map_or` 是运行时不变量，`const` 块里的是编译期断言。
    assert!(
        u128::from(MAX_ACQUIRE_ATTEMPTS) * BUSY_RETRY.as_millis() >= LEASE_TTL.as_millis(),
        "等租约到期的预算必须够长"
    );
    const { assert!(MAX_CLAIM_ERROR_ATTEMPTS < MAX_ACQUIRE_ATTEMPTS) };
}

/// 租约的围栏：别人的令牌写不动；交还与续租也都围栏在令牌上。
#[tokio::test]
async fn every_write_goes_through_the_lease() {
    let (store, ledger) = ledger();
    let turn = ReplyTurn {
        id: Id::new(),
        depth: 0,
    };
    let (lease, _) = ledger
        .acquire(&target(), turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    let lease = lease.expect("lease");
    let stranger = DeliveryLease {
        turn_id: lease.turn_id(),
        token: Id::new(),
        row: lease.row().clone(),
    };
    assert!(!store
        .claim_send(lease.turn_id(), stranger.token())
        .await
        .expect("stranger"));
    assert!(!ledger.claim_send(&stranger).await.expect("stranger claim"));
    assert!(!ledger.release(&stranger).await.expect("stranger release"));
    assert!(!ledger.renew(&stranger).await.expect("stranger renew"));
    assert!(ledger.renew(&lease).await.expect("own renew"));
    assert!(ledger.release(&lease).await.expect("own release"));
    // 交还之后下一条路径**立刻**能拿到（不必等租约到期）。
    let (next, status) = ledger
        .acquire(&target(), turn, PHASE_TERMINAL)
        .await
        .expect("acquire");
    assert_eq!(status, DeliveryStatus::Acquired);
    assert!(next.is_some());
}
