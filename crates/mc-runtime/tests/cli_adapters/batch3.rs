//! 批 3（本片）的 9 个 adapter 的端到端用例 —— 与 `main.rs` 同一个测试 target。
//!
//! 这里只放"这一批独有的线协议/argv 契约"，共享的部分（注册表收口、协议族映射、
//! ACP 能力形状）留在 `main.rs`：那三条断言遍历的是 `AgentType::ALL` 全员，
//! 属于 M3-8 的整体口径，不该随批次漂移。
//!
//! 共享的 helper 与常量从 crate 根（`main.rs`）直接取。

use crate::{adapter_for, ACP_FAMILY_B3};
use mc_runtime::conformance::response_frame_groups;
use mc_runtime::{AgentType, FakeCli, LaunchRequest, RunStatus, RuntimeAdapter, RuntimeEvent};

/// 批 3 的 6 个 ACP provider：与批 2 完全同一套握手，差异只在 argv 与差异表。
///
/// `dim` 额外有**静态配置链**（`session/set_config_option` ×2，帧号 50 / 51），
/// 所以它的回放用 `conformance_config_stdout`；其余 5 项用通用回放 —— 逐帧闸门是
/// 按客户端**实际发的帧**推进的，多一帧应答就会死等。
#[tokio::test]
async fn batch3_acp_family_speaks_the_shared_handshake_frame_by_frame() {
    for (kind, session_id) in ACP_FAMILY_B3 {
        let transcript = if kind == AgentType::Dim {
            mc_runtime::adapters::acp_core::conformance_config_stdout(session_id, "ok", 2)
        } else {
            mc_runtime::adapters::acp_core::conformance_success_stdout(session_id, "ok", false)
        };
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
        // qwenpaw / mcode / zeroclaw 不报模型（上游同款），用量标签退化成 "unknown"，
        // 但**总量**仍要报对。
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

        // stdin 是长连接 JSON-RPC：握手帧 + prompt 帧都在里面，argv 不带 prompt。
        let stdin = cli.recorded_stdin();
        assert!(
            stdin.contains("\"method\":\"initialize\""),
            "{kind}：{stdin}"
        );
        assert!(stdin.contains("\"method\":\"session/prompt\""), "{kind}");
        assert!(stdin.contains("改代码"), "{kind}：prompt 必须走 stdin");
        assert!(stdin.ends_with('\n'), "{kind}：每帧一行");
        assert!(
            !stdin.contains("\"method\":\"authenticate\""),
            "{kind}：六家都不需要先认证"
        );

        // argv 契约：`acp` 是第一个参数；reasonix 另外钉了一串沙箱开关。
        let argv = cli.recorded_argv();
        assert_eq!(argv.lines().next(), Some("acp"), "{kind}：{argv:?}");
        assert!(
            !argv.contains("改代码"),
            "{kind} argv 不该带 prompt：{argv:?}"
        );
        if kind == AgentType::Reasonix {
            for flag in [
                "--profile",
                "balanced",
                "--planner",
                "auto",
                "--sandbox-network",
                "--sandbox-bash",
                "--workspace-only",
            ] {
                assert!(argv.contains(flag), "reasonix argv 缺 {flag}：{argv:?}");
            }
        } else {
            assert_eq!(argv.lines().count(), 1, "{kind} 只该有 `acp`：{argv:?}");
        }
    }
}

/// qwen：复用 claude 的 stream-json 形态，argv 是 `--output-format stream-json --yolo`，
/// prompt 走 stdin 的**纯文本**（不是 JSON 信封）。
#[tokio::test]
async fn qwen_speaks_stream_json_with_the_prompt_as_plain_stdin_text() {
    let cli = FakeCli::replaying(concat!(
        r#"{"type":"system","subtype":"init","session_id":"qwen-int-1"}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"qwen3-coder-plus","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":4},"content":[{"type":"text","text":"ok"}]}}"#,
        "\n",
        // `result` 不给用量：assistant 的增量必须留着（不能被"清零"）。
        r#"{"type":"result","subtype":"success","session_id":"qwen-int-1","result":"ok","is_error":false}"#,
        "\n",
    ));
    let adapter = mc_runtime::Qwen::with_executable(cli.path());
    let handle = adapter
        .launch(LaunchRequest::new("改代码").with_model("qwen3-coder-plus"))
        .await
        .expect("launch");
    let (_, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.failure_reason, None);
    assert_eq!(outcome.output, "ok");
    assert_eq!(outcome.session_id.as_deref(), Some("qwen-int-1"));
    // qwen 把 cache_read 折进 input：10 - 4 = 6，再加 output 5 与 cache_read 4 ⇒ 15。
    assert_eq!(
        outcome
            .usage
            .iter()
            .map(|usage| usage.usage.total_tokens)
            .sum::<u64>(),
        15
    );

    let argv = cli.recorded_argv();
    assert!(argv.contains("--output-format"), "argv={argv:?}");
    assert!(argv.contains("stream-json"), "argv={argv:?}");
    assert!(argv.contains("--yolo"), "argv={argv:?}");
    assert!(argv.contains("--model"), "argv={argv:?}");
    // 上游 header 写的 `qwen -p (stream-json)` 只是**展示串**：`-p` 由 CLI 自己按
    // “stdin 是管道”判定，不由我们传（见 docs/33 §11 的偏离说明）。
    assert!(
        !argv.lines().any(|arg| arg == "-p"),
        "`-p` 不该出现在 argv：{argv:?}"
    );
    assert_eq!(cli.recorded_stdin(), "改代码", "prompt 是 stdin 纯文本");
}

