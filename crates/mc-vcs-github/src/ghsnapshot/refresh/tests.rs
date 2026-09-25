//! `refresh.rs` 的管道用例 —— 队列去重、单地址串行、限流三级、退避序列、TTL sweep、停机链，
//! 外加**离线 GraphQL 替身**上的端到端（替身道具见 `test_support`）。
//!
//! 全部用例**零真实等待**：时钟是假的、延迟被 [`RecordingTimer`] 捕获、抓取是替身；
//! 只有 HTTP 那几条是真的 wire（对着本机的 `TcpListener` 替身跑真 `reqwest`）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::test_support::{
    decided_snapshot, enabled_client, github_double, open_row, options, quiet_tuning, real_now,
    running_snapshot, variables_of, wait_until, workspace_id, ConcurrencyFetcher, FakeClock,
    FetcherFn, FixedStore, GatedFetcher, RecordingTimer, Wire, TEST_KEY_PEM,
};
use super::{
    system_now_unix, Address, HttpSnapshotFetcher, Manager, PrRowRef, ResolvedTarget,
    SnapshotFetcher, Tuning,
};
use crate::ghsnapshot::snapshot::fetch_pr_snapshot;
use crate::ghsnapshot::Client;
use crate::port::{PrRefreshRequest, RefreshReason, SharedPrRefresh};
use crate::rest::GithubError;

fn request(reason: RefreshReason, number: i32) -> PrRefreshRequest {
    PrRefreshRequest {
        workspace_id: workspace_id(),
        repo_owner: "acme".into(),
        repo_name: "api".into(),
        pr_number: number,
        head_sha: Some("sha-current".into()),
        reason,
    }
}

/// 缺 App 私钥的客户端（所有触发路径都该 no-op）。
fn disabled_client() -> Arc<Client> {
    Arc::new(Client::disabled())
}

/// 启用的客户端（真 App id + 真私钥；不需要真 wire 的管道用例用它过 `enabled()` 判据）。
fn enabled_stub_client() -> Arc<Client> {
    Arc::new(Client::new(Some("123".into()), Some(TEST_KEY_PEM.into())))
}

fn enabled_manager(
    store: Arc<FixedStore>,
    clock: &FakeClock,
    timer: Arc<RecordingTimer>,
    fetcher: Arc<dyn SnapshotFetcher>,
) -> Manager {
    Manager::with_options(
        enabled_stub_client(),
        store,
        options(quiet_tuning(), clock, timer, fetcher),
    )
}

/// 带自定义调参的启用 manager（并发上限 / chase 上限 / sweep TTL 用例）。
fn enabled_manager_with(
    tuning: Tuning,
    store: Arc<FixedStore>,
    clock: &FakeClock,
    timer: Arc<RecordingTimer>,
    fetcher: Arc<dyn SnapshotFetcher>,
) -> Manager {
    Manager::with_options(
        enabled_stub_client(),
        store,
        options(tuning, clock, timer, fetcher),
    )
}

// ---------------------------------------------------------------------------
// 禁令与去重
// ---------------------------------------------------------------------------

/// 缺 App 私钥 ⇒ 整条管道**惰性**（上游验收判据 4）：`start` 不起 worker、
/// `enqueue` / `maybe_enqueue_on_view` 什么都不做、不 panic。
#[tokio::test]
async fn disabled_manager_is_inert() {
    let store = FixedStore::new();
    let timer = Arc::new(RecordingTimer::default());
    let manager = Manager::with_options(
        disabled_client(),
        store,
        options(
            quiet_tuning(),
            &FakeClock::at(0),
            timer.clone(),
            Arc::new(FetcherFn(|_| Ok(decided_snapshot()))) as Arc<dyn SnapshotFetcher>,
        ),
    );
    assert!(!manager.enabled());
    assert!(manager.start().is_ok());
    assert!(!manager.enqueue_request(&request(RefreshReason::Webhook, 1)));
    manager.enqueue_address(Address::new(7, "acme", "api", 1));
    assert_eq!(manager.active_addresses(), 0);
    assert_eq!(manager.pending_requests(), 0);
    assert!(timer.delays().is_empty());
    manager.shutdown().await;
}

