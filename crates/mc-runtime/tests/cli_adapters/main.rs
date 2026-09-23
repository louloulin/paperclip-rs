//! M3-8 批 1 / 批 2 / 批 3 的 adapter：走**公开 API** 验证注册表 + 真实 argv + 真实事件流。
//!
//! 分工与 `pi_local_e2e.rs` 一致：
//!
//! - crate 内的一致性套件保证"每个 adapter 都满足 8 项契约"；
//! - 本文件保证"**M3-3 会怎么用就能怎么用**"，并把 `docs/15` §6 / `docs/18` 定下的
//!   **argv 契约**与**协议族映射**钉在公开面上 —— 这两样都在 `capabilities()` /
//!   `recorded_argv()` 上可观测，不需要读私有实现。
//!
//! 批 2 的 ACP 5 项（kimi / kiro / qoder / qoderclicn / traecli）加 grok 共 6 个
//! provider 走**同一套**握手，所以这里用同一个逐帧回放用例一次性证它们：
//! 差异只允许出现在 argv 与差异表（resume 方法 / 认证 / prompt 字段）上。
//!
//! 批 3 收口（本片）：最后 9 项落地后，注册表 = `AgentType::ALL` 全员 25 项，且
//! 每一项的协议族都已归类（`Opaque` 清零）—— 这两条断言就是 M3-8 的收口口径。
//! 批 3 里 6 个 ACP provider 复用同一套逐帧回放（`dim` 多两条配置链应答）；
//! `qwen` 复用 claude 的 stream-json 形态；`openclaw`（`agent --json`）与 `dsh`
//! （`--stdio` 上的版本化 JSONL）分别是自己的线协议。
//!
//! 假 CLI 现场生成，不依赖机器上装了任何一个 provider。

#![cfg(unix)]

use std::sync::Arc;

use mc_runtime::conformance::response_frame_groups;
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

/// 批 2 的 8 个新类型（按上游白名单顺序）。
const BATCH2: [AgentType; 8] = [
    AgentType::Cursor,
    AgentType::Kimi,
    AgentType::Kiro,
    AgentType::Antigravity,
    AgentType::Qoder,
    AgentType::QoderCliCn,
    AgentType::TraeCli,
    AgentType::Grok,
];

/// 批 3 的 9 个新类型（按上游白名单顺序）。
const BATCH3: [AgentType; 9] = [
    AgentType::Openclaw,
    AgentType::Hermes,
    AgentType::Reasonix,
    AgentType::Dsh,
    AgentType::Qwen,
    AgentType::QwenPaw,
    AgentType::Mcode,
    AgentType::Dim,
    AgentType::Zeroclaw,
];

/// 批 3 里共享 ACP 骨架的 6 个 provider：`(类型, 会话 id)`。
///
/// 六家都是 `auth: None`（回放里没有 `id=2`），`dim` 之外都没有静态配置链；
/// 六家的 argv 都是 `acp` 开头（reasonix 后面还钉了一串沙箱开关）。
const ACP_FAMILY_B3: [(AgentType, &str); 6] = [
    (AgentType::QwenPaw, "acp-int-qwenpaw"),
    (AgentType::Hermes, "acp-int-hermes"),
    (AgentType::Reasonix, "acp-int-reasonix"),
    (AgentType::Dim, "acp-int-dim"),
    (AgentType::Mcode, "acp-int-mcode"),
    (AgentType::Zeroclaw, "acp-int-zeroclaw"),
];

/// 批 2 里共享 ACP 骨架的 6 个 provider：`(类型, 会话 id, 要不要先认证)`。
///
/// 只有 grok 的 `AcpAuth` 不是 `None`，所以只有它的回放里带 `id=2` 的
/// `authenticate` 应答 —— 回放帧是按**客户端实际发的那几帧**闸门触发的，多一帧就死等。
const ACP_FAMILY: [(AgentType, &str, bool); 6] = [
    (AgentType::Kimi, "acp-int-kimi", false),
    (AgentType::Kiro, "acp-int-kiro", false),
    (AgentType::Qoder, "acp-int-qoder", false),
    (AgentType::QoderCliCn, "acp-int-qoderclicn", false),
    (AgentType::TraeCli, "acp-int-traecli", false),
    (AgentType::Grok, "acp-int-grok", true),
];

