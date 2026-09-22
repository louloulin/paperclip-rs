//! 状态机属性测试：任意事件序列下，`apply` 只有两种结果。
//!
//! 逐条矩阵用例（`state/tests.rs`）证明「哪些边合法」；这个文件证明**任何**序列都
//! 不会破坏三条结构性不变量：
//!
//! 1. `Ok` ⇒ 返回的状态与 `transition.to` 一致，且 `transition.from` 等于调用前的
//!    状态（不存在「不知道从哪来」的成功迁移）。
//! 2. `Err` ⇒ 行**逐字节未变**（`apply` 先校验后写，失败不留半截写入 —— 否则
//!    M3-6 会把一个坏状态当成 CAS 的源状态写下去）。
//! 3. 终结态吸收：一旦终结，任何后续改变状态的事件都必须失败。
//!
//! 另外两条结构性事实也顺手钉住：`apply` 永远到不了 `deferred`（只由创建给出），
//! 以及 `waiting_local_directory` 必有非空 `wait_reason`。

use mc_core::{Id, Timestamp};
use mc_task::retry::{FailureReason, RetryBudget};
use mc_task::state::CancelledBy;
use mc_task::{TaskEvent, TaskState, TaskStatus};
use proptest::prelude::*;

fn arb_reason() -> impl Strategy<Value = FailureReason> {
    prop::sample::select(FailureReason::ALL.to_vec())
}

fn arb_event() -> impl Strategy<Value = TaskEvent> {
    prop_oneof![
        Just(TaskEvent::Queued),
        Just(TaskEvent::Dispatch),
        Just(TaskEvent::Running),
        Just(TaskEvent::Progress),
        Just(TaskEvent::Message),
        Just(TaskEvent::Completed),
        Just(TaskEvent::Deferred),
        Just(TaskEvent::Reclaim),
        Just(TaskEvent::RequeueAfterClaimFailure),
        ".*".prop_map(|wait_reason| TaskEvent::WaitingLocalDirectory { wait_reason }),
        arb_reason().prop_map(|reason| TaskEvent::Failed {
            reason,
            message: None,
        }),
        prop_oneof![
            Just(CancelledBy::System),
            Just(CancelledBy::User {
                id: Some(Id::new()),
                name: None,
            }),
        ]
        .prop_map(|by| TaskEvent::Cancelled { by }),
    ]
}

fn t(n: i64) -> Timestamp {
    Timestamp::from_unix(n)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn any_event_sequence_keeps_the_row_structurally_sound(
        events in prop::collection::vec(arb_event(), 0..24),
    ) {
        let (mut state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
        for (step, event) in events.iter().enumerate() {
            let before = state.clone();
            let at = t(1_000 + i64::try_from(step).expect("步数小"));
            match state.apply(event.clone(), at) {
                Ok(transition) => {
                    prop_assert_eq!(transition.from, Some(before.status));
                    prop_assert_eq!(state.status, transition.to);
                    prop_assert_ne!(state.status, TaskStatus::Deferred);
                    if state.status == TaskStatus::WaitingLocalDirectory {
                        prop_assert!(
                            state.wait_reason.as_deref().is_some_and(|r| !r.trim().is_empty())
                        );
                    }
                }
                Err(_) => {
                    // 失败不留半截写入。
                    prop_assert_eq!(&state, &before);
                }
            }
        }
    }

    #[test]
    fn a_terminal_row_absorbs_every_status_changing_event(
        first in arb_event(),
        later in arb_event(),
    ) {
        let (mut state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
        // 先把任意首个事件打进去，再用一次必定可用的终结事件收尾：`Cancelled`
        // 从任何非终结态都合法，而已经是终结态的行会直接拒掉它 —— 两种情况下
        // 循环结束时都必然终结。
        let _ = state.apply(first, t(1));
        let _ = state.apply(TaskEvent::Cancelled { by: CancelledBy::System }, t(2));
        prop_assert!(state.status.is_terminal());
        let before = state.clone();
        let changes_status = later.kind().changes_status();
        let result = state.apply(later, t(4));
        if changes_status {
            prop_assert!(result.is_err());
        }
        prop_assert_eq!(&state, &before);
    }
}
