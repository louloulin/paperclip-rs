//! `relay/tests.rs` 的**替身与装备**（端口替身：发布方 / claim 存储 / handler / 汇）。
//!
//! 用例按面分在三个子模块里：`contract`（配置、帧、claim 存储、错误分类）、`idempotency`
//! （**本片专属验收第 1 条**：重投递链的幂等）、`ordering`（**第 2 条**：顺序与准入）。
//! 拆分的依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

mod contract;
mod idempotency;
mod ordering;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mc_core::id::Id;

use crate::wecom::metrics::Metrics;
use crate::wecom::outbound::{Outbound, OutboundError};
use crate::wecom::ws_sender::SenderError;

use super::relayed::provably_not_sent;
use super::*;

// =====================================================================
// 替身
// =====================================================================

/// 一个按脚本回答的 handler（每个决定都由用例写下来，而不是由替身猜）。
#[derive(Default)]
struct FakeHandler {
    owns: bool,
    /// 依次交出去的结论；用完之后一律 `Done`。
    script: Mutex<VecDeque<RelayResult>>,
    delivered: Mutex<Vec<RelayFrame>>,
    records: Mutex<Vec<RelayRecord>>,
}

impl FakeHandler {
    fn owns(owns: bool) -> Arc<Self> {
        Arc::new(Self {
            owns,
            ..Self::default()
        })
    }

    fn scripted(owns: bool, results: Vec<RelayResult>) -> Arc<Self> {
        Arc::new(Self {
            owns,
            script: Mutex::new(results.into()),
            ..Self::default()
        })
    }

    fn delivered(&self) -> Vec<RelayFrame> {
        self.delivered.lock().expect("lock").clone()
    }

    fn records(&self) -> Vec<RelayRecord> {
        self.records.lock().expect("lock").clone()
    }

    fn calls(&self) -> usize {
        self.delivered.lock().expect("lock").len()
    }
}

#[async_trait]
impl RelayHandler for FakeHandler {
    async fn deliver_relayed(&self, frame: &RelayFrame) -> RelayResult {
        self.delivered.lock().expect("lock").push(frame.clone());
        let mut script = self.script.lock().expect("lock");
        script
            .pop_front()
            .unwrap_or_else(|| RelayResult::new(RelayOutcome::Done))
    }

    fn owns_socket(&self, _installation_id: &str) -> bool {
        self.owns
    }

    fn record(&self, record: RelayRecord) {
        self.records.lock().expect("lock").push(record);
    }
}

/// 一个只数帧的发布方。
#[derive(Default)]
struct FakePublisher {
    published: Mutex<Vec<(String, String)>>,
}

impl RelayPublisher for FakePublisher {
    fn publish_with_id(
        &self,
        scope_type: &str,
        scope_id: &str,
        _exclude: &str,
        _frame: &[u8],
        id: &str,
    ) -> Result<(), String> {
        self.published
            .lock()
            .expect("lock")
            .push((scope_type.to_string(), format!("{scope_id}|{id}")));
        Ok(())
    }
}

/// 只数 `relay_shed` 的汇。
#[derive(Default)]
struct CountingShed {
    shed: AtomicUsize,
    dropped: AtomicUsize,
    dropped_labels: Mutex<Vec<String>>,
}

impl Metrics for CountingShed {
    fn record_connect_failure(&self) {}
    fn record_auth_failure(&self) {}
    fn record_callback_queued(&self) {}
    fn record_callback_queue_blocked(&self) {}
    fn record_stream_finished(&self) {}
    fn record_stream_fell_back(&self) {}
    fn record_stream_opened(&self) {}
    fn record_outbound_delivered(&self) {}
    fn record_outbound_dropped(&self, reason: &str) {
        self.dropped_labels
            .lock()
            .expect("lock")
            .push(reason.to_string());
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_skipped(&self, _reason: &str) {}
    fn record_attachment_delivered(&self) {}
    fn record_attachment_dropped(&self, _reason: &str) {}
    fn record_attachment_delivery_shed(&self) {}
    fn record_outbound_unconfirmed(&self, _reason: &str) {}
    fn record_attachment_unconfirmed(&self, _reason: &str) {}
    fn record_relay_shed(&self, _kind: &str) {
        self.shed.fetch_add(1, Ordering::SeqCst);
    }
}

/// 一个什么都不答的查询端口（`Outbound::outcome_test_double` 之外的地方也要一个非空 `Arc`）。
pub(crate) struct NoQueries;

#[async_trait]
impl crate::wecom::outbound::OutboundQueries for NoQueries {
    async fn get_task_delivery(
        &self,
        _task_id: Id,
    ) -> Result<Option<crate::wecom::outbound::TaskDelivery>, String> {
        Ok(None)
    }
    async fn get_agent_task(
        &self,
        _task_id: Id,
    ) -> Result<Option<crate::wecom::outbound::AgentTask>, String> {
        Ok(None)
    }
    async fn task_has_channel_ingested_messages(&self, _task_id: Id) -> Result<bool, String> {
        Ok(false)
    }
    async fn get_installation(
        &self,
        _installation_id: Id,
    ) -> Result<Option<crate::wecom::outbound::InstallationRecord>, String> {
        Ok(None)
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

/// 一条回答帧（安装 id 用一个固定字面量，好让分片是确定的）。
fn reply_frame(installation: &str, task: &str, content: &str) -> RelayFrame {
    let mut frame = RelayFrame::reply(
        installation.to_string(),
        "chat-1".to_string(),
        1,
        content,
        task,
        "msg-1",
        "ws-1",
        "sess-1",
    );
    frame.session_id = "sess-1".to_string();
    frame
}

fn fast_config() -> RelayConfig {
    RelayConfig {
        // 用例里的链短而快：落定窗口 40ms ⇒ 链约 2 节，退避 5ms。
        lease_settle: Some(Duration::from_millis(40)),
        retry_backoff: Some(Duration::from_millis(5)),
        shards: Some(2),
        queue_depth: Some(4),
        ..RelayConfig::default()
    }
}

// =====================================================================
// 配置与链
// =====================================================================
