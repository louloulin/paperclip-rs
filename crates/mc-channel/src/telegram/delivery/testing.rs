//! 投递账的**测试替身**：一个忠实复刻库侧语义的进程内存储。
//!
//! `pub(crate)` 是因为 `outbound/tests.rs` 也要用它（流式/终态投递同样需要一条真租约）。
//! 它替的是**存储**，不是**平台** —— `docs/60` §4.2 第 1 条约束的是平台替身。

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::Utc;
use uuid::Uuid;

use super::*;

/// 忠实复刻库侧语义的进程内投递账。
#[derive(Default)]
pub(crate) struct FakeStore {
    turns: Mutex<FakeState>,
}

#[derive(Default)]
pub(crate) struct FakeState {
    rows: HashMap<Uuid, ChannelReplyDeliveryRow>,
    /// `task → (turn, depth)`（上游 `GetChannelReplyTurn` 的替身）。
    chains: HashMap<Uuid, (Id, i32)>,
    /// 每一次写的流水（断言"写了什么、按什么顺序"）。
    writes: Vec<String>,
}

impl FakeStore {
    pub(crate) fn chain(self: &Arc<Self>, task: Id, turn: Id, depth: i32) {
        self.turns
            .lock()
            .expect("lock")
            .chains
            .insert(task.0, (turn, depth));
    }

    pub(crate) fn writes(&self) -> Vec<String> {
        self.turns.lock().expect("lock").writes.clone()
    }

    pub(crate) fn row(&self, turn: Id) -> ChannelReplyDeliveryRow {
        self.turns
            .lock()
            .expect("lock")
            .rows
            .get(&turn.0)
            .cloned()
            .expect("row")
    }

    /// 把租约改成已过期（模拟"持有者死在投递中途"）—— 库侧时间是唯一权威。
    pub(crate) fn expire(&self, turn: Id) {
        let mut state = self.turns.lock().expect("lock");
        let row = state.rows.get_mut(&turn.0).expect("row");
        row.owner_expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    }
}

/// `now + seconds`（`f64` 的秒 → 一个 `chrono` 时刻；`as` 转换会触发 pedantic 的截断 lint）。
fn deadline(seconds: f64) -> chrono::DateTime<Utc> {
    Utc::now()
        + chrono::Duration::from_std(std::time::Duration::from_secs_f64(seconds))
            .expect("a sane lease")
}

