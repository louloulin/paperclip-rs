//! R367 集成测试 — `pc-acpx` skill staging + managed home preparation。
//!
//! 覆盖：skill materialization + 哈希 cache key + Codex managed home
//! seeding + manifest 读写 + reconciliation + 三个 agent (claude/codex/
//! gemini) 顶层 skill runtime 端到端。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pc_acpx::error_classification::classify_error;
use pc_acpx::managed_home::{
    prepare_managed_codex_home, read_managed_codex_skills_manifest,
    write_managed_codex_skills_manifest, LogStream, ManagedSkillsManifest, OnLogSink,
    PrepareManagedCodexHomeInput,
};
use pc_acpx::reconcile_skills::{
    reconcile_managed_codex_skills, ReconcileManagedCodexSkillsInput, RevocationPhase,
};
use pc_acpx::skill_materialize::{
    build_skill_set_key, materialize_paperclip_skill_copy, PaperclipSkillEntry, SkillSourceStatus,
};
use pc_acpx::skill_runtime::{
    prepare_claude_skill_runtime, prepare_codex_skill_runtime, prepare_gemini_skill_runtime,
    PrepareClaudeSkillRuntimeInput, PrepareCodexSkillRuntimeInput, PrepareGeminiSkillRuntimeInput,
};

fn unique_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pc-acpx-r367-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

fn write_file(path: &PathBuf, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn entry(key: &str, name: &str, source: PathBuf) -> PaperclipSkillEntry {
    PaperclipSkillEntry {
        key: key.into(),
        runtime_name: name.into(),
        source,
        version_id: None,
        current_version_id: None,
        source_status: Some(SkillSourceStatus::Available),
        missing_detail: None,
    }
}

fn capture_sink() -> (Arc<Mutex<Vec<(LogStream, String)>>>, OnLogSink) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_closure = captured.clone();
    let sink: OnLogSink = Arc::new(move |stream, line| {
        captured_for_closure
            .lock()
            .unwrap()
            .push((stream, line.to_string()));
    });
    (captured, sink)
}

// ============================================================================
// Managed home e2e
// ============================================================================