/// 上游 `TestEnqueueCoalesces`：同一请求重复入队只留**一件**；不同地址不合并。
#[tokio::test]
async fn enqueue_coalesces_identical_requests() {
    let manager = enabled_manager(
        FixedStore::new(),
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(|_| Ok(decided_snapshot()))),
    );
    let one = request(RefreshReason::Webhook, 7);
    assert!(manager.enqueue_request(&one));
    assert!(!manager.enqueue_request(&one), "第二次必须被合并");
    assert!(!manager.enqueue_request(&one));
    assert_eq!(manager.pending_requests(), 1);
    assert_eq!(manager.queued(), 1);

    assert!(manager.enqueue_request(&request(RefreshReason::Webhook, 8)));
    assert!(manager.enqueue_request(&PrRefreshRequest {
        repo_name: "other".into(),
        ..request(RefreshReason::Webhook, 7)
    }));
    assert_eq!(manager.pending_requests(), 3);
    assert_eq!(manager.queued(), 3);
}

/// 地址路径（TTL sweep / trailing 回放）的去重：同一地址只留一件，且 `active` 立刻置位。
#[tokio::test]
async fn enqueue_address_coalesces_and_marks_active() {
    let manager = enabled_manager(
        FixedStore::new(),
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(|_| Ok(decided_snapshot()))),
    );
    let address = Address::new(7, "acme", "api", 3);
    manager.enqueue_address(address.clone());
    manager.enqueue_address(address.clone());
    assert_eq!(manager.queued(), 1);
    assert!(manager.chart(&address).0, "active 立刻置位");
    manager.enqueue_address(Address::new(7, "acme", "api", 4));
    assert_eq!(manager.queued(), 2);
}

/// 上游 `TestMaybeEnqueueOnViewRespectsTTL` 的本仓形状（偏离 D3：TTL 判定在解析阶段做）：
/// 比 view TTL 新 ⇒ **不抓**；陈旧或从未抓过 ⇒ 抓。三件请求一次入队，落在两个 worker 上。
#[tokio::test]
async fn page_view_respects_the_view_ttl() {
    let store = FixedStore::new();
    let clock = FakeClock::at(10_000);
    store.set_target_for(
        1,
        ResolvedTarget {
            installation_id: 7,
            snapshot_fetched_at: Some(clock.now() - 10), // 10 秒前 < 60 秒 TTL ⇒ 不抓
        },
    );
    store.set_target_for(
        2,
        ResolvedTarget {
            installation_id: 7,
            snapshot_fetched_at: Some(clock.now() - 300), // 5 分钟前 ⇒ 抓
        },
    );
    store.set_target_for(
        3,
        ResolvedTarget {
            installation_id: 7,
            snapshot_fetched_at: None, // 从没抓过 ⇒ 抓
        },
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let manager = enabled_manager(
        store.clone(),
        &clock,
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(decided_snapshot())
        })),
    );
    for number in [1, 2, 3] {
        assert!(manager.maybe_enqueue_request_on_view(&request(RefreshReason::PageView, number)));
    }
    assert_eq!(
        manager.pending_requests(),
        3,
        "三件都「受理」了（解析阶段才判 TTL）"
    );
    manager.start().expect("start");
    assert!(
        wait_until(|| calls.load(Ordering::SeqCst) == 2, Duration::from_secs(3)).await,
        "TTL 内的那次不该抓（实抓 {}）",
        calls.load(Ordering::SeqCst)
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "只有 stale / missing 两件被抓"
    );
    assert_eq!(
        store.resolve_calls.load(Ordering::SeqCst),
        3,
        "三件都解析过"
    );
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// 单地址串行 + 并发上限
// ---------------------------------------------------------------------------

/// 上游验收判据 3：同一地址**绝不并发**抓 —— 在飞期间来的那一次留成 trailing 边，
/// 当前抓取结束后**回放一次**（且只有一次）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_address_is_fetched_serially_with_one_trailing_replay() {
    let store = FixedStore::new().with_rows(vec![open_row()]);
    let gate = GatedFetcher::new(decided_snapshot());
    let manager = enabled_manager(
        store,
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        gate.clone() as Arc<dyn SnapshotFetcher>,
    );
    let address = Address::new(7, "acme", "api", 5);
    manager.enqueue_address(address.clone());
    manager.start().expect("start");
    gate.wait_entered(1).await;
    // 在飞：这一件必须变成 trailing 边，而不是第二个并发抓取。
    manager.enqueue_address(address.clone());
    assert!(manager.chart(&address).2, "在飞期间入队必须留 trailing 边");
    gate.release_all();
    assert!(
        wait_until(|| gate.calls() == 2, Duration::from_secs(2)).await,
        "trailing 边必须回放恰好一次（实抓 {}）",
        gate.calls()
    );
    gate.release_all();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(gate.calls(), 2, "回放只发生一次");
    assert_eq!(gate.max_live(), 1, "同一地址绝不并发");
    manager.shutdown().await;
}

