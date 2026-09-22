//! Golden JSON 契约测试（LUM-1407 用例 ②）—— 线上字节由**上游 Go 结构体标签**推导。
//!
//! # 这些字面量是怎么来的（必须如实记录）
//!
//! 本机**没有 `go` 工具链**（`which go` 为空），无法用 `go run` 打印
//! `json.Marshal` 的真实输出。因此这 4 组 golden 是**手工按 Go 结构体标签推导**的，
//! 推导规则有 3 条，都可在 `server/pkg/protocol/messages.go` 上逐字复核：
//!
//! 1. **键名** = `json:"..."` 标签原文（如 `json:"output_truncated,omitempty"` → `output_truncated`）；
//! 2. **键序** = Go 结构体字段声明顺序（`encoding/json` 顺序输出结构体字段）；
//! 3. **键出现/省略** = `omitempty` 的零值规则（`false`/`0`/`""`/空切片/空 map/nil 指针都省略）。
//!
//! 上游冻结 commit：`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`（`docs/16` §2 有清单）。
//! 复算办法见 `docs/16` §12（在同一台有 Go 的机器上跑 `go test` 打印，再与本文件的
//! 断言逐条对照）；在此之前，本文件的价值是**锁住解码侧语义**，而不是自称上游字节。

use mc_daemon_proto::*;
use serde_json::json;

/// 帧信封 golden（`messages.go:112` 的 `Message` + L87 的 `RPCRequestPayload`）：
/// 键序 = 字段声明顺序，所以逐字比较即可锁住 `rename = "type"` 与 `omitempty`。
const FRAME: &str = r#"{"type":"daemon:rpc_request","payload":{"request_id":"r1","method":"tasks.claim","body":{"daemon_id":"d1","runtime_ids":[],"max_tasks":0}}}"#;

/// 把 JSON 文本解析成某载荷，并把解码结果**再**序列化一遍。
fn decode_and_reencode<T>(raw: &str) -> (T, serde_json::Value)
where
    T: for<'de> serde::Deserialize<'de> + serde::Serialize,
{
    let value: T = serde_json::from_str(raw).expect("golden JSON must decode");
    let back = serde_json::to_value(&value).expect("decoded payload must re-serialize");
    (value, back)
}

