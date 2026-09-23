//! M3-8 批 1 的 7 个 adapter：走**公开 API** 验证注册表 + 真实 argv + 真实事件流。
//!
//! 分工与 `pi_local_e2e.rs` 一致：
//!
//! - crate 内的一致性套件保证"每个 adapter 都满足 8 项契约"；
//! - 本文件保证"**M3-3 会怎么用就能怎么用**"，并把 `docs/15` §6 / `docs/18` 定下的
//!   **argv 契约**与**协议族映射**钉在公开面上 —— 这两样都在 `capabilities()` /
//!   `recorded_argv()` 上可观测，不需要读私有实现。
//!
//! 假 CLI 现场生成，不依赖机器上装了任何一个 provider。

#![cfg(unix)]

use std::sync::Arc;

use mc_runtime::{
    AdapterRegistry, AgentType, FailureReason, FakeCli, LaunchRequest, ProtocolFamily, RunStatus,
    RuntimeAdapter, RuntimeEvent,
};

/// 批 1 的 7 个新类型（`pi` 见 `pi_local_e2e.rs`）。
const BATCH1: [AgentType; 7] = [
    AgentType::Claude,
    AgentType::Codebuddy,
    AgentType::Codex,
    AgentType::Copilot,
    AgentType::Opencode,
    AgentType::Codearts,
    AgentType::Deveco,
];

/// 按类型造一个指向假 CLI 的 adapter（只走公开构造器）。
fn adapter_for(kind: AgentType, cli: &FakeCli) -> Arc<dyn RuntimeAdapter> {
    let path = cli.path();
    match kind {
        AgentType::Claude => Arc::new(mc_runtime::Claude::with_executable(path)),
        AgentType::Codebuddy => Arc::new(mc_runtime::Codebuddy::with_executable(path)),
        AgentType::Codex => Arc::new(mc_runtime::Codex::with_executable(path)),
        AgentType::Copilot => Arc::new(mc_runtime::Copilot::with_executable(path)),
        AgentType::Opencode => Arc::new(mc_runtime::Opencode::with_executable(path)),
        AgentType::Codearts => Arc::new(mc_runtime::Codearts::with_executable(path)),
        AgentType::Deveco => Arc::new(mc_runtime::Deveco::with_executable(path)),
        other => panic!("{other} 不在批 1"),
    }
}

/// 注册表里的 8 项（按上游白名单顺序）—— 顺序本身是断言的一部分。
#[test]
fn registry_exposes_batch1_plus_pi_in_whitelist_order() {
    let registry = AdapterRegistry::with_builtin_adapters();
    assert_eq!(
        registry.names(),
        vec![
            "claude",
            "codebuddy",
            "codex",
            "copilot",
            "opencode",
            "codearts",
            "deveco",
            "pi"
        ]
    );
    assert_eq!(registry.len(), 8);
    assert!(
        registry.get(AgentType::Qwen).is_none(),
        "批 2 的类型还没注册"
    );
}

/// adapter 自报的协议族必须与 `catalog` 的映射一致，且**都不是 `Opaque`**。
#[test]
fn every_batch1_adapter_declares_a_classified_protocol_family() {
    let registry = AdapterRegistry::with_builtin_adapters();
    for kind in BATCH1 {
        let adapter = registry
            .get(kind)
            .unwrap_or_else(|| panic!("{kind} 未注册"));
        let caps = adapter.capabilities();
        assert_eq!(caps.protocol, kind.protocol_family(), "{kind}");
        assert_ne!(caps.protocol, ProtocolFamily::Opaque, "{kind} 未归类");
        // 批 1 全是流式 + 能探版本（套件会按这两个开关断言真实行为）。
        assert!(caps.streaming, "{kind}");
        assert!(caps.version_probe, "{kind}");
        assert_eq!(caps.launch_header, kind.launch_header(), "{kind}");
    }
}

