//! 全量 serde 往返测试（LUM-1407 用例 ①）—— `messages.go` 的 **29 个结构体**逐个
//! 「填满 → 序列化 → 反序列化 → 相等」。
//!
//! 这里不重复 golden JSON 的字节级断言（那在 `tests/golden.rs`），只保证**每一个**
//! 载荷类型都能在 Rust 侧完整地进出一次：没有字段被 `skip_serializing_if` 误吞、
//! 没有三态被塌缩、没有 `#[serde(rename)]` 写反。填充值刻意全部非零
//! （空字符串 / 0 / false 会被 `omitempty` 省略，用它做断言等于什么都没测）。

use mc_daemon_proto::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Map;

/// 往返一次并断言等值；同时断言「序列化 → 反序列化 → 再序列化」字节稳定（幂等），
/// 这样任何一次出站/入站不对称都会被抓住。
fn assert_roundtrip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    let wire = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&wire).expect("deserialize");
    assert_eq!(&back, value, "往返不相等：{wire}");
    let again = serde_json::to_string(&back).expect("re-serialize");
    assert_eq!(again, wire, "往返后再序列化不稳定");
}

/// 一个非空 JSON 对象，给 `input` / `body` 这两个 `map`/`RawMessage` 字段用。
fn non_empty_map() -> Map<String, serde_json::Value> {
    let mut map = Map::new();
    map.insert("cmd".to_owned(), serde_json::json!("ls -l"));
    map.insert("nested".to_owned(), serde_json::json!({"a": [1, 2, 3]}));
    map
}

#[test]
fn roundtrip_envelope() {
    assert_roundtrip(&Message {
        kind: TASK_MESSAGE.to_owned(),
        payload: serde_json::json!({"task_id": "t1"}),
    });
    // payload 为 Null 也必须能往返（Go 的 nil RawMessage 对应形态）。
    assert_roundtrip(&Message {
        kind: String::new(),
        payload: serde_json::Value::Null,
    });
    assert_roundtrip(&RPCRequestPayload {
        request_id: "r1".to_owned(),
        method: rpc::method::TASKS_CLAIM.to_owned(),
        body: Some(serde_json::json!({"daemon_id": "d1", "runtime_ids": ["rt-1"], "max_tasks": 3})),
        timeout_ms: 15_000,
    });
    assert_roundtrip(&RPCRequestPayload {
        request_id: "r2".to_owned(),
        method: "tasks.claim".to_owned(),
        body: None,
        timeout_ms: 0,
    });
    assert_roundtrip(&RPCResponsePayload {
        request_id: "r1".to_owned(),
        status: rpc::RPC_STATUS_OK,
        body: Some(serde_json::json!({"tasks": []})),
        error: String::new(),
    });
    assert_roundtrip(&RPCResponsePayload {
        request_id: "r1".to_owned(),
        status: rpc::RPC_STATUS_UNKNOWN_METHOD,
        body: None,
        error: "unknown rpc method \"x\"".to_owned(),
    });
}

#[test]
fn roundtrip_task_payloads() {
    assert_roundtrip(&TaskDispatchPayload {
        task_id: "t1".to_owned(),
        issue_id: "i1".to_owned(),
        title: "标题".to_owned(),
        description: "描述".to_owned(),
    });
    assert_roundtrip(&TaskAvailablePayload {
        runtime_id: "rt-1".to_owned(),
        task_id: "t1".to_owned(),
    });
    assert_roundtrip(&TaskAvailablePayload {
        runtime_id: "rt-1".to_owned(),
        task_id: String::new(),
    });
    assert_roundtrip(&TaskProgressPayload {
        task_id: "t1".to_owned(),
        summary: "正在跑测试".to_owned(),
        step: 3,
        total: 7,
    });
    assert_roundtrip(&TaskCompletedPayload {
        task_id: "t1".to_owned(),
        pr_url: "https://github.com/louloulin/paperclip-rs/pull/1".to_owned(),
        output: "done".to_owned(),
    });
    assert_roundtrip(&TaskMessagePayload {
        call_id: "c1".to_owned(),
        task_id: "t1".to_owned(),
        issue_id: "i1".to_owned(),
        seq: 7,
        kind: "tool_use".to_owned(),
        tool: "bash".to_owned(),
        content: "text".to_owned(),
        input: non_empty_map(),
        output: "out".to_owned(),
        output_truncated: Some(true),
        created_at: "2026-09-22T10:00:00Z".to_owned(),
    });
    // 三态的另外两个取值也必须各自往返。
    for tri in [Some(false), None] {
        assert_roundtrip(&TaskMessagePayload {
            output_truncated: tri,
            ..TaskMessagePayload::default()
        });
    }
}