/// 这个类型是不是共享 ACP 骨架的那批（kimi 在批 1 落地，同骨架）。
fn is_acp_family(kind: AgentType) -> bool {
    matches!(
        kind,
        AgentType::Kimi
            | AgentType::Kiro
            | AgentType::Qoder
            | AgentType::QoderCliCn
            | AgentType::TraeCli
            | AgentType::Grok
            | AgentType::QwenPaw
            | AgentType::Hermes
            | AgentType::Reasonix
            | AgentType::Dim
            | AgentType::Mcode
            | AgentType::Zeroclaw
    )
}

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
        // 批 2（本片）：五个 ACP + 两个 stream-json + grok。
        AgentType::Kimi => Arc::new(mc_runtime::Kimi::with_executable(path)),
        AgentType::Kiro => Arc::new(mc_runtime::Kiro::with_executable(path)),
        AgentType::Qoder => Arc::new(mc_runtime::Qoder::with_executable(path)),
        AgentType::QoderCliCn => Arc::new(mc_runtime::QoderCliCn::with_executable(path)),
        AgentType::TraeCli => Arc::new(mc_runtime::TraeCli::with_executable(path)),
        AgentType::Grok => Arc::new(mc_runtime::Grok::with_executable(path)),
        AgentType::Cursor => Arc::new(mc_runtime::Cursor::with_executable(path)),
        AgentType::Antigravity => Arc::new(mc_runtime::Antigravity::with_executable(path)),
        // 批 3（本片）：6 个 ACP + qwen（stream-json）+ openclaw / dsh（自定义 JSONL）。
        AgentType::QwenPaw => Arc::new(mc_runtime::Qwenpaw::with_executable(path)),
        AgentType::Hermes => Arc::new(mc_runtime::Hermes::with_executable(path)),
        AgentType::Reasonix => Arc::new(mc_runtime::Reasonix::with_executable(path)),
        AgentType::Dim => Arc::new(mc_runtime::Dim::with_executable(path)),
        AgentType::Mcode => Arc::new(mc_runtime::Mcode::with_executable(path)),
        AgentType::Zeroclaw => Arc::new(mc_runtime::Zeroclaw::with_executable(path)),
        AgentType::Qwen => Arc::new(mc_runtime::Qwen::with_executable(path)),
        AgentType::Openclaw => Arc::new(mc_runtime::Openclaw::with_executable(path)),
        AgentType::Dsh => Arc::new(mc_runtime::Dsh::with_executable(path)),
        AgentType::Pi => panic!("pi 的用例在 pi_local_e2e.rs"),
    }
}

/// 注册表里的 **25 项**（按上游白名单顺序 = `AgentType::ALL`）—— 顺序本身是断言的一部分。
///
/// 这是 M3-8 的收口断言：批 3 落地后 `AgentType::ALL` 全员都有 adapter，且没有任何
/// 一项还留在 `Opaque`。
#[test]
fn registry_exposes_every_whitelisted_kind_in_whitelist_order() {
    let registry = AdapterRegistry::with_builtin_adapters();
    let expected: Vec<String> = AgentType::ALL
        .iter()
        .map(|kind| kind.as_str().to_owned())
        .collect();
    assert_eq!(registry.names(), expected, "注册表顺序 = 上游白名单顺序");
    assert_eq!(registry.len(), 25);
    assert_eq!(registry.len(), AgentType::ALL.len());

    // 白名单 25 项：每项都能取到 adapter，且协议族已归类（`Opaque` 清零）。
    for kind in AgentType::ALL {
        assert!(registry.get(kind).is_some(), "{kind} 应已注册");
        assert_ne!(
            kind.protocol_family(),
            ProtocolFamily::Opaque,
            "{kind} 的协议族不该还是 Opaque"
        );
    }
    // 批 1 / 批 2 / 批 3 的 24 个新类型一个不少（`pi` 见 `pi_local_e2e.rs`）。
    for kind in BATCH1.into_iter().chain(BATCH2).chain(BATCH3) {
        assert!(registry.get(kind).is_some(), "{kind} 应已注册");
    }
}

