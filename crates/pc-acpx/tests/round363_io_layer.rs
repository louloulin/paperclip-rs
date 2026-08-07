//! R363 集成测试 — `pc-acpx` I/O 层端到端验证。
//!
//! 用例覆盖：settings 解析 + fs_ops 原子写 + find_ancestor_bin 走目录树查找，
//! 证明新模块协同工作可以完成"解析引擎设置 → 写入 staging 文件 → 查找
//! node_modules/.bin 工具 → 读取文件验证"的完整链路。

use pc_acpx::{
    find_ancestor_bin, path_exists, resolve_engine_settings, write_file_atomically,
    AcpxEngineOptions, Platform, WriteFileAtomicallyInput,
};
use std::path::PathBuf;

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pc-acpx-r363-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

#[tokio::test]
async fn settings_to_stage_file_pipeline_writes_to_resolved_paths() {
    let root = unique_root("pipeline");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let settings = resolve_engine_settings(
        &AcpxEngineOptions {
            adapter_type: Some("claude_local".into()),
            module_dir: Some(root.clone()),
            ..Default::default()
        },
        PathBuf::from("/unused/fallback").as_path(),
    );
    assert_eq!(settings.adapter_type, "claude_local");
    assert!(settings.module_dir.is_absolute());
    assert!(settings.package_root_dir.is_absolute());

    // Stage a settings.json file using the resolved module_dir.
    let target = settings.module_dir.join("settings.json");
    write_file_atomically(WriteFileAtomicallyInput::new(
        &target,
        "{\"adapter\":\"claude\"}",
        0o644,
    ))
    .await
    .unwrap();

    assert!(path_exists(&target).await);
    let read = tokio::fs::read_to_string(&target).await.unwrap();
    assert_eq!(read, "{\"adapter\":\"claude\"}");

    let _ = tokio::fs::remove_dir_all(&root).await;
}

#[tokio::test]
async fn default_adapter_type_flows_through_pipeline() {
    let root = unique_root("default");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let settings = resolve_engine_settings(&AcpxEngineOptions::default(), root.as_path());
    assert_eq!(settings.adapter_type, "acp_engine");
    assert!(settings.module_dir.ends_with(root.file_name().unwrap()));

    let _ = tokio::fs::remove_dir_all(&root).await;
}

#[tokio::test]
async fn find_ancestor_bin_then_write_atomically_round_trip() {
    let root = unique_root("round");
    let package_dir = root.join("packages/cli");
    let bin_dir = root.join("node_modules/.bin");
    tokio::fs::create_dir_all(&package_dir).await.unwrap();
    tokio::fs::create_dir_all(&bin_dir).await.unwrap();

    // The acpx-style shim script.
    let shim = bin_dir.join("paperclip-acp");
    tokio::fs::write(&shim, "#!/bin/sh\necho paperclip-acp\n")
        .await
        .unwrap();

    // Walk up from the package dir to find the shim.
    let found = find_ancestor_bin(&package_dir, "paperclip-acp", Platform::Posix)
        .await
        .expect("found");
    assert_eq!(found, shim);

    // Drop a state file next to the shim, mimicking the staging flow.
    let state_path = bin_dir.join("state.json");
    write_file_atomically(WriteFileAtomicallyInput::new(
        &state_path,
        "{\"ok\":true}",
        0o600,
    ))
    .await
    .unwrap();
    assert!(path_exists(&state_path).await);

    let _ = tokio::fs::remove_dir_all(&root).await;
}

#[tokio::test]
async fn write_file_atomically_overwrites_with_changing_contents() {
    let root = unique_root("overwrite");
    tokio::fs::create_dir_all(&root).await.unwrap();
    let target = root.join("config.json");
    for i in 0..3 {
        write_file_atomically(WriteFileAtomicallyInput::new(
            &target,
            format!("{{\"version\":{i}}}"),
            0o644,
        ))
        .await
        .unwrap();
        let read = tokio::fs::read_to_string(&target).await.unwrap();
        assert_eq!(read, format!("{{\"version\":{i}}}"));
    }
    let _ = tokio::fs::remove_dir_all(&root).await;
}