/// 用例 ②-1：`DaemonHeartbeatAckPayload`（`messages.go:401`）的两条真实路径
/// —— 正常 ack 与 `runtime_gone` 回收 ack。
#[test]
fn golden_heartbeat_ack() {
    // 正常路径：status 为普通状态串，没有 runtime_gone / 没有 server_capabilities
    // （老 server），也没有任何 pending_*。
    let raw = r#"{"runtime_id":"rt-1","status":"ok"}"#;
    let (ack, back) = decode_and_reencode::<DaemonHeartbeatAckPayload>(raw);
    assert_eq!(ack.runtime_id, "rt-1");
    assert_eq!(ack.status, "ok");
    assert!(!ack.runtime_gone);
    assert!(ack.server_capabilities.is_empty());
    assert!(ack.pending_update.is_none());
    assert!(ack.pending_model_list.is_none());
    assert!(ack.pending_local_skills.is_none());
    assert!(ack.pending_local_skill_import.is_none());
    assert!(ack.pending_local_skill_imports.is_empty());
    assert_eq!(back, json!({"runtime_id":"rt-1","status":"ok"}));

    // runtime_gone 路径（`messages.go:419` 的常量 + 布尔）：server 用它替代 HTTP 404，
    // 撕连接会让死 UUID 一直心跳到 daemon 进程重启。
    let raw = r#"{"runtime_id":"rt-1","status":"runtime_gone","runtime_gone":true}"#;
    let (ack, back) = decode_and_reencode::<DaemonHeartbeatAckPayload>(raw);
    assert!(ack.runtime_gone);
    assert_eq!(ack.status, HEARTBEAT_STATUS_RUNTIME_GONE);
    assert_eq!(
        back,
        json!({"runtime_id":"rt-1","status":"runtime_gone","runtime_gone":true})
    );

    // 协议协商 + 四个 pending 指针 + 复数导入（键序 = 声明顺序）。
    let raw = r#"{"runtime_id":"rt-1","status":"ok","server_capabilities":["rpc-v1"],
        "pending_update":{"id":"u1","target_version":"1.2.3"},
        "pending_model_list":{"id":"m1"},
        "pending_local_skills":{"id":"s1"},
        "pending_local_skill_import":{"id":"i1","skill_key":"k1"},
        "pending_local_skill_imports":[{"id":"i2","skill_key":"k2"},{"id":"i1","skill_key":"k1"}]}"#;
    let (ack, back) = decode_and_reencode::<DaemonHeartbeatAckPayload>(raw);
    assert_eq!(ack.server_capabilities, vec!["rpc-v1".to_owned()]);
    assert_eq!(
        ack.pending_update,
        Some(DaemonHeartbeatPendingUpdate {
            id: "u1".to_owned(),
            target_version: "1.2.3".to_owned(),
        })
    );
    assert_eq!(
        ack.pending_model_list,
        Some(DaemonHeartbeatPendingModelList {
            id: "m1".to_owned()
        })
    );
    assert_eq!(
        ack.pending_local_skills,
        Some(DaemonHeartbeatPendingLocalSkills {
            id: "s1".to_owned()
        })
    );
    assert_eq!(
        ack.pending_local_skill_import,
        Some(DaemonHeartbeatPendingLocalSkillImport {
            id: "i1".to_owned(),
            skill_key: "k1".to_owned(),
        })
    );
    assert_eq!(ack.pending_local_skill_imports.len(), 2);
    // 出站键序与声明顺序一致（Go 的结构体字段顺序）—— 逐字比较，因为
    // `serde_json::Value`（BTreeMap）会把键排序，只有 `to_string` 才反映声明顺序。
    assert_eq!(
        serde_json::to_string(&ack).unwrap(),
        concat!(
            r#"{"runtime_id":"rt-1","status":"ok","server_capabilities":["rpc-v1"],"#,
            r#""pending_update":{"id":"u1","target_version":"1.2.3"},"#,
            r#""pending_model_list":{"id":"m1"},"#,
            r#""pending_local_skills":{"id":"s1"},"#,
            r#""pending_local_skill_import":{"id":"i1","skill_key":"k1"},"#,
            r#""pending_local_skill_imports":[{"id":"i2","skill_key":"k2"},{"id":"i1","skill_key":"k1"}]}"#
        )
    );
    // `runtime_gone=false` 必须被 `omitempty` 省略（Go 的 bool 零值规则）。
    assert!(back.get("runtime_gone").is_none());
}

/// 用例 ②-2：`TaskMessagePayload`（`messages.go:201`）的**三态** `output_truncated`。
#[test]
fn golden_task_message_tristate() {
    // (a) 完整工具结果：`output_truncated: false` 是**显式的**「量过，没截断」。
    let raw = r#"{"call_id":"c1","task_id":"t1","issue_id":"i1","seq":3,"type":"tool_result",
        "tool":"bash","output":"ok","output_truncated":false,"created_at":"2026-09-22T10:00:00Z"}"#;
    let (msg, back) = decode_and_reencode::<TaskMessagePayload>(raw);
    assert_eq!(msg.kind, "tool_result");
    assert_eq!(msg.seq, 3);
    assert_eq!(msg.tool, "bash");
    assert_eq!(msg.output_truncated, Some(false));
    // 关键断言：`Some(false)` 不能被 `omitempty` 吞掉（这是 `*bool` 与 `bool` 的分界）。
    assert_eq!(back.get("output_truncated"), Some(&json!(false)));
    assert!(
        back.get("input").is_none(),
        "nil map 应被 omitempty 省略（Go 零值规则）"
    );

    // (b) 历史行 / 老 daemon：键缺失 = 「没有任何 daemon 量过」，客户端必须按未知渲染。
    let raw = r#"{"task_id":"t1","seq":0,"type":"text","content":"hi"}"#;
    let (msg, back) = decode_and_reencode::<TaskMessagePayload>(raw);
    assert_eq!(msg.output_truncated, None);
    assert!(msg.kind == "text" && msg.content == "hi");
    assert!(
        back.get("output_truncated").is_none(),
        "None 必须重新省略，绝不能折成 false"
    );

    // (c) 被截断 + 工具入参（`input` 是 map[string]any，非空时保留）。
    let raw = r#"{"task_id":"t1","seq":7,"type":"tool_use","tool":"bash",
        "input":{"cmd":"ls","flags":["-l"]},"output_truncated":true}"#;
    let (msg, back) = decode_and_reencode::<TaskMessagePayload>(raw);
    assert_eq!(msg.output_truncated, Some(true));
    assert_eq!(msg.input.get("cmd"), Some(&json!("ls")));
    assert_eq!(msg.input.get("flags"), Some(&json!(["-l"])));
    assert_eq!(back.get("output_truncated"), Some(&json!(true)));
}