/// `--version` 探测走真实子进程，且拿到的是三段版本号。
#[tokio::test]
async fn probe_version_runs_the_binary_with_version() {
    let cli = FakeCli::version("claude 1.2.3\n");
    let adapter = mc_runtime::Claude::with_executable(cli.path());
    let probe = adapter.probe_version().await.expect("探测成功");
    assert_eq!(probe.kind, AgentType::Claude);
    assert_eq!(
        probe.version.map(|v| v.to_string()),
        Some("1.2.3".to_owned())
    );
    assert_eq!(probe.raw.trim(), "claude 1.2.3", "raw 保留 CLI 原样输出");
}

/// claude：prompt 走 stdin（JSON 信封），`-p` / `--output-format stream-json` 在 argv 上。
#[tokio::test]
async fn claude_streams_stream_json_and_sends_the_prompt_on_stdin() {
    let cli = FakeCli::replaying(concat!(
        r#"{"type":"system","subtype":"init","session_id":"sess-int-1"}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-sonnet-4","usage":{"input_tokens":1,"output_tokens":2},"content":[{"type":"text","text":"ok"}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","session_id":"sess-int-1","result":"ok","is_error":false}"#,
        "\n",
    ));
    let adapter = mc_runtime::Claude::with_executable(cli.path());
    let handle = adapter
        .launch(LaunchRequest::new("改代码").with_model("claude-sonnet-4"))
        .await
        .expect("launch");
    let (events, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.failure_reason, None);
    assert_eq!(outcome.output, "ok");
    assert_eq!(outcome.session_id.as_deref(), Some("sess-int-1"));
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Text { .. })));

    let argv = cli.recorded_argv();
    assert!(argv.contains("--output-format"), "argv={argv:?}");
    assert!(argv.contains("stream-json"), "argv={argv:?}");
    // prompt 走 stdin（不是 argv），且 argv 里不能出现 prompt 本体。
    assert!(cli.recorded_stdin().contains("改代码"));
    assert!(!argv.contains("改代码"), "argv 不该带 prompt：{argv:?}");
}

/// copilot：prompt 是 **argv 取值**（`-p <prompt>`），stdin 不参与。
#[tokio::test]
async fn copilot_passes_the_prompt_as_an_argv_value() {
    let cli = FakeCli::replaying(concat!(
        r#"{"type":"session.start","data":{"sessionId":"copilot-int-1","selectedModel":"gpt-5"}}"#,
        "\n",
        r#"{"type":"assistant.message_delta","data":{"messageId":"m1","deltaContent":"ok"}}"#,
        "\n",
        r#"{"type":"assistant.usage","data":{"model":"gpt-5","inputTokens":11,"outputTokens":5,"cacheReadTokens":1,"cacheWriteTokens":0}}"#,
        "\n",
        r#"{"type":"result","sessionId":"copilot-int-1","exitCode":0}"#,
        "\n",
    ));
    let adapter = mc_runtime::Copilot::with_executable(cli.path());
    let handle = adapter
        .launch(LaunchRequest::new("改代码"))
        .await
        .expect("launch");
    let (_, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.output, "ok");
    let argv = cli.recorded_argv();
    assert!(argv.contains("改代码"), "prompt 必须在 argv 上：{argv:?}");
    assert!(argv.contains("-p"), "argv={argv:?}");
    assert!(argv.contains("--output-format"), "argv={argv:?}");
    assert!(cli.recorded_stdin().is_empty(), "argv 传输不该写 stdin");
}