/// adapter 自报的协议族必须与 `catalog` 的映射一致，且**都不是 `Opaque`**。
///
/// 收口后这里遍历 `AgentType::ALL` 全员：25 项都得有 adapter、协议族都要归类、
/// header 都要与 catalog 逐字一致 —— 少一项就会在这里红。
#[test]
fn every_processed_adapter_declares_a_classified_protocol_family() {
    let registry = AdapterRegistry::with_builtin_adapters();
    for kind in AgentType::ALL {
        let adapter = registry
            .get(kind)
            .unwrap_or_else(|| panic!("{kind} 未注册"));
        let caps = adapter.capabilities();
        assert_eq!(caps.protocol, kind.protocol_family(), "{kind}");
        assert_ne!(caps.protocol, ProtocolFamily::Opaque, "{kind} 未归类");
        // 已落地的项全是流式 + 能探版本（套件会按这两个开关断言真实行为）。
        assert!(caps.streaming, "{kind}");
        assert!(caps.version_probe, "{kind}");
        assert_eq!(caps.launch_header, kind.launch_header(), "{kind}");
    }
}

/// ACP 家族（批 1 的 kimi + 批 2 的 5 项 + 批 3 的 6 项）共享同一套能力声明。
///
/// 这是“共享骨架”在公开面上的证据：协议族、流式、thinking、工具事件、用量、
/// 会话恢复六项全开是 `AcpDecoder` 的契约，而不是各家自己填的。
#[test]
fn acp_family_shares_one_capability_shape() {
    let registry = AdapterRegistry::with_builtin_adapters();
    for kind in [
        AgentType::Kimi,
        AgentType::Kiro,
        AgentType::Qoder,
        AgentType::QoderCliCn,
        AgentType::TraeCli,
        AgentType::Grok,
        AgentType::QwenPaw,
        AgentType::Hermes,
        AgentType::Reasonix,
        AgentType::Dim,
        AgentType::Mcode,
        AgentType::Zeroclaw,
    ] {
        let caps = registry.get(kind).expect("ACP 项已注册").capabilities();
        assert_eq!(caps.protocol, ProtocolFamily::Acp, "{kind}");
        assert!(caps.streaming, "{kind}");
        assert!(caps.thinking, "{kind}");
        assert!(caps.tool_events, "{kind}");
        assert!(caps.usage_reporting, "{kind}");
        assert!(caps.resume, "{kind}");
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

/// 批 2：非零退出 → `Failed` + `AgentError` + stderr 尾部带诊断。
///
/// ACP 的假 CLI 必须**逐帧**回放（客户端等每一帧的应答），所以末组
/// （`session/prompt` 应答）不给 —— 让进程死在“等 prompt 应答”的那一刻，
/// 与 crate 内套件的 `failing_fake` 用的是同一条路径。
#[tokio::test]
async fn batch2_nonzero_exit_is_reported_as_agent_error_with_stderr_tail() {
    for kind in BATCH2 {
        let cli = if is_acp_family(kind) {
            let mut frames =
                response_frame_groups(&mc_runtime::adapters::acp_core::conformance_success_stdout(
                    "ses-int-fail",
                    "ok",
                    kind == AgentType::Grok,
                ));
            frames.pop();
            FakeCli::live_scripted_failing(&frames, 3, "boom")
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

/// 批 2 的 6 个 ACP provider 走同一套握手：`initialize` →（grok 才有
/// `authenticate`）→ `session/new` → `session/prompt`，逐帧等应答，
/// prompt 走 stdin 的 JSON-RPC 帧，argv 里不带 prompt。
///
/// 差异只在 argv 和差异表上，**不该**在事件流上：所以这里对 6 项断言同一组终态。
#[tokio::test]
async fn acp_family_speaks_the_shared_handshake_frame_by_frame() {
    for (kind, session_id, with_auth) in ACP_FAMILY {
        let transcript =
            mc_runtime::adapters::acp_core::conformance_success_stdout(session_id, "ok", with_auth);
        let frames = response_frame_groups(&transcript);
        let cli = FakeCli::live_scripted(&frames, "exit 0\n");
        let adapter = adapter_for(kind, &cli);
        let handle = adapter
            .launch(LaunchRequest::new("改代码"))
            .await
            .expect("launch");
        let (events, outcome) = handle.drain().await.expect("终态");

        assert_eq!(outcome.status, RunStatus::Completed, "{kind}");
        assert_eq!(outcome.failure_reason, None, "{kind}");
        assert_eq!(outcome.output, "ok", "{kind}");
        assert_eq!(outcome.session_id.as_deref(), Some(session_id), "{kind}");
        assert_eq!(
            outcome
                .usage
                .iter()
                .map(|usage| usage.usage.total_tokens)
                .sum::<u64>(),
            15,
            "{kind}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::Text { .. })),
            "{kind} 应有正文事件"
        );

        // prompt 必须在 stdin 的 JSON-RPC 帧里，且 argv 里没有它。
        let stdin = cli.recorded_stdin();
        assert!(stdin.contains("\"jsonrpc\":\"2.0\""), "{kind}：{stdin}");
        assert!(stdin.contains("\"method\":\"initialize\""), "{kind}");
        assert!(stdin.contains("\"method\":\"session/prompt\""), "{kind}");
        assert!(stdin.contains("改代码"), "{kind}：prompt 必须走 stdin");
        assert!(stdin.ends_with('\n'), "{kind}：每帧一行");
        assert_eq!(
            stdin.contains("\"method\":\"authenticate\""),
            with_auth,
            "{kind}：只有 grok 先认证"
        );
        let argv = cli.recorded_argv();
        assert!(
            !argv.contains("改代码"),
            "{kind} argv 不该带 prompt：{argv:?}"
        );
    }
}

/// cursor：`-p --output-format stream-json --yolo`，prompt 走 stdin（写完即关）。
#[tokio::test]
async fn cursor_speaks_stream_json_with_the_prompt_on_stdin() {
    let cli = FakeCli::replaying(concat!(
        r#"{"type":"system","subtype":"init","session_id":"cursor-int-1"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","session_id":"cursor-int-1","result":"ok","is_error":false,"inputTokens":10,"outputTokens":5,"cacheReadTokens":0,"cacheWriteTokens":0}"#,
        "\n",
    ));
    let adapter = mc_runtime::Cursor::with_executable(cli.path());
    let cwd = cli.dir().join("repo");
    std::fs::create_dir_all(&cwd).expect("建 cwd");
    let handle = adapter
        .launch(
            LaunchRequest::new("改代码")
                .with_model("gpt-5")
                .with_cwd(cwd.clone()),
        )
        .await
        .expect("launch");
    let (_, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.output, "ok");
    assert_eq!(outcome.session_id.as_deref(), Some("cursor-int-1"));
    assert_eq!(
        outcome
            .usage
            .iter()
            .map(|usage| usage.usage.total_tokens)
            .sum::<u64>(),
        15
    );

    let argv = cli.recorded_argv();
    assert!(argv.contains("-p"), "argv={argv:?}");
    assert!(argv.contains("--output-format"), "argv={argv:?}");
    assert!(argv.contains("stream-json"), "argv={argv:?}");
    assert!(argv.contains("--yolo"), "argv={argv:?}");
    assert!(argv.contains("--workspace"), "argv={argv:?}");
    assert!(
        argv.contains(cwd.to_str().unwrap()),
        "`--workspace` 要带上请求的 cwd：{argv:?}"
    );
    assert!(cli.recorded_stdin().contains("改代码"), "prompt 走 stdin");
    assert!(!argv.contains("改代码"), "argv 不该带 prompt：{argv:?}");
}

/// antigravity：`agy -p <prompt>`（prompt 是 argv 取值），stream-json 事件流 +
/// 纯文本回退，`--print-timeout` 永远在。
#[tokio::test]
async fn antigravity_passes_the_prompt_on_argv_and_reads_both_stream_shapes() {
    let cli = FakeCli::replaying(concat!(
        r#"{"event":"init","conversation_id":"agy-int-1","init":{"model":"gemini-3.6-flash-high"}}"#,
        "\n",
        r#"{"event":"step_update","conversation_id":"agy-int-1","step_update":{"step_index":0,"state":"active","step_type":"agent_response","text_delta":"o"}}"#,
        "\n",
        // 纯文本行（agy 1.0.14 的空白 stdout 之前的形态）：按原样回显。
        "不是 JSON 的裸文本\n",
        r#"{"event":"step_update","step_update":{"step_index":0,"state":"done","step_type":"agent_response","text_delta":"k","usage":{"input_tokens":4,"output_tokens":6,"thinking_tokens":3,"total_tokens":10}}}"#,
        "\n",
        r#"{"event":"result","result":{"status":"SUCCESS","response":"ok","usage":{"input_tokens":100,"output_tokens":100,"total_tokens":200}}}"#,
        "\n",
    ));
    let adapter = mc_runtime::Antigravity::with_executable(cli.path());
    let cwd = cli.dir().join("repo");
    std::fs::create_dir_all(&cwd).expect("建 cwd");
    let handle = adapter
        .launch(
            LaunchRequest::new("改代码")
                .with_model("gemini-3.6-flash-high")
                .with_cwd(cwd.clone()),
        )
        .await
        .expect("launch");
    let (events, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.session_id.as_deref(), Some("agy-int-1"));
    // 正文 = 拼接的 Text 事件（含回退的裸文本行），**不**取 `result.response`：
    // 这与 crate 的 `output == 拼接 Text` 不变量一致（见模块文档的偏离 1）。
    assert_eq!(outcome.output, "o\n不是 JSON 的裸文本k");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Usage { .. })),
        "step_update 里的用量要报出来"
    );
    // 单步用量（10）胜出：`result` 那份是**整轮**统计，不参与按模型取最大值的口径。
    assert_eq!(
        outcome
            .usage
            .iter()
            .map(|usage| usage.usage.total_tokens)
            .sum::<u64>(),
        10
    );

    let argv = cli.recorded_argv();
    assert_eq!(
        argv.lines().next(),
        Some("-p"),
        "`-p` 是第一个参数（后面紧跟 prompt）：{argv:?}"
    );
    assert!(argv.contains("改代码"), "prompt 是 argv 取值：{argv:?}");
    assert!(argv.contains("--dangerously-skip-permissions"), "{argv:?}");
    assert!(argv.contains("--output-format"), "{argv:?}");
    assert!(argv.contains("stream-json"), "{argv:?}");
    assert!(
        argv.lines().any(|arg| arg == "--print-timeout"),
        "`--print-timeout` 永远在（上游默认 5m 会掐死长 turn）：{argv:?}"
    );
    assert!(argv.contains("--add-dir"), "{argv:?}");
    assert!(cli.recorded_stdin().is_empty(), "argv 传输不该写 stdin");
}

mod batch3;
