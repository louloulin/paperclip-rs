//! 两个**同步**接缝、端口接线表，以及一个可选端口的兜底。
//!
//! 本文件是 `typing/tests.rs` 的子模块（门 ⑩ 的 800 行硬限拆分，见 `docs/32` §38 的 D9）。

use super::*;

/// 上游那两个方法在**没有运行时**时不许 panic（那正是这两个同步接缝的纪律）。
#[test]
fn the_sync_seam_does_not_panic_without_a_runtime() {
    let harness = Harness::builder().build();
    let notifier = DetachedTypingNotifier::new(Arc::new(harness.indicator));
    assert!(format!("{notifier:?}").contains("wired"));
    notifier.on_ingested(
        &installation(harness.installation_id),
        &inbound("req-1", "room"),
        harness.session,
    );
    notifier.on_settled(harness.session);
    // 同步接缝只推任务 ⇒ 屏幕上一个气泡都没有（而且什么都没炸）。
    assert_eq!(notifier.indicator().depth(), 0);
    assert!(notifier.indicator().wired().streams);
}

/// 有运行时 ⇒ 同步接缝真的把工作推出去了，而完整路径可以直接 `await`。
#[tokio::test]
async fn the_sync_seam_pushes_the_work_when_a_runtime_exists() {
    let harness = Harness::builder().build();
    let indicator = Arc::new(harness.indicator);
    let notifier = DetachedTypingNotifier::new(Arc::clone(&indicator));
    let session = harness.session;
    notifier.on_ingested(
        &installation(harness.installation_id),
        &inbound("req-1", "room"),
        session,
    );
    for _ in 0..8 {
        tokio::task::yield_now().await;
        if indicator.depth() == 1 {
            break;
        }
    }
    assert_eq!(indicator.depth(), 1, "脱离任务必须把气泡画出来");
    assert_eq!(harness.senders.opened(), 1);

    notifier.on_settled(session);
    for _ in 0..8 {
        tokio::task::yield_now().await;
        if indicator.depth() == 0 {
            break;
        }
    }
    assert_eq!(indicator.depth(), 0, "`on_settled` 必须把它关掉");
    assert_eq!(harness.senders.endings(), 1);
}

/// `wired()` 把"哪几个端口配了"逐格报出来（它正是"没有配置"必须可观测的那一半）。
#[test]
fn wired_reports_every_port_independently() {
    let bare = TypingIndicator::new();
    assert!(!bare.wired().senders && !bare.wired().streams);
    assert!(!bare.wired().tasks && !bare.wired().deliveries);
    assert!(!bare.wired().languages && !bare.wired().relay && !bare.wired().roots);
    let harness = Harness::builder().build();
    let wired = harness.indicator.wired();
    assert!(wired.senders && wired.streams && wired.tasks && wired.deliveries);
    assert!(!wired.languages && !wired.relay && !wired.roots);
    assert!(format!("{:?}", harness.indicator).contains("streams: true"));
    assert!(harness.router.frames().is_empty());
}

/// 一个**没配**语言面 / 中继面 / 血缘查询的指示器仍然是完整的（每一格都有兜底）。
#[test]
fn the_optional_ports_have_fallbacks() {
    let indicator =
        TypingIndicator::new().with_roots(Arc::new(crate::wecom::typing::TaskLookupRoots::new(
            Arc::new(FakeTasks::default()) as Arc<dyn TaskQueries>,
        )));
    assert!(indicator.wired().roots);
    assert!(!indicator.wired().languages);
    assert!(!indicator.holding());
    // `DeploymentLanguage` 永远给部署语言（1:1 是降级而不是错误）。
    assert_eq!(
        super::DeploymentLanguage.locale_for(Id::new(), CHAT_TYPE_GROUP_INT, "x"),
        Locale::ZhHans
    );
}