/// worker 池的**并发上限**：N 个不同地址同时在排队时，同时在飞的抓取不超过 `concurrency`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_pool_caps_concurrency() {
    let fetcher = ConcurrencyFetcher::new(decided_snapshot());
    let manager = enabled_manager_with(
        Tuning {
            concurrency: 2,
            ..quiet_tuning()
        },
        FixedStore::new(),
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        fetcher.clone() as Arc<dyn SnapshotFetcher>,
    );
    for number in 1..=5 {
        manager.enqueue_address(Address::new(7, "acme", "api", number));
    }
    manager.start().expect("start");
    assert!(
        wait_until(|| fetcher.calls() == 5, Duration::from_secs(3)).await,
        "五个地址都该被抓（实抓 {}）",
        fetcher.calls()
    );
    assert!(
        fetcher.max_live() <= 2,
        "并发上限 = 2，实测 {}",
        fetcher.max_live()
    );
    assert!(fetcher.max_live() >= 2, "两个 worker 该真的并行");
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// 三级退避
// ---------------------------------------------------------------------------

/// 第一级：`RateLimited` ⇒ **只**记 installation 级暂停截止，**不**直接重排重试
/// （上游 `TestProcessRateLimitedSetsPause` + `TestPersistentRateLimitReturnsToTTLSweep`）。
#[tokio::test]
async fn rate_limited_fetch_records_installation_pause_without_direct_retry() {
    let timer = Arc::new(RecordingTimer::default());
    let manager = enabled_manager(
        FixedStore::new(),
        &FakeClock::at(20_000),
        timer.clone(),
        Arc::new(FetcherFn(|_| {
            Err(GithubError::RateLimited {
                retry_after_secs: 90,
            })
        })),
    );
    manager.enqueue_address(Address::new(1, "acme", "api", 1));
    manager.start().expect("start");
    assert!(
        wait_until(
            || manager.chart(&Address::new(1, "acme", "api", 1)).3 == 0
                && manager.rate_limit_pause(1) > 0,
            Duration::from_secs(2)
        )
        .await,
        "暂停截止该被记下来"
    );
    assert_eq!(manager.rate_limit_pause(1), 90);
    assert!(
        timer.delays().is_empty(),
        "限流**不**建直接重试环（交回 TTL sweep / 下一次事件）：{:?}",
        timer.delays()
    );
    manager.shutdown().await;
}

/// 第二级：`extend_rate_limit` **只延不缩**（上游 `TestRateLimitDeadlineNeverShortens`）
/// 且按 installation 隔离（上游 `TestRateLimitIsolatedByInstallation`）。
#[test]
fn extend_rate_limit_never_shortens_and_is_installation_scoped() {
    let clock = FakeClock::at(21_000);
    let manager = Manager::with_options(
        disabled_client(),
        FixedStore::new(),
        options(
            quiet_tuning(),
            &clock,
            Arc::new(RecordingTimer::default()),
            Arc::new(FetcherFn(|_| Ok(decided_snapshot()))),
        ),
    );
    manager.extend_rate_limit(1, 90);
    manager.extend_rate_limit(1, 30);
    assert_eq!(
        manager.rate_limit_pause(1),
        90,
        "短的 Retry-After 不得缩短截止"
    );
    manager.extend_rate_limit(1, 120);
    assert_eq!(
        manager.rate_limit_pause(1),
        120,
        "更长的 Retry-After 该被采纳"
    );
    assert_eq!(
        manager.rate_limit_pause(2),
        0,
        "另一个 installation 不受影响"
    );
    clock.advance(120);
    assert_eq!(manager.rate_limit_pause(1), 0, "过期后自动清账");
    assert_eq!(manager.rate_limit_pause(1), 0, "清账是幂等的");
}

/// 第三级：被限流的 installation **不占 worker 池**（上游
/// `TestRateLimitedInstallationDoesNotOccupyWorkers`）——地址交给定时器，池子留给别的租户。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_installation_does_not_occupy_workers() {
    let fetcher = ConcurrencyFetcher::new(decided_snapshot());
    let timer = Arc::new(RecordingTimer::default());
    let manager = enabled_manager_with(
        Tuning {
            concurrency: 2,
            ..quiet_tuning()
        },
        FixedStore::new(),
        &FakeClock::at(30_000),
        timer.clone(),
        fetcher.clone() as Arc<dyn SnapshotFetcher>,
    );
    manager.extend_rate_limit(1, 3_600);
    for number in 1..=2 {
        manager.enqueue_address(Address::new(1, "acme", "api", number));
    }
    manager.enqueue_address(Address::new(2, "acme", "api", 1));
    manager.start().expect("start");
    assert!(
        wait_until(|| fetcher.calls_for(2) >= 1, Duration::from_secs(2)).await,
        "另一个租户必须被服务"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fetcher.calls_for(1), 0, "被暂停的 installation 不得抓取");
    assert_eq!(
        timer.delays(),
        vec![Duration::from_secs(3_600); 2],
        "两件都交给定时器"
    );
    manager.shutdown().await;
}