/// 造一行（字段齐全，好让状态机的读取路径与原字段对得上）。
pub(crate) fn row(turn: Id, task: Id, phase: &str, send_state: &str) -> ChannelReplyDeliveryRow {
    ChannelReplyDeliveryRow {
        turn_id: turn.0,
        task_id: task.0,
        attempt_depth: 0,
        binding_id: Id::new().0,
        installation_id: Id::new().0,
        channel_type: "telegram".to_string(),
        chat_id: "chat-1".to_string(),
        phase: phase.to_string(),
        send_state: send_state.to_string(),
        message_id: String::new(),
        chunks_sent: 0,
        owner_token: None,
        owner_expires_at: None,
        settled_reason: String::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[async_trait]
impl DeliveryStore for FakeStore {
    async fn turn_for(&self, task_id: Id) -> DeliveryResult<Option<ReplyTurn>> {
        Ok(self
            .turns
            .lock()
            .expect("lock")
            .chains
            .get(&task_id.0)
            .map(|(id, depth)| ReplyTurn {
                id: *id,
                depth: *depth,
            }))
    }

    async fn acquire(
        &self,
        acquire: &AcquireDelivery,
    ) -> DeliveryResult<Option<ChannelReplyDeliveryRow>> {
        let mut state = self.turns.lock().expect("lock");
        let turn = acquire.turn.id.0;
        let now = Utc::now();
        match state.rows.get_mut(&turn) {
            None => {
                let mut fresh = row(
                    acquire.turn.id,
                    acquire.target.task_id,
                    &acquire.phase,
                    SEND_NONE,
                );
                fresh.attempt_depth = acquire.turn.depth;
                fresh.owner_token = Some(acquire.token.0);
                fresh.owner_expires_at = Some(deadline(acquire.lease_seconds));
                state.writes.push("acquire:insert".to_string());
                state.rows.insert(turn, fresh.clone());
                Ok(Some(fresh))
            }
            Some(existing) => {
                let free = existing.owner_token.is_none()
                    || existing.owner_expires_at.is_none_or(|at| at <= now);
                let phase_ok = existing.phase != PHASE_SETTLED
                    && !(acquire.phase == PHASE_STREAMING && existing.phase == PHASE_TERMINAL);
                let depth_ok = acquire.turn.depth >= existing.attempt_depth;
                if !(free && phase_ok && depth_ok) {
                    return Ok(None);
                }
                existing.task_id = acquire.target.task_id.0;
                existing.attempt_depth = acquire.turn.depth;
                if acquire.phase == PHASE_TERMINAL {
                    existing.phase = PHASE_TERMINAL.to_string();
                }
                existing.owner_token = Some(acquire.token.0);
                existing.owner_expires_at = Some(deadline(acquire.lease_seconds));
                let updated = existing.clone();
                state.writes.push("acquire:update".to_string());
                Ok(Some(updated))
            }
        }
    }

    async fn read(&self, turn_id: Id) -> DeliveryResult<Option<ChannelReplyDeliveryRow>> {
        Ok(self
            .turns
            .lock()
            .expect("lock")
            .rows
            .get(&turn_id.0)
            .cloned())
    }

    async fn release(&self, turn_id: Id, token: Id) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.owner_token != Some(token.0) {
            return Ok(false);
        }
        existing.owner_token = None;
        existing.owner_expires_at = None;
        state.writes.push("release".to_string());
        Ok(true)
    }

    async fn renew(&self, turn_id: Id, token: Id, lease_seconds: f64) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.owner_token != Some(token.0) || existing.phase == PHASE_SETTLED {
            return Ok(false);
        }
        existing.owner_expires_at = Some(deadline(lease_seconds));
        state.writes.push("renew".to_string());
        Ok(true)
    }

    async fn claim_send(&self, turn_id: Id, token: Id) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.owner_token != Some(token.0)
            || matches!(existing.send_state.as_str(), SEND_IN_FLIGHT | SEND_UNKNOWN)
        {
            return Ok(false);
        }
        existing.send_state = SEND_IN_FLIGHT.to_string();
        state.writes.push("claim_send".to_string());
        Ok(true)
    }

    async fn record_placeholder(
        &self,
        turn_id: Id,
        token: Id,
        message_id: &str,
    ) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.owner_token != Some(token.0) || existing.send_state != SEND_IN_FLIGHT {
            return Ok(false);
        }
        existing.send_state = SEND_KNOWN.to_string();
        existing.message_id = message_id.to_string();
        state.writes.push("record_placeholder".to_string());
        Ok(true)
    }

    async fn record_chunk(
        &self,
        turn_id: Id,
        token: Id,
        message_id: &str,
        chunks_sent: i32,
    ) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.owner_token != Some(token.0) {
            return Ok(false);
        }
        existing.send_state = SEND_KNOWN.to_string();
        if existing.message_id.is_empty() {
            existing.message_id = message_id.to_string();
        }
        existing.chunks_sent = existing.chunks_sent.max(chunks_sent);
        state.writes.push("record_chunk".to_string());
        Ok(true)
    }

    async fn reset_send(&self, turn_id: Id, token: Id) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.owner_token != Some(token.0) || existing.send_state != SEND_IN_FLIGHT {
            return Ok(false);
        }
        existing.send_state = if existing.message_id.is_empty() {
            SEND_NONE.to_string()
        } else {
            SEND_KNOWN.to_string()
        };
        state.writes.push("reset_send".to_string());
        Ok(true)
    }

    async fn mark_send_unknown(&self, turn_id: Id) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.send_state != SEND_IN_FLIGHT {
            return Ok(false);
        }
        existing.send_state = SEND_UNKNOWN.to_string();
        state.writes.push("mark_send_unknown".to_string());
        Ok(true)
    }

    async fn settle(&self, turn_id: Id, _token: Id, reason: &str) -> DeliveryResult<bool> {
        let mut state = self.turns.lock().expect("lock");
        let Some(existing) = state.rows.get_mut(&turn_id.0) else {
            return Ok(false);
        };
        if existing.phase == PHASE_SETTLED {
            return Ok(false);
        }
        existing.phase = PHASE_SETTLED.to_string();
        existing.settled_reason = reason.to_string();
        existing.owner_token = None;
        existing.owner_expires_at = None;
        state.writes.push("settle".to_string());
        Ok(true)
    }

    async fn close_turn(
        &self,
        close: &CloseDeliveryTurn,
    ) -> DeliveryResult<Option<ChannelReplyDeliveryRow>> {
        let mut state = self.turns.lock().expect("lock");
        let turn = close.turn.id.0;
        let now = Utc::now();
        match state.rows.get_mut(&turn) {
            None => {
                let mut fresh = row(
                    close.turn.id,
                    close.target.task_id,
                    PHASE_SETTLED,
                    SEND_NONE,
                );
                fresh.attempt_depth = close.turn.depth;
                fresh.settled_reason = close.reason.clone();
                state.writes.push("close_turn:insert".to_string());
                state.rows.insert(turn, fresh.clone());
                Ok(Some(fresh))
            }
            Some(existing) => {
                let free = existing.owner_token.is_none()
                    || existing.owner_expires_at.is_none_or(|at| at <= now);
                if existing.phase == PHASE_SETTLED
                    || !free
                    || close.turn.depth < existing.attempt_depth
                {
                    return Ok(None);
                }
                existing.phase = PHASE_SETTLED.to_string();
                existing.settled_reason = close.reason.clone();
                existing.owner_token = None;
                existing.owner_expires_at = None;
                let updated = existing.clone();
                state.writes.push("close_turn:update".to_string());
                Ok(Some(updated))
            }
        }
    }
}

/// 造一个装着替身的 ledger。
pub(crate) fn ledger() -> (Arc<FakeStore>, DeliveryLedger) {
    let store = Arc::new(FakeStore::default());
    (store.clone(), DeliveryLedger::new(store))
}

/// 造一个目标。
pub(crate) fn target() -> DeliveryTarget {
    DeliveryTarget {
        task_id: Id::new(),
        binding_id: Id::new(),
        installation_id: Id::new(),
        kind: ChannelKind::Telegram,
        chat_id: "chat-1".to_string(),
    }
}
