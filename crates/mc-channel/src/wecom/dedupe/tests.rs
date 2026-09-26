//! `dedupe` 的用例：上游 `dedupe_redis.go` 三张 Lua 脚本**逐格**，加上本仓新增的容量界与
//! 诊断面。
//!
//! 时钟是**注入**的（[`ManualClock`]）：TTL 与 tombstone 的边界不该靠睡真觉去撞。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

/// 可手动推进的时钟（`Instant` 没有零值 ⇒ 从一个真实起点往前推）。
#[derive(Clone)]
struct ManualClock {
    base: Instant,
    offset: Arc<AtomicU64>,
}

impl ManualClock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            offset: Arc::new(AtomicU64::new(0)),
        }
    }

    fn advance(&self, delta: Duration) {
        let millis = u64::try_from(delta.as_millis()).unwrap_or(u64::MAX);
        self.offset.fetch_add(millis, Ordering::SeqCst);
    }

    fn clock(&self) -> Clock {
        let base = self.base;
        let offset = Arc::clone(&self.offset);
        Arc::new(move || base + Duration::from_millis(offset.load(Ordering::SeqCst)))
    }
}

fn store(clock: &ManualClock) -> WeComClaimStore {
    WeComClaimStore::new(64).with_clock(clock.clock())
}

const TTL: Duration = Duration::from_secs(600);

// =====================================================================
// 值域
// =====================================================================

/// `is_token_shaped` 的真值表：**每一个真 token 都带 `/`**，而两个 tombstone 与 legacy 的裸 `1`
/// 都不带 —— 这正是"两种形态不需要版本位就能分辨"的全部依据。
#[test]
fn only_tokens_carry_the_separator() {
    for token in ["a1b2/owner-1", "/", "x/y/z"] {
        assert!(is_token_shaped(token), "{token}");
    }
    for other in ["", "1", "settled", "lost", "0123456789abcdef"] {
        assert!(!is_token_shaped(other), "{other}");
    }
    assert!(!is_token_shaped(CLAIM_SETTLED_VALUE));
    assert!(!is_token_shaped(CLAIM_LOST_VALUE));
    assert!(!is_token_shaped(LEGACY_CLAIM_VALUE));
    // 两个 tombstone 与 legacy 值互不相同 —— 否则 `resolve` 的格子会互相撞。
    assert_ne!(CLAIM_SETTLED_VALUE, CLAIM_LOST_VALUE);
    assert_ne!(CLAIM_SETTLED_VALUE, LEGACY_CLAIM_VALUE);
}

/// 键由**事件 id** 派生（上游逐字：*keyed on the same event id the stream entry carries*），
/// 而且与中继那一份命名是**同一个**字符串。
#[test]
fn the_key_is_the_relay_key_for_the_same_event() {
    assert_eq!(claim_key("evt-7"), super::super::relay::dedupe_key("evt-7"));
    assert_ne!(claim_key("evt-7"), claim_key("evt-8"));
    assert!(!claim_key("evt-7").is_empty());
}

// =====================================================================
// claim / release
// =====================================================================

/// `redisClaimSource` 的三格：absent ⇒ 取到；**同一个 token ⇒ 再取一次也是取到**（一个结局未知的
/// Release 之后持有者回来）；别的 token ⇒ 取不到，而且**不改**键上的任何东西。
#[tokio::test]
async fn claim_grants_absent_and_the_same_token_but_never_steals() {
    let clock = ManualClock::new();
    let store = store(&clock);
    let key = claim_key("evt-1");

    assert!(store.claim(&key, "aaaa/one", TTL).await.expect("claim"));
    assert_eq!(store.raw_value(&key).as_deref(), Some("aaaa/one"));

    // 同一个 token 再取一次 —— 这是"重发一条命令永远不会作用在一个更晚的持有者身上"的另一半。
    assert!(store.claim(&key, "aaaa/one", TTL).await.expect("re-claim"));
    assert_eq!(store.raw_value(&key).as_deref(), Some("aaaa/one"));

    // 另一个持有者拿不到，且键上的值一字未改。
    assert!(!store.claim(&key, "bbbb/two", TTL).await.expect("steal"));
    assert_eq!(store.raw_value(&key).as_deref(), Some("aaaa/one"));

    // 过期之后它又能被取到。
    clock.advance(TTL + Duration::from_secs(1));
    assert!(store.claim(&key, "bbbb/two", TTL).await.expect("after ttl"));
    assert_eq!(store.raw_value(&key).as_deref(), Some("bbbb/two"));
}

