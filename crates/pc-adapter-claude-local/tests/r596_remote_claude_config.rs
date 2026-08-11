use pc_acpx::bridge_executor::LocalProcessBridgeRunner;
use pc_adapter_claude_local::claude_remote_config::{
    build_remote_claude_config_materialization_command,
    materialize_remote_claude_config_for_target, materialize_remote_claude_config_with_runner,
    RemoteClaudeConfigMaterializationInput,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

fn temp_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "paperclip-r596-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

#[test]
fn command_quotes_remote_paths_and_preserves_home_expansion() {
    let command = build_remote_claude_config_materialization_command(
        "/remote/claude config/'managed'",
        "/remote/seed dir/'snapshot'",
    );

    assert!(command.contains("mkdir -p '/remote/claude config/'\"'\"'managed'\"'\"''"));
    assert!(command.contains("[ -d '/remote/seed dir/'\"'\"'snapshot'\"'\"'' ]"));
    assert!(command.contains("${HOME:-}"));
    assert!(command.contains("${HOME}/.claude/${file}"));
    assert!(!command.contains("mkdir -p /remote/claude config"));
}

#[tokio::test]
async fn real_shell_materializes_seed_and_home_credentials_without_overwrite() {
    let root = temp_root("materialize");
    let seed = root.join("seed snapshot");
    let target = root.join("managed config");
    let home = root.join("operator home");
    std::fs::create_dir_all(seed.join("nested")).expect("create seed");
    std::fs::create_dir_all(home.join(".claude")).expect("create home config");
    std::fs::write(seed.join("settings.json"), "seed-settings").expect("write settings");
    std::fs::write(seed.join("nested/config.json"), "nested").expect("write nested");
    std::fs::write(seed.join("credentials.json"), "seed-credential")
        .expect("write seed credential");
    std::fs::write(home.join(".claude/credentials.json"), "home-credential")
        .expect("write home credential");
    std::fs::write(
        home.join(".claude/.credentials.json"),
        "legacy-home-credential",
    )
    .expect("write legacy credential");

    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), home.to_string_lossy().into_owned());
    let runner = Arc::new(LocalProcessBridgeRunner);
    let result = materialize_remote_claude_config_with_runner(
        runner,
        &RemoteClaudeConfigMaterializationInput {
            remote_cwd: root.to_string_lossy().into_owned(),
            remote_claude_config_dir: target.to_string_lossy().into_owned(),
            remote_claude_config_seed_dir: seed.to_string_lossy().into_owned(),
            env,
            timeout_ms: 15_000,
        },
    )
    .await
    .expect("materialize config");

    assert!(result.succeeded());
    assert_eq!(read(&target.join("settings.json")), "seed-settings");
    assert_eq!(read(&target.join("nested/config.json")), "nested");
    assert_eq!(
        read(&target.join("credentials.json")),
        "seed-credential",
        "HOME fallback must not overwrite the seeded credential"
    );
    assert_eq!(
        read(&target.join(".credentials.json")),
        "legacy-home-credential"
    );

    std::fs::remove_dir_all(root).expect("cleanup temp root");
}

#[tokio::test]
async fn missing_seed_still_creates_target_and_imports_home_credentials() {
    let root = temp_root("missing-seed");
    let seed = root.join("missing seed");
    let target = root.join("managed config");
    let home = root.join("operator home");
    std::fs::create_dir_all(home.join(".claude")).expect("create home config");
    std::fs::write(home.join(".claude/credentials.json"), "home-credential")
        .expect("write credential");

    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), home.to_string_lossy().into_owned());
    materialize_remote_claude_config_with_runner(
        Arc::new(LocalProcessBridgeRunner),
        &RemoteClaudeConfigMaterializationInput {
            remote_cwd: root.to_string_lossy().into_owned(),
            remote_claude_config_dir: target.to_string_lossy().into_owned(),
            remote_claude_config_seed_dir: seed.to_string_lossy().into_owned(),
            env,
            timeout_ms: 15_000,
        },
    )
    .await
    .expect("materialize without seed");

    assert!(target.is_dir());
    assert_eq!(read(&target.join("credentials.json")), "home-credential");
    std::fs::remove_dir_all(root).expect("cleanup temp root");
}

#[tokio::test]
async fn target_router_rejects_local_and_unconfigured_sandbox_targets() {
    let root = temp_root("target-router");
    let input = RemoteClaudeConfigMaterializationInput {
        remote_cwd: root.to_string_lossy().into_owned(),
        remote_claude_config_dir: root.join("config").to_string_lossy().into_owned(),
        remote_claude_config_seed_dir: root.join("seed").to_string_lossy().into_owned(),
        env: BTreeMap::new(),
        timeout_ms: 15_000,
    };
    let local = pc_acpx::execution_target::parse_adapter_execution_target(&serde_json::json!({
        "kind": "local"
    }))
    .expect("local target");
    let sandbox = pc_acpx::execution_target::parse_adapter_execution_target(&serde_json::json!({
        "kind": "remote",
        "transport": "sandbox",
        "remoteCwd": "/remote/workspace"
    }))
    .expect("sandbox target");

    let local_error = materialize_remote_claude_config_for_target(&local, &input)
        .await
        .expect_err("local target must be rejected");
    assert!(local_error.contains("local execution target"));
    let sandbox_error = materialize_remote_claude_config_for_target(&sandbox, &input)
        .await
        .expect_err("sandbox without provider must be rejected");
    assert!(sandbox_error.contains("sandbox provider runner"));
    std::fs::remove_dir_all(root).expect("cleanup temp root");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
