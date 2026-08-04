//! WorkerSupervisor 契约测试（指数 backoff + max_restarts cap）。

use std::time::Duration;

use pc_plugin_host::{SupervisorConfig, SupervisorEvent};

#[test]
fn supervisor_config_default_backoff_caps_at_max() {
    let c = SupervisorConfig::default();
    assert_eq!(c.base_delay_ms, 500);
    assert_eq!(c.max_delay_ms, 30_000);
    assert_eq!(c.max_restarts, 5);
    // cap 行为：2^20 * 500 >> 30_000 → 应被 cap 住
    assert_eq!(c.backoff_delay_ms(20), 30_000);
}

#[test]
fn supervisor_config_custom_backoff_grows() {
    let c = SupervisorConfig {
        max_restarts: 3,
        base_delay_ms: 100,
        max_delay_ms: 800,
        poll_interval_ms: 50,
    };
    assert_eq!(c.backoff_delay_ms(1), 100);
    assert_eq!(c.backoff_delay_ms(2), 200);
    assert_eq!(c.backoff_delay_ms(3), 400);
    // 800 = 100 * 8 < cap 800
    assert_eq!(c.backoff_delay_ms(4), 800);
    // 1600 capped to 800
    assert_eq!(c.backoff_delay_ms(5), 800);
}

#[test]
fn supervisor_event_variants_instantiate() {
    let _ = SupervisorEvent::Restarted {
        plugin_id: uuid::Uuid::new_v4(),
        attempt: 1,
        next_delay_ms: 500,
    };
    let _ = SupervisorEvent::Crashed {
        plugin_id: uuid::Uuid::new_v4(),
        reason: "boom".into(),
    };
    let _ = SupervisorEvent::Recovered {
        plugin_id: uuid::Uuid::new_v4(),
    };
    let _ = Duration::from_millis(100);
}