#[test]
fn roundtrip_daemon_payloads() {
    assert_roundtrip(&DaemonRegisterPayload {
        daemon_id: "d1".to_owned(),
        agent_id: "a1".to_owned(),
        runtimes: vec![RuntimeInfo {
            kind: "claude".to_owned(),
            version: "1.2.3".to_owned(),
            status: "ready".to_owned(),
        }],
    });
    assert_roundtrip(&RuntimeInfo {
        kind: "codex".to_owned(),
        version: "0.9.0".to_owned(),
        status: "ready".to_owned(),
    });
    assert_roundtrip(&DaemonHeartbeatRequestPayload {
        runtime_id: "rt-1".to_owned(),
        supports_batch_import: true,
    });
    assert_roundtrip(&DaemonHeartbeatRequestPayload {
        runtime_id: "rt-1".to_owned(),
        supports_batch_import: false,
    });
    assert_roundtrip(&DaemonHeartbeatPendingUpdate {
        id: "u1".to_owned(),
        target_version: "1.2.3".to_owned(),
    });
    assert_roundtrip(&DaemonHeartbeatPendingModelList {
        id: "m1".to_owned(),
    });
    assert_roundtrip(&DaemonHeartbeatPendingLocalSkills {
        id: "s1".to_owned(),
    });
    assert_roundtrip(&DaemonHeartbeatPendingLocalSkillImport {
        id: "i1".to_owned(),
        skill_key: "k1".to_owned(),
    });
    assert_roundtrip(&DaemonHeartbeatAckPayload {
        runtime_id: "rt-1".to_owned(),
        status: HEARTBEAT_STATUS_RUNTIME_GONE.to_owned(),
        server_capabilities: vec![DAEMON_CAPABILITY_RPC_V1.to_owned()],
        runtime_gone: true,
        pending_update: Some(DaemonHeartbeatPendingUpdate {
            id: "u1".to_owned(),
            target_version: "1.2.3".to_owned(),
        }),
        pending_model_list: Some(DaemonHeartbeatPendingModelList {
            id: "m1".to_owned(),
        }),
        pending_local_skills: Some(DaemonHeartbeatPendingLocalSkills {
            id: "s1".to_owned(),
        }),
        pending_local_skill_import: Some(DaemonHeartbeatPendingLocalSkillImport {
            id: "i1".to_owned(),
            skill_key: "k1".to_owned(),
        }),
        pending_local_skill_imports: vec![DaemonHeartbeatPendingLocalSkillImport {
            id: "i2".to_owned(),
            skill_key: "k2".to_owned(),
        }],
    });
    assert_roundtrip(&PendingWorkPayload {
        runtime_id: "rt-1".to_owned(),
        kind: daemon::pending_work_kind::LOCAL_SKILL_IMPORT.to_owned(),
    });
    assert_roundtrip(&RuntimeProfilesChangedPayload {
        workspace_id: "w1".to_owned(),
        runtime_profile_id: "rp1".to_owned(),
    });
    assert_roundtrip(&WorkspacesChangedPayload {});
}

#[test]
fn roundtrip_chat_payloads() {
    assert_roundtrip(&ChatQuickAction {
        label: "继续".to_owned(),
        prompt: "继续下去".to_owned(),
        primary: true,
    });
    assert_roundtrip(&ChatQuickActionsPayload {
        chat_session_id: "cs1".to_owned(),
        task_id: "t1".to_owned(),
        message_id: "m1".to_owned(),
        quick_actions: vec![ChatQuickAction {
            label: "l".to_owned(),
            prompt: "p".to_owned(),
            primary: false,
        }],
        failed: true,
    });
    // 空 `quick_actions` 是**有意义**的终态，必须原样往返（无 omitempty）。
    let empty_actions = ChatQuickActionsPayload {
        chat_session_id: "cs1".to_owned(),
        task_id: "t1".to_owned(),
        message_id: "m1".to_owned(),
        quick_actions: Vec::new(),
        failed: false,
    };
    assert_roundtrip(&empty_actions);
    assert_eq!(
        serde_json::to_string(&empty_actions).unwrap(),
        r#"{"chat_session_id":"cs1","task_id":"t1","message_id":"m1","quick_actions":[]}"#
    );

    assert_roundtrip(&ChatMessagePayload {
        chat_session_id: "cs1".to_owned(),
        message_id: "m1".to_owned(),
        role: "assistant".to_owned(),
        content: "你好".to_owned(),
        task_id: "t1".to_owned(),
        created_at: "2026-09-22T10:00:00Z".to_owned(),
    });
    assert_roundtrip(&ChatDonePayload {
        chat_session_id: "cs1".to_owned(),
        task_id: "t1".to_owned(),
        message_id: "m1".to_owned(),
        content: "完成".to_owned(),
        elapsed_ms: 1234,
        created_at: "2026-09-22T10:00:00Z".to_owned(),
        message_kind: chat::message_kind::NO_RESPONSE.to_owned(),
        quick_actions: vec![ChatQuickAction {
            label: "l".to_owned(),
            prompt: "p".to_owned(),
            primary: true,
        }],
        quick_actions_pending: true,
    });
    assert_roundtrip(&ChatCancelFinalizedPayload {
        outcome: chat::cancel_outcome::RESTORED.to_owned(),
        chat_session_id: "cs1".to_owned(),
        task_id: "t1".to_owned(),
        initiator_user_id: "u1".to_owned(),
        message_id: "m1".to_owned(),
        content: "Stopped.".to_owned(),
        message_kind: chat::message_kind::MESSAGE.to_owned(),
        created_at: "2026-09-22T10:00:00Z".to_owned(),
        elapsed_ms: 99,
    });
    assert_roundtrip(&ChatSessionReadPayload {
        chat_session_id: "cs1".to_owned(),
    });
    assert_roundtrip(&ChatSessionCreatedPayload {
        workspace_id: "w1".to_owned(),
        chat_session_id: "cs1".to_owned(),
        agent_id: "a1".to_owned(),
        creator_id: "u1".to_owned(),
        title: "会话".to_owned(),
        channel_source: ChatSessionChannelSource {
            channel_type: "slack".to_owned(),
            installation_id: "inst1".to_owned(),
            route_revision: 42,
        },
        is_current_channel_route: true,
    });
    assert_roundtrip(&ChatSessionChannelSource {
        channel_type: "web".to_owned(),
        installation_id: String::new(),
        route_revision: 0,
    });
    assert_roundtrip(&ChatSessionDeletedPayload {
        chat_session_id: "cs1".to_owned(),
    });
    // 三态字段的三种取值各自往返（`None` / `Some(None)` / `Some(Some)`）。
    for project_id in [None, Some(None), Some(Some("p1".to_owned()))] {
        assert_roundtrip(&ChatSessionUpdatedPayload {
            chat_session_id: "cs1".to_owned(),
            title: "t".to_owned(),
            project_id,
            pinned: Some(false),
            status: Some("archived".to_owned()),
            updated_at: "2026-09-22T10:00:00Z".to_owned(),
        });
    }
}