/// openclaw：`agent --local --json … --message <prompt>` —— prompt 走 **argv**，
/// stdin 完全不参与；stdout 的 `payloads` blob 就是正文。
#[tokio::test]
async fn openclaw_passes_the_prompt_on_argv_and_reads_the_result_blob() {
    let cli = FakeCli::replaying(concat!(
        "openclaw: 冷启动中\n",
        r#"{"payloads":[{"text":"ok"}],"meta":{"durationMs":12,"agentMeta":{"sessionId":"openclaw-int-1","model":"deepseek-chat","usage":{"inputTokens":10,"outputTokens":5}}}}"#,
        "\n",
    ));
    let adapter = mc_runtime::Openclaw::with_executable(cli.path());
    let handle = adapter
        .launch(
            LaunchRequest::new("改代码")
                .with_model("deepseek-chat")
                .with_timeout(std::time::Duration::from_secs(42)),
        )
        .await
        .expect("launch");
    let (events, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.output, "ok");
    assert_eq!(outcome.session_id.as_deref(), Some("openclaw-int-1"));
    assert_eq!(
        outcome
            .usage
            .iter()
            .map(|usage| usage.usage.total_tokens)
            .sum::<u64>(),
        15
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Usage { .. })));

    let argv = cli.recorded_argv();
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(lines[0], "agent", "argv={argv:?}");
    assert!(lines.contains(&"--local"), "argv={argv:?}");
    assert!(lines.contains(&"--json"), "argv={argv:?}");
    assert!(lines.contains(&"--session-id"), "argv={argv:?}");
    assert!(lines.contains(&"--timeout"), "argv={argv:?}");
    assert!(lines.contains(&"42"), "timeout 走秒：{argv:?}");
    // 模型以 `--agent` 注入，且排在用户参数之后、`--message` 之前。
    assert!(lines.contains(&"--agent"), "argv={argv:?}");
    assert_eq!(lines.last(), Some(&"改代码"));
    assert_eq!(lines[lines.len() - 2], "--message");
    let agent_at = lines
        .iter()
        .position(|line| *line == "--agent")
        .expect("--agent");
    assert_eq!(lines[agent_at + 1], "deepseek-chat");
    assert!(cli.recorded_stdin().is_empty(), "openclaw 不读 stdin");
}

/// dsh：`--profile multica --stdio`，stdin 上的 `execute` 帧（spawn 即写）带 prompt，
/// 之后逐行读 `{v,type,request_id,…}` 帧；`extra_args` 被上游忽略（这里也要忽略）。
#[tokio::test]
async fn dsh_speaks_its_versioned_jsonl_protocol_and_ignores_extra_args() {
    let transcript = concat!(
        r#"{"v":1,"type":"ready","runtime":"dsh"}"#,
        "\n",
        r#"{"v":1,"type":"session","session_id":"dsh-int-1"}"#,
        "\n",
        r#"{"v":1,"type":"text","content":"ok"}"#,
        "\n",
        r#"{"v":1,"type":"usage","provider":"anthropic","model":"claude","input_tokens":10,"output_tokens":5}"#,
        "\n",
        r#"{"v":1,"type":"result","status":"completed","session_id":"dsh-int-1"}"#,
        "\n",
    );
    let cli = FakeCli::live_gated(transcript, "\"type\":\"execute\"");
    let adapter = mc_runtime::Dsh::with_executable(cli.path());
    let handle = adapter
        .launch(
            LaunchRequest::new("改代码")
                .with_model("anthropic/claude")
                .with_extra_args(["--definitely-not-a-dsh-flag"]),
        )
        .await
        .expect("launch");
    let (events, outcome) = handle.drain().await.expect("终态");

    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.failure_reason, None);
    assert_eq!(outcome.output, "ok");
    assert_eq!(outcome.session_id.as_deref(), Some("dsh-int-1"));
    assert_eq!(
        outcome
            .usage
            .iter()
            .map(|usage| usage.usage.total_tokens)
            .sum::<u64>(),
        15
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Usage { .. })));

    let argv = cli.recorded_argv();
    assert_eq!(argv.lines().next(), Some("--profile"), "argv={argv:?}");
    assert!(argv.contains("multica"), "argv={argv:?}");
    assert!(argv.contains("--stdio"), "argv={argv:?}");
    assert!(
        !argv.contains("--definitely-not-a-dsh-flag"),
        "dsh 的启动参数由 runtime 独占（上游不转发 extraArgs）：{argv:?}"
    );

    // prompt 在 `execute` 帧里（第一帧就是它），模型拆成 provider/model。
    let stdin = cli.recorded_stdin();
    assert!(stdin.contains("\"type\":\"execute\""), "{stdin}");
    assert!(
        stdin.contains("改代码"),
        "prompt 必须在 execute 帧里：{stdin}"
    );
    assert!(stdin.contains("\"provider\":\"anthropic\""), "{stdin}");
    assert!(stdin.contains("\"id\":\"claude\""), "{stdin}");
    assert!(stdin.contains("\"mcp_servers\":[]"), "{stdin}");
    assert!(stdin.ends_with('\n'), "帧要换行结尾");
    assert!(
        !argv.contains("改代码"),
        "dsh 的 argv 里不该有 prompt：{argv:?}"
    );
}