/// 用例 ②-3：`ChatSessionUpdatedPayload`（`messages.go:365`）的 `**string`
/// —— 缺失 / `null` / 有值 三态，外加 `*bool` 的 `false` 不能丢。
#[test]
fn golden_chat_session_updated_null_vs_omitted() {
    // (a) 改名路径：没提 project_id / pinned / status → 三键都不得出现在出站。
    let raw = r#"{"chat_session_id":"cs1","title":"renamed","updated_at":"2026-09-22T10:00:00Z"}"#;
    let (upd, back) = decode_and_reencode::<ChatSessionUpdatedPayload>(raw);
    assert_eq!(upd.project_id, None);
    assert_eq!(upd.pinned, None);
    assert_eq!(upd.status, None);
    for key in ["project_id", "pinned", "status"] {
        assert!(back.get(key).is_none(), "缺失的键 {key} 不能被补出来");
    }

    // (b) 移出项目路径：**显式 null** —— 必须与 (a) 区分开。
    let raw = r#"{"chat_session_id":"cs1","title":"renamed","project_id":null,"updated_at":"2026-09-22T10:00:00Z"}"#;
    let (upd, back) = decode_and_reencode::<ChatSessionUpdatedPayload>(raw);
    assert_eq!(upd.project_id, Some(None));
    assert_eq!(back.get("project_id"), Some(&json!(null)));

    // (c) 置顶路径：`pinned: false` 是非 nil 指针 → 必须序列化出来（`*bool` 不是 `bool`）。
    let raw = r#"{"chat_session_id":"cs1","title":"t","pinned":false,"updated_at":"2026-09-22T10:00:00Z"}"#;
    let (upd, back) = decode_and_reencode::<ChatSessionUpdatedPayload>(raw);
    assert_eq!(upd.pinned, Some(false));
    assert_eq!(back.get("pinned"), Some(&json!(false)));

    // (d) 改项目 + 归档路径：三态齐全。
    let raw = r#"{"chat_session_id":"cs1","title":"t","project_id":"p1","pinned":true,
        "status":"archived","updated_at":"2026-09-22T10:00:00Z"}"#;
    let (upd, back) = decode_and_reencode::<ChatSessionUpdatedPayload>(raw);
    assert_eq!(upd.project_id, Some(Some("p1".to_owned())));
    assert_eq!(upd.pinned, Some(true));
    assert_eq!(upd.status.as_deref(), Some("archived"));
    assert_eq!(back.get("project_id"), Some(&json!("p1")));
    assert_eq!(
        serde_json::to_string(&upd).unwrap(),
        concat!(
            r#"{"chat_session_id":"cs1","title":"t","project_id":"p1","pinned":true,"#,
            r#""status":"archived","updated_at":"2026-09-22T10:00:00Z"}"#
        )
    );
}