/// codex：`app-server` 的 JSON-RPC 长连接 —— 唯一走 `JsonRpc` 传输的 provider。
///
/// 假 CLI 等 `initialize` → 回放 `thread/start` 应答 → 等 `turn/start`（带 prompt）→ 退出。
/// 断言的重点是**逐行 JSON-RPC 帧**（每帧以换行结尾，prompt 在 `turn/start` 的 params 里）。
#[tokio::test]
async fn codex_speaks_newline_delimited_json_rpc_over_stdio() {
    let cli = FakeCli::live_replaying(
        concat!(
            r#"{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th_int_1"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"turn_int_1"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"ok"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"turnId":"turn_int_1","tokenUsage":{"total":{"inputTokens":10,"outputTokens":5,"cachedInputTokens":1,"cacheWriteInputTokens":0},"last":{}}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"turn_int_1","status":"completed"}}}"#,
            "\n",
        ),
        "initialize",
        "turn/start",
    );
    let adapter = mc_runtime::Codex::with_executable(cli.path());
    let handle = adapter
        .launch(LaunchRequest::new("改代码").with_model("gpt-5-codex"))
        .await
        .expect("launch");
    let (events, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.output, "ok");
    assert_eq!(outcome.session_id.as_deref(), Some("th_int_1"));
    assert_eq!(
        outcome
            .usage
            .iter()
            .map(|u| u.usage.total_tokens)
            .sum::<u64>(),
        16
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Usage { .. })));

    // argv = `app-server --listen stdio://`（+ 用户 extra_args，顺序固定）。
    let argv = cli.recorded_argv();
    assert!(argv.contains("app-server"), "argv={argv:?}");
    assert!(argv.contains("stdio://"), "argv={argv:?}");

    let stdin = cli.recorded_stdin();
    let methods: Vec<String> = stdin
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("每行一个 JSON 帧"))
        .filter_map(|frame| frame["method"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        methods,
        vec!["initialize", "initialized", "thread/start", "turn/start"]
    );
    assert!(stdin.contains("改代码"), "prompt 必须在 turn/start 里");
    assert!(stdin.ends_with('\n'), "帧必须换行结尾");
}

/// opencode 系（opencode / codearts / deveco）：prompt 走 stdin，NDJSON 事件流。
#[tokio::test]
async fn opencode_family_streams_ndjson_from_stdin_prompt() {
    let transcript = concat!(
        r#"{"type":"step_start","sessionID":"ses_int_1","part":{}}"#,
        "\n",
        r#"{"type":"text","sessionID":"ses_int_1","part":{"text":"ok"}}"#,
        "\n",
        r#"{"type":"step_finish","sessionID":"ses_int_1","part":{"reason":"stop","tokens":{"input":10,"output":5,"cache":{"read":1,"write":0}}}}"#,
        "\n",
    );
    // (类型, 是否把 cwd 交给 CLI, prompt 走 stdin 还是 argv)
    for (kind, has_dir_flag, prompt_via_stdin) in [
        (AgentType::Opencode, true, true),
        (AgentType::Codearts, false, true),
        // deveco 是家族里唯一把 prompt 当位置参数的（上游 `deveco run ... <prompt>`）。
        (AgentType::Deveco, true, false),
    ] {
        let cli = FakeCli::replaying(transcript);
        let adapter = adapter_for(kind, &cli);
        let cwd = cli.dir().join("repo");
        std::fs::create_dir_all(&cwd).expect("建 cwd");
        let handle = adapter
            .launch(
                LaunchRequest::new("改代码")
                    .with_model("m-1")
                    .with_cwd(cwd.clone()),
            )
            .await
            .expect("launch");
        let (_, outcome) = handle.drain().await.expect("终态");

        assert_eq!(outcome.status, RunStatus::Completed, "{kind}");
        assert_eq!(outcome.output, "ok", "{kind}");
        assert_eq!(outcome.session_id.as_deref(), Some("ses_int_1"), "{kind}");
        let argv = cli.recorded_argv();
        assert!(argv.contains("run"), "{kind} argv={argv:?}");
        assert!(argv.contains("json"), "{kind} argv={argv:?}");
        assert_eq!(argv.contains("--dir"), has_dir_flag, "{kind} argv={argv:?}");
        if has_dir_flag {
            assert!(
                argv.contains(cwd.to_str().unwrap()),
                "{kind} `--dir` 要带上请求的 cwd：{argv:?}"
            );
        }
        if prompt_via_stdin {
            assert!(
                cli.recorded_stdin().contains("改代码"),
                "{kind} prompt 走 stdin"
            );
            assert!(!argv.contains("改代码"), "{kind} argv 不该带 prompt");
        } else {
            assert!(argv.contains("改代码"), "{kind} prompt 走 argv：{argv:?}");
        }
    }
}

