//! pi-local 的端到端测试：走**公开 API** 把一次 run 真正跑起来。
//!
//! 与 crate 内 `crate::adapter_conformance!` 的分工：
//!
//! - 一致性套件（crate 内）保证"每个 adapter 都满足契约"，断言在 `conformance.rs` 里；
//! - 本文件保证"**M3-3 会怎么用就能怎么用**"：从注册表拿 adapter、构造 `LaunchRequest`、
//!   消费事件流、拿终态、取消/超时。它只依赖 `mc_runtime::*` 的公开面，
//!   所以接口一旦被改成 M3-3 用不了的样子，这里就会红。
//!
//! 全部用 [`FakeCli`]（现场生成的 `#!/bin/sh`），不依赖机器上装了 `pi`。

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use mc_runtime::{
    AdapterError, AdapterRegistry, AgentType, CancelOutcome, FailureReason, FakeCli, LaunchRequest,
    PiLocal, PiLocalConfig, RunStatus, RuntimeAdapter, RuntimeEvent,
};

/// 假 CLI + 指向它的 adapter（会话文件落在假 CLI 的临时目录里）。
fn harness(cli: &FakeCli) -> PiLocal {
    PiLocal::new(PiLocalConfig {
        executable: cli.path(),
        session_dir: cli.dir().join("sessions"),
        ..PiLocalConfig::default()
    })
}

const HAPPY_TRANSCRIPT: &str = concat!(
    r#"{"type":"agent_start"}"#,
    "\n",
    r#"{"type":"turn_start"}"#,
    "\n",
    r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"第一步："}}"#,
    "\n",
    r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"改完了"}}"#,
    "\n",
    r#"{"type":"tool_execution_start","toolName":"bash"}"#,
    "\n",
    r#"{"type":"tool_execution_end","toolName":"bash"}"#,
    "\n",
    r#"{"type":"turn_end","message":{"role":"assistant","model":"pi-1","usage":{"input":10,"output":5,"cacheRead":1,"cacheWrite":2,"totalTokens":18}}}"#,
    "\n",
);

#[tokio::test]
async fn spawn_stream_progress_exit_complete_lifecycle() {
    // 注册表 → adapter → launch → 事件流 → 终态，全链路。
    let cli = FakeCli::replaying(HAPPY_TRANSCRIPT);
    let registry = AdapterRegistry::new();
    registry.register(Arc::new(harness(&cli)));
    let adapter = registry.get(AgentType::Pi).expect("注册表里有 pi");

    let request = LaunchRequest::new("把 build 修好").with_model("pi-1");
    let handle = adapter.launch(request).await.expect("launch 成功");
    let (events, outcome) = handle.drain().await.expect("run 必须给出终态");

    // 1) 进程起来了，且 Started 是第一条。
    let Some(RuntimeEvent::Started { executable, pid }) = events.first() else {
        panic!("第一条事件必须是 Started：{:?}", events.first());
    };
    assert_eq!(executable.as_str(), cli.path().display().to_string());
    assert!(pid.is_some());

    // 2) 流式内容：正文、工具调用、用量都从 stdout 增量里来。
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "第一步：改完了");
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::ToolUse { tool, .. } if tool == "bash")));
    let turn_usage: u64 = events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Usage { usage, .. } => Some(usage.total_tokens),
            _ => None,
        })
        .sum();
    assert_eq!(turn_usage, 18);

    // 3) 终态：进程正常退出 ⇒ Completed，没有 failure_reason，退出码 0。
    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.failure_reason, None);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.output, "第一步：改完了");
    assert_eq!(outcome.usage.len(), 1);
    assert_eq!(outcome.usage[0].model, "pi-1");
    assert_eq!(outcome.usage[0].usage.total_tokens, 18);
    // 会话 id 是"下次续跑"的唯一凭据，必须回传。
    let session_id = outcome.session_id.expect("会话 id");
    let session_path = std::path::Path::new(&session_id);
    assert!(
        session_path.extension().is_some_and(|ext| ext == "jsonl"),
        "{session_id}"
    );
    assert!(session_path.exists(), "会话文件已落地");
    // 本片不写库：run 的产物只有会话文件与事件流。
    assert!(outcome.duration_ms < 60_000);
}

#[tokio::test]
async fn nonzero_exit_maps_to_agent_error() {
    let cli = FakeCli::failing(HAPPY_TRANSCRIPT, 7, "pi: upstream 502");
    let adapter = harness(&cli);
    let outcome = adapter
        .launch(LaunchRequest::new("会失败的 run"))
        .await
        .expect("启动本身是成功的")
        .outcome()
        .await
        .expect("终态");

    assert_eq!(outcome.status, RunStatus::Failed);
    assert_eq!(outcome.failure_reason, Some(FailureReason::AgentError));
    assert_eq!(outcome.exit_code, Some(7));
    let error = outcome.error.expect("错误串");
    assert!(error.contains("exited with error"), "{error}");
    // stderr 尾部要带回来，否则 M3-3 落库的失败原因只剩一个退出码。
    assert!(outcome.stderr_tail.contains("upstream 502"));
}