/// 退避**序列**可测（`DoD`）：未决快照按 `30s → 1m → 2m → 5m → 5m…` 重排，且到
/// `max_chase_attempts` 就**停**（上游 `TestScheduleChaseBounded` 的有界性）。
#[tokio::test]
async fn chase_backoff_sequence_is_bounded() {
    let store = FixedStore::new().with_rows(vec![open_row()]);
    let timer = Arc::new(RecordingTimer::default());
    let manager = enabled_manager_with(
        Tuning {
            max_chase_attempts: 5,
            ..quiet_tuning()
        },
        store,
        &FakeClock::at(40_000),
        timer.clone(),
        Arc::new(FetcherFn(|address: Address| {
            Ok(running_snapshot(&format!("sha-{}", address.number)))
        })),
    );
    manager.enqueue_address(Address::new(7, "acme", "api", 1));
    manager.start().expect("start");
    for fired in 0..5 {
        assert!(
            wait_until(|| timer.delays().len() == fired + 1, Duration::from_secs(2)).await,
            "第 {} 次 chase 没排上（当前 {:?}）",
            fired + 1,
            timer.delays()
        );
        assert!(timer.fire_next());
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        timer.delays(),
        vec![
            Duration::from_secs(30),
            Duration::from_secs(60),
            Duration::from_secs(120),
            Duration::from_secs(300),
            Duration::from_secs(300),
        ],
        "退避序列必须逐档爬升并停在末项"
    );
    assert_eq!(timer.delays().len(), 5, "到上限后不再排");
    manager.shutdown().await;
}

/// 追捕的两个**停止条件**：快照已决；或 head-SHA 守卫把整条响应丢弃
/// （上游 `applySnapshot` 回 0 行 ⇒ `anyApplied == false`）。
#[tokio::test]
async fn decided_snapshot_and_head_guard_stop_the_chase() {
    let cases: [(&str, bool); 3] = [
        ("已决 ⇒ 停", true),
        ("head 已前进 ⇒ 停", false),
        ("未决且写成功 ⇒ 追", true),
    ];
    for (index, (name, apply_ok)) in cases.into_iter().enumerate() {
        let store = FixedStore::new().with_rows(vec![open_row()]);
        store.set_apply_ok(apply_ok);
        let timer = Arc::new(RecordingTimer::default());
        let snapshot = if index == 0 {
            decided_snapshot()
        } else {
            running_snapshot("sha-new")
        };
        let manager = enabled_manager(
            store,
            &FakeClock::at(50_000),
            timer.clone(),
            Arc::new(FetcherFn(move |_| Ok(snapshot.clone()))),
        );
        manager.enqueue_address(Address::new(7, "acme", "api", 1));
        manager.start().expect("start");
        let want = usize::from(index == 2);
        if want == 1 {
            assert!(
                wait_until(|| !timer.delays().is_empty(), Duration::from_secs(2)).await,
                "{name}: 该追却没追"
            );
        } else {
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        assert_eq!(timer.delays().len(), want, "{name}");
        manager.shutdown().await;
    }
}

// ---------------------------------------------------------------------------
// TTL sweep
// ---------------------------------------------------------------------------

/// TTL sweep：候选入队 + **游标前进**（上游
/// `TestListStaleUndecidedGitHubPRsExcludesDecidedAndRotatesCursor` 的调用侧）。
#[tokio::test]
async fn sweep_once_enqueues_stale_addresses_and_advances_the_cursor() {
    let store = FixedStore::new();
    let first = Address::new(7, "acme", "api", 1);
    let second = Address::new(7, "acme", "api", 2);
    store.set_sweep_rows(vec![first, second.clone()]);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let clock = FakeClock::at(100_000);
    let manager = enabled_manager_with(
        Tuning {
            sweep_ttl_secs: 600,
            ..quiet_tuning()
        },
        store.clone(),
        &clock,
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(decided_snapshot())
        })),
    );
    manager.start().expect("start");
    manager.sweep_once().await;
    assert!(
        wait_until(|| calls.load(Ordering::SeqCst) == 2, Duration::from_secs(2)).await,
        "两个候选地址都该被抓（实抓 {}）",
        calls.load(Ordering::SeqCst)
    );
    let sweep_calls = store.sweep_calls();
    assert_eq!(sweep_calls.len(), 1);
    assert_eq!(
        sweep_calls[0].0,
        100_000 - 600,
        "陈旧阈值 = now - sweep_ttl"
    );
    assert_eq!(sweep_calls[0].1, Address::default(), "首批用零值游标");
    assert_eq!(sweep_calls[0].2, 200, "一轮有界");

    clock.advance(60);
    manager.sweep_once().await;
    let sweep_calls = store.sweep_calls();
    assert_eq!(sweep_calls.len(), 2);
    assert_eq!(sweep_calls[1].1, second, "游标必须前进到上一批的末地址");
    manager.shutdown().await;
}

