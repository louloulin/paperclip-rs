//! 契约测试（LUM-1407 用例 ③④⑤）—— 未知字段/未知事件容忍、能力协商、
//! RPC method 名表与上游逐条对照、事件表完整性。
//!
//! 本文件的每个断言都对应 `docs/16-M3-DAEMON-PROTOCOL.md` 里的一行契约；断言的
//! 上游出处写在注释里（冻结 commit `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`）。

use mc_daemon_proto::*;

// ---------------------------------------------------------------------------
// 用例 ③：未知字段容忍 + 未知事件忽略
// ---------------------------------------------------------------------------

/// 未知**字段**必须被忽略（前向兼容的基石）：上游解码从不因为多出字段报错，
/// 所以新 server 可以给老 daemon 加字段。
#[test]
fn unknown_fields_are_ignored() {
    // 每个 payload 都塞进三个不存在的键：`future_field` / `_internal` / `Type`（后者
    // 大小写不同，不能命中 `rename = "type"`）。
    let raw = r#"{"task_id":"t1","seq":1,"type":"text","content":"hi","future_field":{"a":1},
        "_internal":null,"Type":"tool_use","unknown_array":[1,2,3]}"#;
    let msg: TaskMessagePayload = serde_json::from_str(raw).expect("未知字段必须被忽略");
    assert_eq!(msg.task_id, "t1");
    assert_eq!(msg.kind, "text", "`Type` 不应命中 `type`");

    let raw = r#"{"runtime_id":"rt-1","status":"ok","brand_new_capability":[1,2],
        "pending_future_thing":{"id":"x"}}"#;
    let ack: DaemonHeartbeatAckPayload = serde_json::from_str(raw).expect("未知字段必须被忽略");
    assert_eq!(ack.runtime_id, "rt-1");
    assert!(ack.pending_update.is_none());

    let raw = r#"{"request_id":"r1","method":"tasks.claim","body":{"a":1},"future":true}"#;
    let req: RPCRequestPayload = serde_json::from_str(raw).expect("未知字段必须被忽略");
    assert_eq!(req.method, rpc::method::TASKS_CLAIM);

    // 反过来：把**已知**字段写错类型仍然必须报错（容忍未知 ≠ 容忍坏数据）。
    assert!(serde_json::from_str::<TaskMessagePayload>(r#"{"seq":"not-a-number"}"#).is_err());
}

/// 未知**事件**必须被忽略，而不是变成解析错误 —— 上游 `hub.go` `handleFrame` 的
/// `default` 分支：`// Unknown app messages are intentionally ignored for forward
/// compatibility`。这正是本 crate 只提供谓词、不提供枚举的原因。
#[test]
fn unknown_events_are_ignored_not_rejected() {
    let frame: Message =
        serde_json::from_str(r#"{"type":"future:shiny_event","payload":{"whatever":1}}"#)
            .expect("未知 type 的帧必须仍能解码成信封");
    assert!(!is_known_event(&frame.kind), "未知事件必须判定为未知");
    assert!(is_known_event(ISSUE_CREATED), "对照：已知事件应判定为已知");

    // 老读者遇到新 payload 形状：信封解码成功，具体载荷解码失败但不影响忽略决策。
    let frame: Message =
        serde_json::from_str(r#"{"type":"future:shiny_event","payload":"not-an-object"}"#).unwrap();
    assert!(!is_known_event(&frame.kind));
    assert!(frame.decode_payload::<TaskMessagePayload>().is_err());

    // 谓词只认全等，不做大小写/前后缀归一化。
    for wrong in [
        "",
        "ISSUE_CREATED",
        "issue:created ",
        " issue:created",
        "issue:creat",
    ] {
        assert!(!is_known_event(wrong), "{wrong:?} 不应被判为已知");
    }
}

// ---------------------------------------------------------------------------
// 用例 ④：版本 / 能力协商
// ---------------------------------------------------------------------------

/// 能力协商的完整例子：daemon 组头 → server 解析 → 按能力分流。
///
/// 上游出处：`daemon/client.go:184`–L203（daemon 侧拼头）、
/// `handler/daemon.go:1577`（`requestClientCapabilities`）、
/// L1612（`requestHasClientCapability`）、L1594（`runtimeHasCapability`，fail-closed）。
#[test]
fn capability_negotiation() {
    // 1) daemon 侧：WS 是公共 10 条 + `claim-poll-hints-v1`，HTTP 只有公共 10 条。
    let common = daemon_common_capabilities();
    assert_eq!(common.len(), 10, "公共能力集条数（daemon/client.go:203）");
    assert!(common.contains(&DAEMON_CAPABILITY_SKILL_BUNDLES_V1));
    assert!(common.contains(&DAEMON_CAPABILITY_CHECKOUT_KEEPS_WORK_V1));
    assert!(
        !common.contains(&DAEMON_CAPABILITY_CLAIM_POLL_HINTS_V1),
        "claim-poll-hints-v1 不得出现在公共集里"
    );

    let ws = daemon_ws_capabilities();
    let http = daemon_http_capabilities();
    assert_eq!(ws.len(), 11);
    assert_eq!(http.len(), 10);
    assert!(ws.contains(&DAEMON_CAPABILITY_CLAIM_POLL_HINTS_V1));
    assert!(!http.contains(&DAEMON_CAPABILITY_CLAIM_POLL_HINTS_V1));

    // 2) 每条能力都是「无空格、非空、互不相同」的字面量 —— 协商是全等比较，
    //    一个多余空格就会让整条能力失效。
    for cap in &ws {
        assert!(!cap.is_empty());
        assert!(!cap.contains(char::is_whitespace), "{cap} 含空白字符");
    }
    for (i, cap) in ws.iter().enumerate() {
        assert!(!ws[..i].contains(cap), "{cap} 重复声明");
    }

    // 3) 头编码/解码：`X-Client-Capabilities` 逗号分隔，解析侧 TrimSpace + 丢空串。
    let header = encode_capabilities_header(&ws);
    let parsed = parse_client_capabilities(&header);
    assert_eq!(
        parsed,
        ws.iter().map(|c| (*c).to_owned()).collect::<Vec<_>>()
    );
    assert_eq!(
        parse_client_capabilities(" rpc-v1 , ,claim-poll-hints-v1,"),
        vec!["rpc-v1".to_owned(), "claim-poll-hints-v1".to_owned()]
    );
    // 缺失/全空 = 老 daemon 什么都没声明（上游返回 nil）。
    assert!(parse_client_capabilities("").is_empty());
    assert!(parse_client_capabilities(",,,").is_empty());
    assert!(!request_has_client_capability("", DAEMON_CAPABILITY_RPC_V1));
    assert!(request_has_client_capability(
        &header,
        DAEMON_CAPABILITY_RPC_V1
    ));
    assert!(request_has_client_capability(
        " rpc-v1 ",
        DAEMON_CAPABILITY_RPC_V1
    ));
    // 不做子串匹配：`rpc-v1-x` 不是 `rpc-v1`。
    assert!(!request_has_client_capability(
        "rpc-v1-x",
        DAEMON_CAPABILITY_RPC_V1
    ));

    // 4) server → daemon 方向：只有 `rpc-v1`（`handler/daemon.go:1379`），并且它
    //    必须由 server **显式**给出，daemon 不得从自己声明的能力反推。
    assert_eq!(SERVER_HEARTBEAT_CAPABILITIES, [DAEMON_CAPABILITY_RPC_V1]);
    let ack: DaemonHeartbeatAckPayload = serde_json::from_str(
        r#"{"runtime_id":"rt-1","status":"ok","server_capabilities":["rpc-v1"]}"#,
    )
    .unwrap();
    assert!(ack
        .server_capabilities
        .iter()
        .any(|c| c == DAEMON_CAPABILITY_RPC_V1));
    assert_eq!(CLIENT_CAPABILITIES_HEADER, "X-Client-Capabilities");

    // 5) 运行时行 metadata 的能力门：**fail-closed**（`handler/daemon.go:1594`）。
    let ok = br#"{"capabilities":["local-worktree-v1","rpc-v1"]}"#;
    assert!(runtime_has_capability(
        Some(ok),
        DAEMON_CAPABILITY_LOCAL_WORKTREE_V1
    ));
    assert!(!runtime_has_capability(
        Some(ok),
        DAEMON_CAPABILITY_PLATFORM_SKILL_V1
    ));
    for failing in [
        None,
        Some(&b""[..]),
        Some(&b"{"[..]),
        Some(&b"null"[..]),
        Some(&b"{}"[..]),
        Some(&br#"{"capabilities":[]}"#[..]),
        Some(&br#"{"capabilities":"rpc-v1"}"#[..]),
        Some(&br#"{"capabilities":[1,2]}"#[..]),
    ] {
        assert!(
            !runtime_has_capability(failing, DAEMON_CAPABILITY_RPC_V1),
            "查不到声明时必须 fail-closed：{failing:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 用例 ⑤：RPC method 名表与上游逐条对照
// ---------------------------------------------------------------------------

/// 上游 `server/internal/handler/daemon_rpc.go:51` 的 `switch method` 分支**逐条转录**
/// （冻结 commit `f41fae6b08fb`）：`(方法名字面量, 上游行号)`。
///
/// 这是本片「method 名表与上游一致」唯一机械化证据 —— 转录抄错一行，下面的
/// 逐条对照就会红。上游 `default` 分支返回 404 + `unknown rpc method %q`
/// （`daemon_rpc.go:54`），对应 [`rpc::RPC_STATUS_UNKNOWN_METHOD`]。
const UPSTREAM_RPC_METHODS: [(&str, usize); 1] = [("tasks.claim", 52)];

#[test]
fn rpc_method_table_matches_upstream() {
    assert_eq!(
        rpc::method::KNOWN.len(),
        UPSTREAM_RPC_METHODS.len(),
        "method 条数与上游 switch 分支数不一致"
    );
    for (i, (name, line)) in UPSTREAM_RPC_METHODS.iter().enumerate() {
        assert_eq!(
            rpc::method::KNOWN[i],
            *name,
            "第 {i} 条 method 与上游 daemon_rpc.go:{line} 不一致"
        );
        assert!(rpc::method::is_known(name));
    }
    assert_eq!(rpc::method::TASKS_CLAIM, "tasks.claim");
    assert!(!rpc::method::is_known("tasks.claim_by_runtime"));
    assert!(!rpc::method::is_known(""));
    assert!(!rpc::method::is_known("tasks.claim "));

    // 通道级状态码与传输常量（`hub.go:17`–L19、L300、L944、L1007、L1015、L1035；
    // `daemon_rpc.go:54`）。daemon 的等待/回退时机直接由这些数字决定。
    assert_eq!(rpc::RPC_STATUS_OK, 200);
    assert_eq!(rpc::RPC_STATUS_UNKNOWN_METHOD, 404);
    assert_eq!(rpc::RPC_STATUS_TOO_MANY_REQUESTS, 429);
    assert_eq!(rpc::RPC_STATUS_INTERNAL, 500);
    assert_eq!(rpc::RPC_STATUS_HANDLER_UNAVAILABLE, 503);
    assert_eq!(rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT, 8);
    assert_eq!(rpc::RPC_READ_LIMIT_BYTES, 64 * 1024);
    assert_eq!(rpc::WRITE_WAIT_MS, 10_000);
    assert_eq!(rpc::PONG_WAIT_MS, 60_000);
    assert_eq!(rpc::PING_PERIOD_MS, 54_000);
    assert_eq!(rpc::PING_PERIOD_MS, rpc::PONG_WAIT_MS / 10 * 9);
}

// ---------------------------------------------------------------------------
// 事件表完整性（支撑 `docs/16` §6 的 109 行对照表）
// ---------------------------------------------------------------------------

/// 把线上字符串按本 crate 的命名规则折成常量名：大写 + 非字母数字 → `_`。
fn fold_event_name(wire: &str) -> String {
    wire.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[test]
fn event_table_is_complete_and_consistent() {
    assert_eq!(KNOWN_EVENTS.len(), KNOWN_EVENT_COUNT);
    assert_eq!(
        KNOWN_EVENT_COUNT, 109,
        "上游 events.go L6–L217 的事件常量条数（冻结值）"
    );

    for (i, event) in KNOWN_EVENTS.iter().enumerate() {
        assert!(!event.is_empty(), "第 {i} 条事件为空");
        assert!(!event.contains(char::is_whitespace), "{event} 含空白字符");
        assert!(is_known_event(event), "{event} 应在 KNOWN_EVENTS 里被认出");
        assert!(
            !KNOWN_EVENTS[..i].contains(event),
            "{event} 在第 {i} 项重复"
        );
    }

    // 核心样本的逐条对照（其余样本见 event_wire_names_match_upstream）。
    // 常量名必须与折叠规则一致（`stringify!` 与折叠结果比对），字符串必须与上游一致。
    macro_rules! check_event {
        ($konst:ident, $wire:literal, $go:literal, $line:literal) => {{
            assert_eq!($konst, $wire, concat!("events.go:", $line));
            assert_eq!(
                stringify!($konst),
                fold_event_name($konst),
                concat!("常量名不符合折叠规则：", stringify!($konst))
            );
            assert!(
                is_known_event($konst),
                concat!($go, " 应被 is_known_event 认出")
            );
        }};
    }
    check_event!(ISSUE_CREATED, "issue:created", "EventIssueCreated", "6");
    check_event!(TASK_DISPATCH, "task:dispatch", "EventTaskDispatch", "35");
    check_event!(TASK_COMPLETED, "task:completed", "EventTaskCompleted", "39");
    check_event!(CHAT_MESSAGE, "chat:message", "EventChatMessage", "75");
    check_event!(
        DAEMON_HEARTBEAT_ACK,
        "daemon:heartbeat_ack",
        "EventDaemonHeartbeatAck",
        "147"
    );
    check_event!(
        DAEMON_PENDING_WORK,
        "daemon:pending_work",
        "EventDaemonPendingWork",
        "160"
    );
    check_event!(
        DAEMON_RPC_REQUEST,
        "daemon:rpc_request",
        "EventDaemonRPCRequest",
        "166"
    );

    // 折叠规则本身的两个边界：驼峰 Go 标识符不影响结果，`_` 与 `:` 都折成 `_`。
    assert_eq!(
        fold_event_name("issue_metadata:changed"),
        "ISSUE_METADATA_CHANGED"
    );
    assert_eq!(fold_event_name("a-b.c d"), "A_B_C_D");
}

/// 事件表的其余抽样对照：常量名 ↔ 线上字符串 ↔ 上游 Go 标识符（`events.go` 行号）。
#[test]
fn event_wire_names_match_upstream() {
    macro_rules! check_event {
        ($konst:ident, $wire:literal, $go:literal, $line:literal) => {{
            assert_eq!($konst, $wire, concat!("events.go:", $line));
            assert_eq!(
                stringify!($konst),
                fold_event_name($konst),
                concat!("常量名不符合折叠规则：", stringify!($konst))
            );
            assert!(
                is_known_event($konst),
                concat!($go, " 应被 is_known_event 认出")
            );
        }};
    }
    check_event!(
        ISSUE_METADATA_CHANGED,
        "issue_metadata:changed",
        "EventIssueMetadataChanged",
        "9"
    );
    check_event!(
        COMMENT_RESOLVED,
        "comment:resolved",
        "EventCommentResolved",
        "16"
    );
    check_event!(TASK_PROGRESS, "task:progress", "EventTaskProgress", "38");
    check_event!(TASK_MESSAGE, "task:message", "EventTaskMessage", "41");
    check_event!(CHAT_DONE, "chat:done", "EventChatDone", "76");
    check_event!(
        CHAT_SESSION_UPDATED,
        "chat:session_updated",
        "EventChatSessionUpdated",
        "91"
    );
    check_event!(
        DAEMON_HEARTBEAT,
        "daemon:heartbeat",
        "EventDaemonHeartbeat",
        "146"
    );
    check_event!(
        DAEMON_TASK_AVAILABLE,
        "daemon:task_available",
        "EventDaemonTaskAvailable",
        "149"
    );
    check_event!(
        DAEMON_WORKSPACES_CHANGED,
        "daemon:workspaces_changed",
        "EventDaemonWorkspacesChanged",
        "151"
    );
    check_event!(
        DAEMON_RUNTIME_PROFILES_CHANGED,
        "daemon:runtime_profiles_changed",
        "EventDaemonRuntimeProfilesChanged",
        "150"
    );
    check_event!(
        DAEMON_RPC_RESPONSE,
        "daemon:rpc_response",
        "EventDaemonRPCResponse",
        "167"
    );
}

/// 载荷种类常量（`messages.go` 的 `const` 块）也必须与上游字面量一致。
#[test]
fn payload_kind_constants_match_upstream() {
    assert_eq!(HEARTBEAT_STATUS_RUNTIME_GONE, "runtime_gone");
    assert_eq!(daemon::pending_work_kind::KNOWN.len(), 3);
    assert_eq!(daemon::pending_work_kind::MODEL_LIST, "model_list");
    assert_eq!(daemon::pending_work_kind::LOCAL_SKILLS, "local_skills");
    assert_eq!(
        daemon::pending_work_kind::LOCAL_SKILL_IMPORT,
        "local_skill_import"
    );
    assert!(daemon::pending_work_kind::is_known("local_skills"));
    // 未知 kind 是**合法**输入（advisory only），只是不带额外行为。
    assert!(!daemon::pending_work_kind::is_known("future_kind"));

    assert_eq!(chat::message_kind::KNOWN.len(), 4);
    assert_eq!(chat::message_kind::MESSAGE, "message");
    assert_eq!(chat::message_kind::NO_RESPONSE, "no_response");
    assert_eq!(chat::message_kind::ONBOARDING_KICKOFF, "onboarding_kickoff");
    assert_eq!(chat::message_kind::ONBOARDING_OPENING, "onboarding_opening");
    assert!(!chat::message_kind::is_known("future_kind"));

    assert_eq!(chat::cancel_outcome::KNOWN.len(), 2);
    assert_eq!(chat::cancel_outcome::STOPPED, "stopped");
    assert_eq!(chat::cancel_outcome::RESTORED, "restored");
    assert!(!chat::cancel_outcome::is_known("future_outcome"));

    assert_eq!(
        APP_CAPABILITY_CHAT_DRAFT_RESTORE_V1,
        "chat-draft-restore-v1"
    );
    assert_eq!(
        DAEMON_CAPABILITY_SOURCE_CONTEXT_QUICK_CREATE_V1,
        "source_context_quick_create_v1"
    );
}