/// `redisReleaseSource`：一次**比较并删除**。错的 token 什么都不删（"一次可证明没发生的投递"才
/// 该把 claim 还回去）。
#[tokio::test]
async fn release_is_a_compare_and_delete_on_the_token() {
    let clock = ManualClock::new();
    let store = store(&clock);
    let key = claim_key("evt-2");
    assert!(store.claim(&key, "aaaa/one", TTL).await.expect("claim"));

    assert!(!store.release(&key, "bbbb/two").await.expect("wrong token"));
    assert_eq!(store.raw_value(&key).as_deref(), Some("aaaa/one"));

    assert!(store.release(&key, "aaaa/one").await.expect("right token"));
    assert_eq!(store.raw_value(&key), None);

    // 再删一次报 false 而无错 —— 删除已经落地，**不是**失败。
    assert!(!store.release(&key, "aaaa/one").await.expect("second"));
}

// =====================================================================
// settle
// =====================================================================

/// `redisSettleSource`：**只有**握着它的 token 才结算；结算一个已经结算的 claim 报 `true`
/// （重试安全）；发布方已经把它解成 lost 之后再结算报 `false`（那持有者什么都不得记）。
#[tokio::test]
async fn settle_needs_the_token_and_is_idempotent_for_a_settled_claim() {
    let clock = ManualClock::new();
    let store = store(&clock);
    let key = claim_key("evt-3");
    assert!(store.claim(&key, "aaaa/one", TTL).await.expect("claim"));

    assert!(!store
        .settle(&key, "bbbb/two")
        .await
        .expect("someone else's token"));
    assert_eq!(store.raw_value(&key).as_deref(), Some("aaaa/one"));

    assert!(store.settle(&key, "aaaa/one").await.expect("settle"));
    assert_eq!(store.raw_value(&key).as_deref(), Some(CLAIM_SETTLED_VALUE));
    assert!(store.settle(&key, "aaaa/one").await.expect("re-settle"));

    // 发布方围栏之后，原持有者的结算被拒 ⇒ 这条回复只以一个记录结束。
    let fenced = claim_key("evt-4");
    assert!(store
        .claim(&fenced, "cccc/three", TTL)
        .await
        .expect("claim"));
    assert_eq!(
        store.resolve(&fenced).await.expect("resolve"),
        ClaimState::Held
    );
    assert!(!store.settle(&fenced, "cccc/three").await.expect("fenced"));
}

// =====================================================================
// resolve：读**且**围栏
// =====================================================================

/// `redisResolveSource` 的四格：absent / settled / lost / **held ⇒ 报告的同时翻成 lost**。
#[tokio::test]
async fn resolve_reads_and_fences_in_one_operation() {
    let clock = ManualClock::new();
    let store = store(&clock);

    let absent = claim_key("evt-5");
    assert_eq!(
        store.resolve(&absent).await.expect("absent"),
        ClaimState::Absent
    );

    let held = claim_key("evt-6");
    assert!(store.claim(&held, "aaaa/one", TTL).await.expect("claim"));
    assert_eq!(store.resolve(&held).await.expect("first"), ClaimState::Held);
    // 围栏已经落地 ⇒ 第二次读到的是 lost，而不是一个"还握着"的键。
    assert_eq!(store.raw_value(&held).as_deref(), Some(CLAIM_LOST_VALUE));
    assert_eq!(
        store.resolve(&held).await.expect("second"),
        ClaimState::Lost
    );

    let settled = claim_key("evt-7");
    assert!(store.claim(&settled, "aaaa/one", TTL).await.expect("claim"));
    assert!(store.settle(&settled, "aaaa/one").await.expect("settle"));
    assert_eq!(
        store.resolve(&settled).await.expect("settled"),
        ClaimState::Settled
    );
}