/// sweep 查询失败只是记一条 warn（上游 `sweepOnce` 同判），不影响 worker。
#[tokio::test]
async fn sweep_failure_is_survivable() {
    let store = FixedStore::new();
    store.set_sweep_fails(true);
    let manager = enabled_manager(
        store,
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(|_| Ok(decided_snapshot()))),
    );
    manager.start().expect("start");
    manager.sweep_once().await;
    assert_eq!(manager.queued(), 0);
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// 停机链
// ---------------------------------------------------------------------------

/// `DoD`：「停机链 —— 写一条 shutdown 用例（N 秒内 worker 退出）」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_stops_workers_within_the_grace_window() {
    let fetcher = ConcurrencyFetcher::new(decided_snapshot());
    let manager = enabled_manager(
        FixedStore::new(),
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        fetcher.clone() as Arc<dyn SnapshotFetcher>,
    );
    manager.start().expect("start");
    let started = tokio::time::Instant::now();
    manager.shutdown().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "worker 必须在宽限期内退出，实测 {elapsed:?}"
    );
    assert!(manager.workers_joined(), "停机后 worker 句柄必须已收口");
    let before = fetcher.calls();
    manager.enqueue_address(Address::new(7, "acme", "api", 1));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fetcher.calls(), before, "停机后不得再抓");
    assert_eq!(manager.active_addresses(), 0, "停机后不再攒 active 记账");
    // 幂等：第二次立刻返回。
    let again = tokio::time::Instant::now();
    manager.shutdown().await;
    assert!(again.elapsed() < Duration::from_millis(100));
}

/// 停机后 `enqueue_*` **立刻**变成 no-op（不再攒 active 记账）。
#[tokio::test]
async fn enqueue_after_shutdown_is_a_no_op() {
    let manager = enabled_manager(
        FixedStore::new(),
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(|_| Ok(decided_snapshot()))),
    );
    manager.start().expect("start");
    manager.shutdown().await;
    manager.enqueue_address(Address::new(7, "acme", "api", 1));
    assert_eq!(manager.queued(), 0);
    assert!(!manager.enqueue_request(&request(RefreshReason::Webhook, 1)));
    assert_eq!(manager.pending_requests(), 0);
}

// ---------------------------------------------------------------------------
// 端口（`PrRefreshPort` 的实现体在 `crate::port`）
// ---------------------------------------------------------------------------

/// 端口形状：`Arc<dyn PrRefreshPort>` 由 `Manager` 提供，三个方法都可用。
#[tokio::test]
async fn port_impl_serves_the_frozen_trait() {
    let manager = enabled_manager(
        FixedStore::new(),
        &FakeClock::at(1_000),
        Arc::new(RecordingTimer::default()),
        Arc::new(FetcherFn(|_| Ok(decided_snapshot()))),
    );
    let port: SharedPrRefresh = Arc::new(manager.clone());
    assert!(port.enabled());
    port.enqueue(request(RefreshReason::Webhook, 1));
    assert!(port.maybe_enqueue_on_view(request(RefreshReason::PageView, 2)));
    assert_eq!(manager.pending_requests(), 2);
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// 离线 GraphQL 替身（真 wire）—— 用例在 `tests/wire.rs`（替身道具见 `test_support`）
// ---------------------------------------------------------------------------

mod wire;

/// 时钟注入的合理性检查：默认时钟就是系统时钟（wire 用例依赖它落在真实区间）。
#[test]
fn system_clock_is_plausible() {
    let now = system_now_unix();
    assert!(now > 1_600_000_000, "系统时钟应当在 2020 年之后：{now}");
}