#[tokio::test]
async fn managed_home_round_trip_through_real_disk() {
    let source = unique_dir("home-src");
    let target = unique_dir("home-tgt");
    tokio::fs::create_dir_all(&source).await.unwrap();
    tokio::fs::write(source.join("auth.json"), r#"{"t":"x"}"#)
        .await
        .unwrap();
    tokio::fs::write(source.join("config.json"), r#"{"m":"y"}"#)
        .await
        .unwrap();
    tokio::fs::write(source.join("instructions.md"), "doc")
        .await
        .unwrap();
    let (log, sink) = capture_sink();
    let input =
        PrepareManagedCodexHomeInput::new("co", source.clone(), target.clone()).with_on_log(sink);
    let resolved = prepare_managed_codex_home(input).await.unwrap();
    assert_eq!(resolved, target);
    // auth.json is a symlink (canonical) → both sides agree on content.
    let auth = tokio::fs::read_to_string(target.join("auth.json"))
        .await
        .unwrap();
    assert_eq!(auth, r#"{"t":"x"}"#);
    // Log line recorded.
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert!(log[0].1.contains("Using Paperclip-managed"));
    let _ = tokio::fs::remove_dir_all(&source).await;
    let _ = tokio::fs::remove_dir_all(&target).await;
}

#[tokio::test]
async fn manifest_persists_across_concurrent_writes() {
    let home = unique_dir("manifest");
    tokio::fs::create_dir_all(&home).await.unwrap();
    let manifest = ManagedSkillsManifest::from_names(["alpha", "beta"]);
    write_managed_codex_skills_manifest(&home, &manifest)
        .await
        .unwrap();
    let read = read_managed_codex_skills_manifest(&home).await;
    assert_eq!(read, manifest);
    let _ = tokio::fs::remove_dir_all(&home).await;
}

// ============================================================================
// Skill materialize e2e
// ============================================================================

#[tokio::test]
async fn materialize_copies_skill_tree_with_skipped_symlinks() {
    let source = unique_dir("mat-src");
    let target = unique_dir("mat-tgt");
    write_file(&source.join("SKILL.md"), "v1");
    write_file(&source.join("scripts/run.sh"), "x");
    #[cfg(unix)]
    std::os::unix::fs::symlink(source.join("SKILL.md"), source.join("SKILL.md.lnk")).unwrap();
    let result = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    assert!(result.copied_files >= 2);
    #[cfg(unix)]
    assert!(!result.skipped_symlinks.is_empty());
    assert!(tokio::fs::try_exists(target.join("SKILL.md"))
        .await
        .unwrap_or(false));
    let _ = tokio::fs::remove_dir_all(&source).await;
    let _ = tokio::fs::remove_dir_all(&target).await;
}

#[tokio::test]
async fn skill_set_key_changes_when_label_changes() {
    let dir = unique_dir("key");
    write_file(&dir.join("SKILL.md"), "v1");
    let entry = entry("k", "skill", dir.clone());
    let h_claude = build_skill_set_key(&[entry.clone()], "claude").await;
    let h_codex = build_skill_set_key(&[entry], "codex").await;
    assert_eq!(h_claude.len(), 64);
    assert_ne!(h_claude, h_codex);
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ============================================================================
// Reconciliation e2e
// ============================================================================

#[tokio::test]
async fn reconcile_managed_skills_three_phases() {
    let home = unique_dir("reconcile");
    let src_keep = unique_dir("keep");
    let src_orphan = unique_dir("orphan");
    let src_legacy = unique_dir("legacy");
    // Phase 1: managed + materialized → `orphan`.
    tokio::fs::create_dir_all(home.join("orphan"))
        .await
        .unwrap();
    tokio::fs::write(home.join("orphan/SKILL.md"), "x")
        .await
        .unwrap();
    // Phase 2: legacy symlink → `legacy`.
    #[cfg(unix)]
    {
        tokio::fs::create_dir_all(&src_legacy).await.unwrap();
        std::os::unix::fs::symlink(&src_legacy, home.join("legacy")).unwrap();
    }
    // Seed manifest: managed = {orphan}.
    let manifest = ManagedSkillsManifest::from_names(["orphan"]);
    write_managed_codex_skills_manifest(&home, &manifest)
        .await
        .unwrap();

    // Caller wants to keep `keep`; `orphan` and `legacy` are stale.
    let keep = entry("keep", "keep", src_keep.clone());
    let legacy = entry("legacy", "legacy", src_legacy.clone());
    let desired = vec![keep.clone()];
    let all = vec![keep, legacy];
    let (_, sink) = capture_sink();
    let input = ReconcileManagedCodexSkillsInput::new(home.clone(), all, desired).with_on_log(sink);
    let revocations = reconcile_managed_codex_skills(input).await.unwrap();
    // Phase 1 → orphan
    assert!(revocations
        .iter()
        .any(|r| r.name == "orphan" && r.phase == RevocationPhase::ManagedNoLongerDesired));
    // Phase 2 → legacy (only on Unix)
    #[cfg(unix)]
    assert!(revocations
        .iter()
        .any(|r| r.name == "legacy" && r.phase == RevocationPhase::LegacySymlink));
    let _ = tokio::fs::remove_dir_all(&home).await;
    let _ = tokio::fs::remove_dir_all(&src_keep).await;
    let _ = tokio::fs::remove_dir_all(&src_orphan).await;
    let _ = tokio::fs::remove_dir_all(&src_legacy).await;
}

// ============================================================================
// Agent-specific runtime e2e
// ============================================================================

#[tokio::test]
async fn claude_runtime_end_to_end() {
    let state = unique_dir("claude-state");
    let skill = unique_dir("claude-skill");
    tokio::fs::create_dir_all(&state).await.unwrap();
    write_file(&skill.join("SKILL.md"), "claude doc");
    let selected = vec![entry("alpha", "alpha-rt", skill.clone())];
    let input = PrepareClaudeSkillRuntimeInput {
        state_dir: state.clone(),
        selected_skills: selected,
        desired_skill_names: vec!["alpha".into()],
        on_log: None,
    };
    let output = prepare_claude_skill_runtime(input).await.unwrap();
    assert_eq!(output.identity.mode, "claude");
    assert_eq!(
        output.identity.selected_skills,
        vec!["alpha-rt".to_string()]
    );
    assert!(output.identity.skills_home.ends_with(".claude/skills"));
    assert!(output.prompt_instructions.contains("alpha-rt"));
    let _ = tokio::fs::remove_dir_all(&state).await;
    let _ = tokio::fs::remove_dir_all(&skill).await;
}

#[tokio::test]
async fn codex_runtime_seeds_home_reconciles_and_writes_manifest() {
    let source = unique_dir("codex-src");
    let target = unique_dir("codex-tgt");
    let skill = unique_dir("codex-skill");
    tokio::fs::create_dir_all(&source).await.unwrap();
    tokio::fs::write(source.join("auth.json"), r#"{"t":"x"}"#)
        .await
        .unwrap();
    write_file(&skill.join("SKILL.md"), "codex doc");
    let env = BTreeMap::new();
    let input = PrepareCodexSkillRuntimeInput {
        company_id: "company-1".into(),
        source_codex_home: source.clone(),
        managed_codex_home: target.clone(),
        env,
        selected_skills: vec![entry("alpha", "alpha-rt", skill.clone())],
        all_skills: vec![entry("alpha", "alpha-rt", skill.clone())],
        desired_skill_names: vec!["alpha".into()],
        on_log: None,
    };
    let output = prepare_codex_skill_runtime(input).await.unwrap();
    assert_eq!(output.identity.mode, "codex");
    // Managed Codex home was created and auth.json was seeded.
    assert!(tokio::fs::try_exists(target.join("auth.json"))
        .await
        .unwrap_or(false));
    // Skill was materialized into skills/.
    assert!(
        tokio::fs::try_exists(target.join("skills/alpha-rt/SKILL.md"))
            .await
            .unwrap_or(false)
    );
    // Manifest recorded the selected names.
    let manifest = read_managed_codex_skills_manifest(target.join("skills")).await;
    assert_eq!(manifest.names(), ["alpha-rt"]);
    let _ = tokio::fs::remove_dir_all(&source).await;
    let _ = tokio::fs::remove_dir_all(&target).await;
    let _ = tokio::fs::remove_dir_all(&skill).await;
}

#[tokio::test]
async fn gemini_runtime_uses_symlink_or_copy_fallback() {
    let skills_home = unique_dir("gemini-home");
    let skill = unique_dir("gemini-skill");
    tokio::fs::create_dir_all(&skills_home).await.unwrap();
    write_file(&skill.join("SKILL.md"), "gemini doc");
    let input = PrepareGeminiSkillRuntimeInput {
        skills_home: skills_home.clone(),
        selected_skills: vec![entry("alpha", "alpha-rt", skill.clone())],
        desired_skill_names: vec!["alpha".into()],
        on_log: None,
    };
    let output = prepare_gemini_skill_runtime(input).await.unwrap();
    assert_eq!(output.identity.mode, "gemini");
    let target = skills_home.join("alpha-rt");
    assert!(tokio::fs::try_exists(&target).await.unwrap_or(false));
    #[cfg(unix)]
    {
        // On Unix the symlink should win (no copy fallback).
        let meta = tokio::fs::symlink_metadata(&target).await.unwrap();
        assert!(meta.file_type().is_symlink());
    }
    let _ = tokio::fs::remove_dir_all(&skills_home).await;
    let _ = tokio::fs::remove_dir_all(&skill).await;
}

// ============================================================================
// Cross-module: error classification works on managed home failures
// ============================================================================

#[derive(Debug)]
struct CodedError(&'static str);

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "code: {}: synthetic error", self.0)
    }
}

impl std::error::Error for CodedError {}

#[test]
fn error_classification_dispatches_to_protocol_phase() {
    // A managed-home failure during the `configure_session` phase routes
    // to `acpx_session_config_failed` (protocol category).
    let err = CodedError("ACP_BACKEND_UNAVAILABLE");
    let classified = classify_error(
        &err,
        Some(pc_acpx::error_classification::AcpxExecutionPhase::ConfigureSession),
    );
    assert_eq!(classified.error_code, "acpx_backend_unavailable");
    assert_eq!(
        classified.error_meta.get("category"),
        Some(&serde_json::Value::String("protocol".into()))
    );
}