/// 🔴 **上游 `resolve` 的 legacy 那一格**（`string.find(v, '/', 1, true)`）。
///
/// 一个**不是 token 形状**的值是这套方案之前那条认领（裸 `1` 的 `SET NX`），它的持有者**已经
/// 就地记过自己的结局** ⇒ 报 `Settled` 并**把这个键原样留着**。围栏它会给一条已经计过数的回复
/// 再记一次结局 —— 这正是上游那三行注释要防的事。
///
/// 本仓**够不着**这一格（进程内存储从空表开始、本文件从不写 `1`）⇒ 用例直接种一个 legacy 键，
/// 用的就是同一份读路径。
#[tokio::test]
async fn a_legacy_value_resolves_as_settled_and_is_left_alone() {
    let clock = ManualClock::new();
    let store = store(&clock);
    let key = claim_key("legacy-1");
    store.write(&key, LEGACY_CLAIM_VALUE.to_string(), TTL);

    assert_eq!(
        store.resolve(&key).await.expect("legacy"),
        ClaimState::Settled
    );
    // **原样留着** —— 这一条才是那一格的全部主张。
    assert_eq!(
        store.raw_value(&key).as_deref(),
        Some(LEGACY_CLAIM_VALUE),
        "legacy 值必须一字未改"
    );
    // 再读一次还是同一格（没有变成 lost，也没有被 delete）。
    assert_eq!(
        store.resolve(&key).await.expect("legacy again"),
        ClaimState::Settled
    );
    assert_eq!(store.raw_value(&key).as_deref(), Some(LEGACY_CLAIM_VALUE));
}

/// 上游那两条 tombstone 常量也带 TTL：一个 settled / lost 的键过一段时间之后重新变成 absent
/// （否则一次重启之前的事件 id 会被永久记住）。
#[tokio::test]
async fn tombstones_expire_too() {
    let clock = ManualClock::new();
    let store = store(&clock).with_tombstone_ttl(Duration::from_secs(30));
    let key = claim_key("evt-8");
    assert!(store.claim(&key, "aaaa/one", TTL).await.expect("claim"));
    assert!(store.settle(&key, "aaaa/one").await.expect("settle"));

    clock.advance(Duration::from_secs(31));
    assert_eq!(
        store.resolve(&key).await.expect("expired"),
        ClaimState::Absent
    );
    assert_eq!(store.len(), 0, "过期项必须被就地丢掉");
}

// =====================================================================
// 容量界与报出的预算
// =====================================================================

/// 键数**永远**不超过容量：满了先淘汰一个已过期项，再淘汰任意一项（Redis 的 `maxmemory` 策略在
/// 本仓的对应物）。
#[test]
fn the_map_never_grows_past_its_capacity() {
    let clock = ManualClock::new();
    let store = WeComClaimStore::new(4).with_clock(clock.clock());
    for index in 0..32 {
        store.write(&claim_key(&format!("evt-{index}")), "a/b".to_string(), TTL);
        assert!(store.len() <= 4, "第 {index} 次写入之后越界");
    }
    assert_eq!(store.len(), 4);
    assert!(!store.is_empty());

    // 更新一个**已存在**的键不会触发淘汰。
    let existing = store.raw_value(&claim_key("evt-31"));
    assert!(existing.is_some());
    store.write(&claim_key("evt-31"), "c/d".to_string(), TTL);
    assert_eq!(store.len(), 4);
    assert_eq!(
        store.raw_value(&claim_key("evt-31")).as_deref(),
        Some("c/d")
    );
}

/// `claim_budget` 报的是**构造时给的那个数**（上游逐字：存储自己说出这个数，因为存储自己执行它）。
#[test]
fn the_store_reports_the_budget_it_was_built_with() {
    let default = WeComClaimStore::new(8);
    assert_eq!(default.claim_budget(), DEFAULT_CLAIM_BUDGET);
    let shrunk = WeComClaimStore::new(8).with_budget(Duration::from_millis(7));
    assert_eq!(shrunk.claim_budget(), Duration::from_millis(7));
    assert!(format!("{shrunk:?}").contains("budget"), "{shrunk:?}");
    // 默认值 = 本仓中继那一份常量（`relay.rs`，这里是**复用**而不是重抄）。
    //
    // ⚠️ 上游 `defaultClaimBudget` 是 **2s**，而本仓 M7-17 定的 `DEFAULT_CLAIM_BUDGET` 是 **50ms**
    // （进程内一次 `HashMap` 操作没有 2s 的可界对象）⇒ 两边**数值不同、口径一致**：存储自己
    // 报出这个数，因为存储自己"执行"它。登记为 `docs/32` §38 的 D3 与 R3。
    assert_eq!(DEFAULT_CLAIM_BUDGET, Duration::from_millis(50));
}