#[tokio::test]
async fn timeout_maps_to_timeout() {
    let cli = FakeCli::sleeping(30);
    let adapter = harness(&cli);
    let request = LaunchRequest::new("卡住了").with_timeout(Duration::from_millis(250));
    let (events, outcome) = adapter
        .launch(request)
        .await
        .expect("launch 成功")
        .drain()
        .await
        .expect("终态");

    assert_eq!(outcome.status, RunStatus::Timeout);
    assert_eq!(outcome.failure_reason, Some(FailureReason::Timeout));
    assert_eq!(outcome.exit_code, None);
    assert!(outcome.error.expect("错误串").contains("timed out"));
    // 超时必须真的把进程杀掉，否则一个卡死的 CLI 会一直占着会话锁。
    let pid = match events.first() {
        Some(RuntimeEvent::Started { pid: Some(pid), .. }) => *pid,
        _ => panic!("第一条事件必须是 Started，且带 pid"),
    };
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "超时后子进程必须已被回收"
    );
}

#[tokio::test]
async fn cancel_maps_to_cancelled_and_is_idempotent() {
    let cli = FakeCli::replaying_then_sleeping(HAPPY_TRANSCRIPT, 30);
    let adapter = harness(&cli);
    let mut handle = adapter
        .launch(LaunchRequest::new("取消我"))
        .await
        .expect("launch 成功");
    let run_id = handle.run_id().clone();

    // 等正文真的开始流出来再取消，避免"取消一个还没起的 run"这种假绿。
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
        CancelOutcome::Signalled
    );
    assert!(matches!(
        adapter.cancel(&run_id).await.expect("重复取消"),
        CancelOutcome::Signalled | CancelOutcome::NotRunning
    ));

    while handle.next_event().await.is_some() {}
    let outcome = handle.outcome().await.expect("终态");
    assert_eq!(outcome.status, RunStatus::Cancelled);
    assert_eq!(outcome.failure_reason, Some(FailureReason::Manual));
    assert_eq!(outcome.error.as_deref(), Some("execution cancelled"));
}

#[tokio::test]
async fn session_file_is_exclusive_and_released_after_the_run() {
    // 上游用 flock 保证"同一个 JSONL 会话文件不能被两个 run 同时续写"；
    // 我们改成进程内注册表（见 pi_local 模块文档），行为必须一致。
    let cli = FakeCli::sleeping(30);
    let adapter = harness(&cli);
    let shared = cli.dir().join("sessions").join("shared.jsonl");
    let resume = shared.display().to_string();

    let mut first = adapter
        .launch(LaunchRequest::new("a").with_resume_session(resume.clone()))
        .await
        .expect("第一次占锁");
    let conflict = adapter
        .launch(LaunchRequest::new("b").with_resume_session(resume.clone()))
        .await
        .expect_err("同一个会话文件不能被两个 run 同时写");
    assert!(
        matches!(conflict, AdapterError::SessionBusy { .. }),
        "{conflict:?}"
    );

    // 收尾：取消第一个 run，锁随 Drop 释放，随后同一个会话文件可以再被续跑。
    let run_id = first.run_id().clone();
    adapter.cancel(&run_id).await.expect("取消");
    while first.next_event().await.is_some() {}
    first.outcome().await.expect("终态");

    let mut third = adapter
        .launch(LaunchRequest::new("c").with_resume_session(resume))
        .await
        .expect("前一个 run 结束后同一个会话文件可以续跑");
    adapter.cancel(third.run_id()).await.expect("收尾取消");
    while third.next_event().await.is_some() {}
    third.outcome().await.expect("终态");
}

#[tokio::test]
async fn empty_prompt_is_rejected_before_spawning() {
    // 上游注释：pi 会先 trim stdin，空白 prompt 会变成"成功的空 turn"，
    // 所以必须在启动前拒掉（否则 M3-3 会记一次成功但什么都没有的 run）。
    let adapter = harness(&FakeCli::version("pi 0.83.2\n"));
    let error = adapter
        .launch(LaunchRequest::new("   \n"))
        .await
        .expect_err("空白 prompt 必须被拒");
    assert!(
        matches!(error, AdapterError::EmptyPrompt { .. }),
        "{error:?}"
    );
    assert!(
        !error.is_runtime_offline(),
        "prompt 问题是调用方的错，不是机器离线"
    );
}

#[tokio::test]
async fn missing_executable_is_reported_before_spawning() {
    let adapter = PiLocal::new(PiLocalConfig {
        executable: std::path::PathBuf::from("/nonexistent/pi-does-not-exist"),
        ..PiLocalConfig::default()
    });
    let error = adapter
        .launch(LaunchRequest::new("hi"))
        .await
        .expect_err("可执行文件不存在");
    assert!(
        matches!(error, AdapterError::ExecutableUnavailable { .. }),
        "{error:?}"
    );
    assert!(error.is_runtime_offline(), "缺二进制属于 runtime_offline");
}