/// 全部 29 个类型的 `Default` 也必须能往返（Go 零值等价）：这条保证 `#[serde(default)]`
/// 在**空对象**上不会 panic，也保证「缺失字段」路径与「显式零值」路径一致。
#[test]
fn roundtrip_all_defaults() {
    assert_roundtrip(&Message::default());
    assert_roundtrip(&RPCRequestPayload::default());
    assert_roundtrip(&RPCResponsePayload::default());
    assert_roundtrip(&TaskDispatchPayload::default());
    assert_roundtrip(&TaskAvailablePayload::default());
    assert_roundtrip(&TaskProgressPayload::default());
    assert_roundtrip(&TaskCompletedPayload::default());
    assert_roundtrip(&TaskMessagePayload::default());
    assert_roundtrip(&DaemonRegisterPayload::default());
    assert_roundtrip(&RuntimeInfo::default());
    assert_roundtrip(&DaemonHeartbeatRequestPayload::default());
    assert_roundtrip(&DaemonHeartbeatAckPayload::default());
    assert_roundtrip(&DaemonHeartbeatPendingUpdate::default());
    assert_roundtrip(&DaemonHeartbeatPendingModelList::default());
    assert_roundtrip(&DaemonHeartbeatPendingLocalSkills::default());
    assert_roundtrip(&DaemonHeartbeatPendingLocalSkillImport::default());
    assert_roundtrip(&PendingWorkPayload::default());
    assert_roundtrip(&RuntimeProfilesChangedPayload::default());
    assert_roundtrip(&WorkspacesChangedPayload::default());
    assert_roundtrip(&ChatQuickAction::default());
    assert_roundtrip(&ChatQuickActionsPayload::default());
    assert_roundtrip(&ChatMessagePayload::default());
    assert_roundtrip(&ChatDonePayload::default());
    assert_roundtrip(&ChatCancelFinalizedPayload::default());
    assert_roundtrip(&ChatSessionReadPayload::default());
    assert_roundtrip(&ChatSessionCreatedPayload::default());
    assert_roundtrip(&ChatSessionChannelSource::default());
    assert_roundtrip(&ChatSessionDeletedPayload::default());
    assert_roundtrip(&ChatSessionUpdatedPayload::default());
    // 全部字段都带 omitempty 的类型，其默认值必须序列化成 `{}`（Go 的零值省略）。
    assert_eq!(
        serde_json::to_string(&ChatDonePayload::default()).unwrap(),
        r#"{"chat_session_id":"","task_id":""}"#
    );
    assert_eq!(
        serde_json::to_string(&WorkspacesChangedPayload::default()).unwrap(),
        "{}"
    );
    // 无 omitempty 的字段照常输出，不能凭空消失。
    assert_eq!(
        serde_json::to_string(&ChatQuickActionsPayload::default()).unwrap(),
        r#"{"chat_session_id":"","task_id":"","message_id":"","quick_actions":[]}"#
    );
    // 无 omitempty 的字段照常输出（`chat_session_id` / `title` / `updated_at`）。
    assert_eq!(
        serde_json::to_string(&ChatSessionUpdatedPayload::default()).unwrap(),
        r#"{"chat_session_id":"","title":"","updated_at":""}"#
    );
    // `Message` 两个字段都无 omitempty。
    assert_eq!(
        serde_json::to_string(&Message::default()).unwrap(),
        r#"{"type":"","payload":null}"#
    );
}
