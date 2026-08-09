//! R489 — 进程 session bridge 全链路集成验证。
//!
//! 把 R487-R488 的决策串成 Node `execution-target.ts`
//! `startAdapterExecutionTargetProcessSessionBridge` 的决策主流程：
//! 1. 启动计划（sandbox gate / 目录树 / command payload / start 脚本）
//! 2. proxy 脚本源码（模板 + 写盘计划）
//! 3. 连接握手（token 鉴权 / stdin 写入 / 事件投递）
//! 4. stop（stdinEnd 补写 + handle 组装）

use pc_acpx::execution_target::*;
use pc_acpx::sandbox_callback_bridge::{
    base64_decode_utf8, create_sandbox_callback_bridge_token, shell_quote,
};
use std::collections::BTreeMap;

fn sandbox_target() -> pc_acpx::execution_target::AdapterExecutionTarget {
    pc_acpx::execution_target::parse_adapter_execution_target(&serde_json::json!({
        "kind": "remote",
        "transport": "sandbox",
        "providerKey": "e2b",
        "environmentId": "env-1",
        "leaseId": "lease-1",
        "remoteCwd": "/sandbox/workspace",
        "timeoutMs": 30_000,
        "streamRunLogs": true,
    }))
    .expect("sandbox target")
}

// =============================================================================
// 1. 启动计划全链路
// =============================================================================

#[test]
fn start_plan_full_flow() {
    let mut launch_env = BTreeMap::new();
    launch_env.insert("FOO".to_string(), "bar".to_string());
    let plan = start_adapter_execution_target_process_session_bridge_plan(
        "session-1",
        Some(&sandbox_target()),
        None,
        "claude",
        "claude",
        &["-p".to_string(), "hello".to_string()],
        "",
        &launch_env,
        Some(60.0),
    )
    .expect("sandbox plan");

    // 目录树（对齐 Node join 链）。
    assert_eq!(
        plan.session_dir,
        "/sandbox/workspace/.paperclip-runtime/claude/process-sessions/session-1"
    );
    assert_eq!(
        plan.remote_script_path,
        "/sandbox/workspace/.paperclip-runtime/claude/process-sessions/paperclip-process-session-remote.mjs"
    );
    assert_eq!(plan.timeout_ms, Some(60_000));
    assert_eq!(plan.proxy_token.len(), 24, "18 bytes → 24 chars base64url");

    // command payload 往返（对齐 Node base64(JSON.stringify(...))）。
    let payload_json = base64_decode_utf8(&plan.command_payload).expect("payload");
    let payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap();
    assert_eq!(payload["command"], "claude");
    assert_eq!(payload["args"], serde_json::json!(["-p", "hello"]));
    assert_eq!(
        payload["cwd"], "/sandbox/workspace",
        "cwd 缺省回退 remoteCwd"
    );
    assert_eq!(payload["env"]["FOO"], "bar");

    // start 脚本（对齐 Node execute）。
    assert!(plan.start_script.contains(
        "mkdir -p '/sandbox/workspace/.paperclip-runtime/claude/process-sessions/session-1/stdin'"
    ));
    assert!(plan.start_script.contains("PAPERCLIP_PROCESS_SESSION_DIR="));
    assert!(plan
        .start_script
        .contains("PAPERCLIP_PROCESS_SESSION_COMMAND_B64="));
    assert!(plan.start_script.contains("nohup node"));
    assert!(plan.start_script.contains("printf '%s\\n' \"$!\""));

    // 远端脚本同步计划（sha 门控 + 专用 lock）。
    let sync =
        sync_process_session_remote_script_plan(&plan.bridge_runtime_dir, &plan.remote_script_path);
    assert_eq!(sync.label, "Process session remote script");
    assert_eq!(sync.action, "sync process session remote script");
    assert!(sync
        .uploaded_decision_script
        .contains(&shell_quote(&sync.lock_dir)));
}

// =============================================================================
// 2. proxy 脚本源码 + 写盘计划
// =============================================================================

