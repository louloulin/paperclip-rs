//! R364 集成测试 — `pc-acpx` buildRuntime 拆分组件协同验证。
//!
//! 覆盖：resolve_built_in_agent_command + build_startup_step_metrics +
//! PreparedRuntime builder 组合成真实可启动的"运行时描述"。

use pc_acpx::{
    build_startup_step_metrics, format_timeout_start_log_line, resolve_built_in_agent_command,
    resolve_engine_settings, AcpxEngineOptions, BuiltInAgentCommand, Platform, PreparedRuntime,
    PreparedRuntimeMode, PreparedRuntimePermissionMode, ResolveBuiltInAgentCommandInput,
    StartupMetricsSource, StartupStepMetrics, TimeoutResolution,
};
use std::path::PathBuf;
use std::sync::Arc;

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pc-acpx-r364-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

struct CountingSource {
    execs: u64,
    exec_ms: u64,
    get_ms: u64,
}

impl StartupMetricsSource for CountingSource {
    fn round_trips(&self) -> Option<u64> {
        Some(self.execs)
    }
    fn provider_exec_ms(&self) -> Option<u64> {
        Some(self.exec_ms)
    }
    fn provider_get_ms(&self) -> Option<u64> {
        Some(self.get_ms)
    }
}

#[tokio::test]
async fn build_runtime_pipeline_combines_agent_command_and_metrics() {
    let root = unique_root("pipeline");
    let bin_dir = root.join("node_modules/.bin");
    tokio::fs::create_dir_all(&bin_dir).await.unwrap();
    let bin_path = bin_dir.join("claude-agent-acp");
    tokio::fs::write(&bin_path, "#!/bin/sh\n").await.unwrap();

    let settings = resolve_engine_settings(
        &AcpxEngineOptions {
            adapter_type: Some("claude_local".into()),
            module_dir: Some(root.clone()),
            package_root_dir: Some(root.clone()),
            ..Default::default()
        },
        PathBuf::from("/unused/fallback").as_path(),
    );
    assert_eq!(settings.adapter_type, "claude_local");

    let agent_command = resolve_built_in_agent_command(&ResolveBuiltInAgentCommandInput {
        agent: "claude".into(),
        package_root_dir: settings.package_root_dir.to_string_lossy().into_owned(),
        execution_target_is_remote: false,
        platform: Platform::Posix,
    })
    .await
    .expect("built-in agent command");
    let BuiltInAgentCommand { command, .. } = &agent_command;
    assert_eq!(command, &bin_path.to_string_lossy().into_owned());

    let source: Arc<dyn StartupMetricsSource> = Arc::new(CountingSource {
        execs: 3,
        exec_ms: 120,
        get_ms: 60,
    });
    let metrics = build_startup_step_metrics(Some(source));
    let round_trips = metrics.round_trips.clone().expect("callback");
    assert_eq!(round_trips(), 3);

    let runtime = PreparedRuntime::builder("claude")
        .mode(PreparedRuntimeMode::Persistent)
        .cwd("/repo")
        .permission_mode(PreparedRuntimePermissionMode::ApproveAll)
        .timeout_sec(0)
        .timeout_resolution(TimeoutResolution {
            timeout_sec: 0,
            source: "default".into(),
            note: None,
        })
        .agent_command(agent_command)
        .step_metrics(metrics)
        .build();
    assert_eq!(runtime.acpx_agent, "claude");
    assert_eq!(runtime.cwd, PathBuf::from("/repo"));
    assert_eq!(
        runtime.agent_command.as_ref().map(|c| c.command.clone()),
        Some(bin_path.to_string_lossy().into_owned())
    );
    assert!(runtime.step_metrics.round_trips.is_some());

    assert_eq!(
        format_timeout_start_log_line(&runtime.timeout_resolution),
        "Adapter execution timeout: none"
    );

    let _ = tokio::fs::remove_dir_all(&root).await;
}

#[tokio::test]
async fn gemini_agent_bypasses_ancestor_lookup() {
    let input = ResolveBuiltInAgentCommandInput {
        agent: "gemini".into(),
        package_root_dir: "/nonexistent".into(),
        execution_target_is_remote: false,
        platform: Platform::Posix,
    };
    let command = resolve_built_in_agent_command(&input).await.unwrap();
    assert_eq!(command.command, "gemini --acp");
}

#[tokio::test]
async fn empty_metrics_can_be_built_then_used() {
    let metrics: StartupStepMetrics = build_startup_step_metrics(None);
    assert!(metrics.round_trips.is_none());
    assert!(metrics.provider_exec_ms.is_none());
    assert!(metrics.provider_get_ms.is_none());
}

#[tokio::test]
async fn timeout_resolution_lines_carry_source_and_note() {
    let resolution = TimeoutResolution {
        timeout_sec: 14400,
        source: "sandbox default".into(),
        note: Some("(sandbox default; set adapterConfig.timeoutSec to override)".into()),
    };
    let line = format_timeout_start_log_line(&resolution);
    assert!(line.contains("timeoutSec=14400"));
    assert!(line.contains("sandbox default"));
    assert!(line.contains("set adapterConfig.timeoutSec to override"));
}