/// 非零退出 → `Failed` + `AgentError` + stderr 尾部带诊断（7 项一视同仁）。
#[tokio::test]
async fn nonzero_exit_is_reported_as_agent_error_with_stderr_tail() {
    for kind in BATCH1 {
        // JSON-RPC 长连接的假 CLI 必须用 `live_*`：它要等 adapter 写出第一帧才回放。
        let cli = if kind == AgentType::Codex {
            FakeCli::live_failing(
                r#"{"jsonrpc":"2.0","method":"error","params":{"message":"boom"}}"#,
                3,
                "boom",
                "initialize",
            )
        } else {
            FakeCli::failing("", 3, "boom")
        };
        let adapter = adapter_for(kind, &cli);
        let handle = adapter
            .launch(LaunchRequest::new("p"))
            .await
            .expect("launch");
        let (_, outcome) = handle.drain().await.expect("终态");

        assert_eq!(outcome.status, RunStatus::Failed, "{kind}");
        assert_eq!(
            outcome.failure_reason,
            Some(FailureReason::AgentError),
            "{kind}"
        );
        assert_eq!(outcome.exit_code, Some(3), "{kind}");
        assert!(
            outcome.stderr_tail.contains("boom"),
            "{kind} 的 stderr 尾部丢了诊断：{:?}",
            outcome.stderr_tail
        );
    }
}

/// 取消：已 spawn 未结束 → `Cancelled` / `Manual`，且幂等。
#[tokio::test]
async fn cancel_stops_a_live_run_and_is_idempotent() {
    // 回放的是**完整**事件流（含 `step_finish`）再睡死：fail-closed 解码器
    // 把截断的流判为 `Failed`，所以取消前必须先把终态行喂进去（见 docs/33）。
    let cli = FakeCli::replaying_then_sleeping(
        concat!(
            r#"{"type":"step_start","sessionID":"ses_int_2","part":{}}"#,
            "\n",
            r#"{"type":"text","sessionID":"ses_int_2","part":{"text":"半句话"}}"#,
            "\n",
            r#"{"type":"step_finish","sessionID":"ses_int_2","part":{"reason":"stop","tokens":{"input":1,"output":1}}}"#,
            "\n",
        ),
        30,
    );
    let adapter = mc_runtime::Opencode::with_executable(cli.path());
    let mut handle = adapter
        .launch(LaunchRequest::new("p"))
        .await
        .expect("launch");
    let run_id = handle.run_id().clone();

    // 等正文真的开始流出来再取消，避免“取消一个还没起的 run”这种假绿。
    let mut saw_text = false;
    while let Some(event) = handle.next_event().await {
        if matches!(event, RuntimeEvent::Text { .. }) {
            saw_text = true;
            break;
        }
    }
    assert!(saw_text, "取消前应有正文事件");

    assert_eq!(
        adapter.cancel(&run_id).await.expect("取消"),
        mc_runtime::CancelOutcome::Signalled
    );
    while handle.next_event().await.is_some() {}
    let outcome = handle.outcome().await.expect("终态");
    assert_eq!(outcome.status, RunStatus::Cancelled);
    assert_eq!(outcome.failure_reason, Some(FailureReason::Manual));
    assert!(matches!(
        adapter.cancel(&run_id).await.expect("幂等"),
        mc_runtime::CancelOutcome::Signalled | mc_runtime::CancelOutcome::NotRunning
    ));
}