/// 用例 ②-4：RPC 请求/响应信封（`messages.go:87` / L104）+ 帧信封
/// （`messages.go:112`）。RPC 响应体与 HTTP 响应体逐字相同，所以这里用 claim 的形状。
#[test]
fn golden_rpc_envelope() {
    // 请求：`timeout_ms` 缺省（0）→ omitempty 省略；body 是 method 专属请求体。
    let raw = r#"{"request_id":"r1","method":"tasks.claim",
        "body":{"daemon_id":"d1","runtime_ids":["rt-1","rt-2"],"max_tasks":5}}"#;
    let (req, back) = decode_and_reencode::<RPCRequestPayload>(raw);
    assert_eq!(req.request_id, "r1");
    assert_eq!(req.method, rpc::method::TASKS_CLAIM);
    assert_eq!(req.timeout_ms, 0);
    assert_eq!(
        req.body.as_ref().and_then(|b| b.get("max_tasks")),
        Some(&json!(5))
    );
    assert!(back.get("timeout_ms").is_none());

    // 带服务端预算的请求：`timeout_ms > 0` 时保留。
    let raw = r#"{"request_id":"r2","method":"tasks.claim","timeout_ms":15000}"#;
    let (req, _) = decode_and_reencode::<RPCRequestPayload>(raw);
    assert_eq!(req.timeout_ms, 15_000);
    assert!(req.body.is_none(), "缺失 body → None（不是空对象）");

    // 成功响应：2xx 带 body（形状 = POST /api/daemon/tasks/claim 的 200 响应）。
    let raw = r#"{"request_id":"r1","status":200,
        "body":{"tasks":[],"claim_poll_hint_supported":true,"next_deferred_task_after_ms":1000}}"#;
    let (resp, back) = decode_and_reencode::<RPCResponsePayload>(raw);
    assert_eq!(resp.status, rpc::RPC_STATUS_OK);
    assert!(resp.error.is_empty());
    assert_eq!(
        resp.body
            .as_ref()
            .and_then(|b| b.get("claim_poll_hint_supported")),
        Some(&json!(true))
    );
    assert!(back.get("error").is_none(), "空 error 要被 omitempty 省略");

    // 失败响应：非 2xx 带 error，无 body。404 = 未知 method（`daemon_rpc.go:54`）。
    let raw = r#"{"request_id":"r9","status":404,"error":"unknown rpc method \"x\""}"#;
    let (resp, back) = decode_and_reencode::<RPCResponsePayload>(raw);
    assert_eq!(resp.status, rpc::RPC_STATUS_UNKNOWN_METHOD);
    assert!(resp.error.contains("unknown rpc method"));
    assert!(back.get("body").is_none());
    assert_eq!(back.get("status"), Some(&json!(404)));

    // 帧信封：`type` 是线上键名，Rust 字段叫 `kind`；payload 缺失落 Null（Go nil）。
    // 期望串的键序 = 字段声明顺序，所以逐字比较即可锁住 `rename` 与 `omitempty`。
    let frame: Message = serde_json::from_str(FRAME).expect("frame must decode");
    assert_eq!(frame.kind, DAEMON_RPC_REQUEST);
    assert!(is_known_event(&frame.kind));
    let req: RPCRequestPayload = frame.decode_payload().expect("payload must decode");
    assert_eq!(req.request_id, "r1");
    assert_eq!(
        req.body.as_ref().and_then(|b| b.get("max_tasks")),
        Some(&json!(0))
    );
    // **偏差**（docs/16 §11）：上游 `Message.Payload` 是 `json.RawMessage`，回编码原样
    // 吐出 daemon 的字节；本 crate 用 `serde_json::Value`（BTreeMap），payload 内的键会被
    // 规范化成字典序。所以这里断言的是「同一份 JSON 文档」，不是「同一串字节」。
    let reencoded = frame.encode().expect("frame must encode");
    assert_ne!(
        reencoded, FRAME,
        "若这里相等，说明 payload 的键序被保留了 —— 那 `docs/16` §11 的偏差登记就该删掉"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reencoded).unwrap(),
        serde_json::from_str::<serde_json::Value>(FRAME).unwrap()
    );

    // 空帧（只给 type）：payload 落 Null，不报错 —— Go 的 nil RawMessage 同义。
    let frame: Message = serde_json::from_str(r#"{"type":"task:progress"}"#).unwrap();
    assert_eq!(frame.payload, serde_json::Value::Null);
}