#[test]
fn proxy_script_source_and_write_plan() {
    let token = create_sandbox_callback_bridge_token(Some(18));
    let source = get_process_session_proxy_source(4310, &token);

    // 模板关键协议片段。
    assert!(source.contains("#!/usr/bin/env node"));
    assert!(source.contains(&format!("port: 4310")));
    assert!(source.contains(&format!("const token = \"{token}\";")));
    assert!(source.contains("socket.on(\"connect\", () => send({ type: \"hello\" }));"));
    assert!(source.contains("process.stdin.on(\"data\""));
    assert!(source.contains("process.stdin.on(\"end\", () => send({ type: \"stdinEnd\" }));"));
    assert!(source.contains("message.type === \"data\""));
    assert!(source.contains("message.stream === \"stderr\" ? process.stderr : process.stdout"));
    assert!(source.contains("message.type === \"error\""));
    assert!(source.contains("message.type === \"exit\""));
    assert!(source.contains("if (!exiting) process.exit(1);"));

    // 写盘计划：path = join(dir, PROXY_SCRIPT)，执行器以 0o700 写入。
    let dir = "/tmp/paperclip-process-session-proxy-x";
    assert_eq!(
        format!("{dir}/{PROCESS_SESSION_PROXY_SCRIPT}"),
        "/tmp/paperclip-process-session-proxy-x/paperclip-process-session-proxy.mjs"
    );
}

// =============================================================================
// 3. 连接握手全链路
// =============================================================================

#[test]
fn connection_handshake_full_flow() {
    let token = "proxy-tok";
    // hello → 鉴权接管（flush 缓冲）。
    assert_eq!(
        decide_proxy_connection_message(Some(token), token, false, false),
        ProxyConnectionDecision::Authenticate
    );
    // 第二个连接抢占 → Reject（会话独占）。
    assert_eq!(
        decide_proxy_connection_message(Some(token), token, false, true),
        ProxyConnectionDecision::Reject
    );
    // 已鉴权连接继续。
    assert_eq!(
        decide_proxy_connection_message(Some(token), token, true, false),
        ProxyConnectionDecision::Proceed
    );

    // stdin 事件流：hello → stdin → stdinEnd。
    let mut seq = 0u64;
    let stdin_write =
        build_proxy_stdin_write(seq + 1, Some("stdin"), Some("aGVsbG8=")).expect("stdin write");
    seq += 1;
    assert_eq!(stdin_write.file_name, "000000000001.json");
    let parsed: serde_json::Value = serde_json::from_str(stdin_write.body.trim_end()).unwrap();
    assert_eq!(parsed["type"], "stdin");
    assert_eq!(parsed["data"], "aGVsbG8=");

    let end_write = build_proxy_stdin_write(seq + 1, Some("stdinEnd"), None).unwrap();
    seq += 1;
    assert_eq!(end_write.file_name, "000000000002.json");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(end_write.body.trim_end()).unwrap()["type"],
        "stdinEnd"
    );

    // 事件投递：exit 前 data 直写；缓冲期 exit 停止轮询。
    assert_eq!(
        decide_remote_event_delivery(true, Some("data")),
        RemoteEventDeliveryDecision::WriteToSocket {
            action: RemoteEventSocketAction::Write
        }
    );
    assert_eq!(
        decide_remote_event_delivery(false, Some("exit")),
        RemoteEventDeliveryDecision::QueuePending { stop_loop: true }
    );
    assert!(decide_proxy_poll_should_stop(Some("exit")));
}

// =============================================================================
// 4. stop + handle 组装
// =============================================================================

#[test]
fn stop_and_handle_assembly() {
    // stop 补写 stdinEnd（序号 +1）。
    let stop = build_proxy_stop_stdin_end_write(5);
    assert_eq!(stop.file_name, "000000000006.json");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(stop.body.trim_end()).unwrap()["type"],
        "stdinEnd"
    );

    // handle 组装（agentCommand = proxy 脚本路径）。
    let handle = build_process_session_bridge_handle(
        "/tmp/paperclip-process-session-proxy-x/paperclip-process-session-proxy.mjs".to_string(),
    );
    assert_eq!(
        handle.agent_command,
        "/tmp/paperclip-process-session-proxy-x/paperclip-process-session-proxy.mjs"
    );
    assert!(handle.has_stop);

    // 端口校验 + 错误消息。
    assert_eq!(process_session_listen_port_or_error(Some(4310)), Ok(4310));
    let error_value: serde_json::Value =
        serde_json::from_str(proxy_error_message_line("boom").trim_end()).unwrap();
    assert_eq!(error_value["type"], "error");
    assert_eq!(error_value["message"], "boom");
}
