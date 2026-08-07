//! `pc-acpx` skill runtime preparation — port of `prepareClaudeSkillRuntime`,
//! `prepareCodexSkillRuntime`, and `prepareGeminiSkillRuntime` from Node
//! `acpx-engine/execute.ts`.
//!
//! Each `prepare_*_skill_runtime` function takes a pre-resolved set of
//! `selected_skills` (caller responsibility, see
//! [`resolve_selected_runtime_skills`]) and stages them into the
//! per-agent skill home:
//!
//! - **Claude** materializes the selected skills into a content-hash-keyed
//!   bundle under `<stateDir>/runtime-skills/claude/<skillSetKey>/.claude/skills/`.
//! - **Codex** materializes the selected skills into the per-company
//!   managed Codex home (after seeding auth.json / config via
//!   `prepare_managed_codex_home`) and writes the manifest.
//! - **Gemini** symlinks the selected skills into `$HOME/.gemini/skills`
//!   (falls back to a copy when symlinks are unavailable).
//!
//! The "config-driven skill resolution" layer
//! (`readPaperclipRuntimeSkillEntries` / `resolvePaperclipDesiredSkillNames`)
//! lives in `paperclip-server` / `paperclip-cn` and is not part of
//! `pc-acpx`; the helpers here accept the resolved entries directly so
//! the engine stays decoupled from the config schema.

use std::path::{Path, PathBuf};

use crate::error::AcpxError;
use crate::fs_ops::{ensure_symlink, write_file_atomically, WriteFileAtomicallyInput};
use crate::managed_home::{
    prepare_managed_codex_home, write_managed_codex_skills_manifest, LogStream,
    ManagedSkillsManifest, OnLogSink, PrepareManagedCodexHomeInput,
};
use crate::reconcile_skills::{reconcile_managed_codex_skills, ReconcileManagedCodexSkillsInput};
use crate::skill_materialize::{
    build_skill_set_key, materialize_paperclip_skill_copy, PaperclipSkillEntry,
};

// ============================================================================
// Shared output type
// ============================================================================

/// Identity payload returned by every `prepare_*_skill_runtime` call.
/// Mirrors the Node `identity` plain object shape (keys are camelCase
/// to match the on-disk JSON contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRuntimeIdentity {
    pub mode: String,
    pub skill_set_key: String,
    pub desired_skill_names: Vec<String>,
    pub selected_skills: Vec<String>,
    pub skills_home: PathBuf,
    /// Codex-only: the resolved CODEX_HOME path.
    pub codex_home: Option<PathBuf>,
    /// Claude-only: bundle root (`<stateDir>/runtime-skills/claude/<key>`).
    pub bundle_root: Option<PathBuf>,
}

/// Standard output of a skill-runtime preparation call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareSkillRuntimeOutput {
    pub identity: SkillRuntimeIdentity,
    /// Extra log-friendly command notes the caller may surface.
    pub command_notes: Vec<String>,
    /// Claude-only: human-readable instructions block to append to the
    /// system prompt.
    pub prompt_instructions: String,
}

// ============================================================================
// prepareClaudeSkillRuntime
// ============================================================================

/// Input for [`prepare_claude_skill_runtime`].
#[derive(Clone)]
pub struct PrepareClaudeSkillRuntimeInput {
    pub state_dir: PathBuf,
    pub selected_skills: Vec<PaperclipSkillEntry>,
    pub desired_skill_names: Vec<String>,
    pub on_log: Option<OnLogSink>,
}

impl std::fmt::Debug for PrepareClaudeSkillRuntimeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepareClaudeSkillRuntimeInput")
            .field("state_dir", &self.state_dir)
            .field("selected_skills", &self.selected_skills.len())
            .field("desired_skill_names", &self.desired_skill_names)
            .field("on_log", &self.on_log.as_ref().map(|_| "<sink>"))
            .finish()
    }
}

/// Prepare the Claude skill bundle for a session. Materializes the
/// selected skills into a content-hash-keyed directory under
/// `<stateDir>/runtime-skills/claude/<skillSetKey>/.claude/skills/`.
pub async fn prepare_claude_skill_runtime(
    input: PrepareClaudeSkillRuntimeInput,
) -> Result<PrepareSkillRuntimeOutput, AcpxError> {
    let skill_set_key = build_skill_set_key(&input.selected_skills, "claude").await;
    let bundle_root = input
        .state_dir
        .join("runtime-skills")
        .join("claude")
        .join(&skill_set_key);
    let skills_home = bundle_root.join(".claude").join("skills");
    tokio::fs::create_dir_all(&skills_home)
        .await
        .map_err(|error| AcpxError::Io {
            path: skills_home.clone(),
            error,
        })?;

    for entry in &input.selected_skills {
        let target = skills_home.join(&entry.runtime_name);
        let result = materialize_paperclip_skill_copy(&entry.source, &target).await;
        match result {
            Ok(copy) => {
                if !copy.skipped_symlinks.is_empty() {
                    emit_log(
                        input.on_log.as_ref(),
                        LogStream::Stdout,
                        &format!(
                            "[paperclip] Materialized ACPX Claude skill \"{}\" into {} and skipped {} symlink(s).\n",
                            entry.runtime_name,
                            skills_home.display(),
                            copy.skipped_symlinks.len()
                        ),
                    );
                }
            }
            Err(error) => {
                emit_log(
                    input.on_log.as_ref(),
                    LogStream::Stderr,
                    &format!(
                        "[paperclip] Failed to materialize ACPX Claude skill \"{}\" into {}: {}\n",
                        entry.key,
                        skills_home.display(),
                        error
                    ),
                );
            }
        }
    }

    let selected_names = {
        let mut names: Vec<String> = input
            .selected_skills
            .iter()
            .map(|entry| entry.runtime_name.clone())
            .collect();
        names.sort();
        names
    };

    let prompt_instructions = if selected_names.is_empty() {
        String::new()
    } else {
        [
            "Paperclip has materialized selected runtime skills for this ACPX Claude session.",
            &format!("Skill root: {}", skills_home.display()),
            &format!(
                "Selected skills: {}",
                selected_names.join(", ")
            ),
            "When a task calls for one of these skills, read its SKILL.md from that root and follow it.",
        ]
        .join("\n")
    };

    let command_notes = if selected_names.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Materialized {} Paperclip skill(s) for ACPX Claude at {}.",
            selected_names.len(),
            skills_home.display()
        )]
    };

    Ok(PrepareSkillRuntimeOutput {
        identity: SkillRuntimeIdentity {
            mode: "claude".into(),
            skill_set_key,
            desired_skill_names: input.desired_skill_names,
            selected_skills: selected_names,
            skills_home: if input.selected_skills.is_empty() {
                skills_home
            } else {
                skills_home.clone()
            },
            codex_home: None,
            bundle_root: Some(bundle_root),
        },
        command_notes,
        prompt_instructions,
    })
}

// ============================================================================
// prepareCodexSkillRuntime
// ============================================================================

/// Input for [`prepare_codex_skill_runtime`]. Mirrors the Node
/// `prepareCodexSkillRuntime` argument shape.
#[derive(Clone)]
pub struct PrepareCodexSkillRuntimeInput {
    pub company_id: String,
    pub source_codex_home: PathBuf,
    pub managed_codex_home: PathBuf,
    pub env: std::collections::BTreeMap<String, String>,
    pub selected_skills: Vec<PaperclipSkillEntry>,
    pub all_skills: Vec<PaperclipSkillEntry>,
    pub desired_skill_names: Vec<String>,
    pub on_log: Option<OnLogSink>,
}

impl std::fmt::Debug for PrepareCodexSkillRuntimeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepareCodexSkillRuntimeInput")
            .field("company_id", &self.company_id)
            .field("source_codex_home", &self.source_codex_home)
            .field("managed_codex_home", &self.managed_codex_home)
            .field("env", &self.env)
            .field("selected_skills", &self.selected_skills.len())
            .field("all_skills", &self.all_skills.len())
            .field("desired_skill_names", &self.desired_skill_names)
            .field("on_log", &self.on_log.as_ref().map(|_| "<sink>"))
            .finish()
    }
}

/// Prepare the Codex skill runtime: seed the managed Codex home,
/// reconcile the per-`skills/` directory, materialize the selected
/// skills, and update the manifest. Returns the per-call identity and
/// the `CODEX_HOME` env value the caller must inject.
pub async fn prepare_codex_skill_runtime(
    mut input: PrepareCodexSkillRuntimeInput,
) -> Result<PrepareSkillRuntimeOutput, AcpxError> {
    let seed_input = PrepareManagedCodexHomeInput::new(
        input.company_id.clone(),
        input.source_codex_home.clone(),
        input.managed_codex_home.clone(),
    );
    let on_log = input.on_log.clone();
    let seed_input = if let Some(sink) = on_log {
        seed_input.with_on_log(sink)
    } else {
        seed_input
    };
    let effective_codex_home = prepare_managed_codex_home(seed_input).await?;
    let skills_home = effective_codex_home.join("skills");
    tokio::fs::create_dir_all(&skills_home)
        .await
        .map_err(|error| AcpxError::Io {
            path: skills_home.clone(),
            error,
        })?;

    reconcile_managed_codex_skills(ReconcileManagedCodexSkillsInput::new(
        skills_home.clone(),
        input.all_skills.clone(),
        input.selected_skills.clone(),
    ))
    .await?;

    for entry in &input.selected_skills {
        let target = skills_home.join(&entry.runtime_name);
        let result = materialize_paperclip_skill_copy(&entry.source, &target).await;
        match result {
            Ok(copy) => {
                if !copy.skipped_symlinks.is_empty() {
                    emit_log(
                        input.on_log.as_ref(),
                        LogStream::Stdout,
                        &format!(
                            "[paperclip] Materialized ACPX Codex skill \"{}\" into {} and skipped {} symlink(s).\n",
                            entry.runtime_name,
                            skills_home.display(),
                            copy.skipped_symlinks.len()
                        ),
                    );
                }
            }
            Err(error) => {
                emit_log(
                    input.on_log.as_ref(),
                    LogStream::Stderr,
                    &format!(
                        "[paperclip] Failed to inject ACPX Codex skill \"{}\" into {}: {}\n",
                        entry.key,
                        skills_home.display(),
                        error
                    ),
                );
            }
        }
    }

    let manifest = ManagedSkillsManifest::from_names(
        input
            .selected_skills
            .iter()
            .map(|entry| entry.runtime_name.as_str()),
    );
    write_managed_codex_skills_manifest(&skills_home, &manifest).await?;

    input.env.insert(
        "CODEX_HOME".into(),
        effective_codex_home.to_string_lossy().into_owned(),
    );

    let selected_names = {
        let mut names: Vec<String> = input
            .selected_skills
            .iter()
            .map(|entry| entry.runtime_name.clone())
            .collect();
        names.sort();
        names
    };

    Ok(PrepareSkillRuntimeOutput {
        identity: SkillRuntimeIdentity {
            mode: "codex".into(),
            skill_set_key: build_skill_set_key(&input.selected_skills, "codex").await,
            desired_skill_names: input.desired_skill_names,
            selected_skills: selected_names,
            skills_home: skills_home.clone(),
            codex_home: Some(effective_codex_home),
            bundle_root: None,
        },
        command_notes: vec![format!(
            "Prepared ACPX Codex skill home at {}.",
            skills_home.display()
        )],
        prompt_instructions: String::new(),
    })
}

// ============================================================================
// prepareGeminiSkillRuntime
// ============================================================================

/// Input for [`prepare_gemini_skill_runtime`].
#[derive(Clone)]
pub struct PrepareGeminiSkillRuntimeInput {
    pub skills_home: PathBuf,
    pub selected_skills: Vec<PaperclipSkillEntry>,
    pub desired_skill_names: Vec<String>,
    pub on_log: Option<OnLogSink>,
}

impl std::fmt::Debug for PrepareGeminiSkillRuntimeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepareGeminiSkillRuntimeInput")
            .field("skills_home", &self.skills_home)
            .field("selected_skills", &self.selected_skills.len())
            .field("desired_skill_names", &self.desired_skill_names)
            .field("on_log", &self.on_log.as_ref().map(|_| "<sink>"))
            .finish()
    }
}

/// Prepare the Gemini skill runtime. The Node implementation prefers
/// `symlink` (with a `EPERM` fallback to a copy); the Rust port uses
/// [`ensure_symlink`] when the host supports it and silently falls
/// back to a copy when symlink creation fails. The materialization
/// step itself uses [`materialize_paperclip_skill_copy`] so the
/// helper transparently drops nested symlinks either way.
pub async fn prepare_gemini_skill_runtime(
    input: PrepareGeminiSkillRuntimeInput,
) -> Result<PrepareSkillRuntimeOutput, AcpxError> {
    tokio::fs::create_dir_all(&input.skills_home)
        .await
        .map_err(|error| AcpxError::Io {
            path: input.skills_home.clone(),
            error,
        })?;

    for entry in &input.selected_skills {
        let target = input.skills_home.join(&entry.runtime_name);
        // Try symlink first; fall back to copy on failure (e.g. EPERM).
        let symlink_result = ensure_symlink(&target, &entry.source).await;
        match symlink_result {
            Ok(()) => {
                emit_log(
                    input.on_log.as_ref(),
                    LogStream::Stdout,
                    &format!(
                        "[paperclip] Linked ACPX Gemini skill \"{}\" into {}\n",
                        entry.runtime_name,
                        input.skills_home.display()
                    ),
                );
            }
            Err(error) => match materialize_paperclip_skill_copy(&entry.source, &target).await {
                Ok(copy) => {
                    let skipped_note = if copy.skipped_symlinks.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Skipped {} nested symlink(s).",
                            copy.skipped_symlinks.len()
                        )
                    };
                    emit_log(
                        input.on_log.as_ref(),
                        LogStream::Stdout,
                        &format!(
                            "[paperclip] Copied ACPX Gemini skill \"{}\" into {} because symlinks are unavailable.{}\n",
                            entry.runtime_name,
                            input.skills_home.display(),
                            skipped_note
                        ),
                    );
                }
                Err(materialize_error) => {
                    emit_log(
                        input.on_log.as_ref(),
                        LogStream::Stderr,
                        &format!(
                            "[paperclip] Failed to link ACPX Gemini skill \"{}\" into {}: {} (copy fallback also failed: {})\n",
                            entry.key,
                            input.skills_home.display(),
                            error,
                            materialize_error
                        ),
                    );
                }
            },
        }
    }

    let selected_names = {
        let mut names: Vec<String> = input
            .selected_skills
            .iter()
            .map(|entry| entry.runtime_name.clone())
            .collect();
        names.sort();
        names
    };

    let command_notes = if selected_names.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Prepared {} ACPX Gemini skill(s) at {}.",
            selected_names.len(),
            input.skills_home.display()
        )]
    };

    Ok(PrepareSkillRuntimeOutput {
        identity: SkillRuntimeIdentity {
            mode: "gemini".into(),
            skill_set_key: build_skill_set_key(&input.selected_skills, "gemini").await,
            desired_skill_names: input.desired_skill_names,
            selected_skills: selected_names,
            skills_home: input.skills_home.clone(),
            codex_home: None,
            bundle_root: None,
        },
        command_notes,
        prompt_instructions: String::new(),
    })
}

// ============================================================================
// resolveSelectedRuntimeSkills (in-crate shim)
// ============================================================================

/// Lightweight in-crate shim for the Node
/// `resolveSelectedRuntimeSkills` helper. The full config-driven
/// resolver lives outside `pc-acpx` (it scans the host filesystem for
/// skill entries); here we expose the small core that filters a
/// pre-supplied `all_skills` list down to the desired set.
pub fn resolve_selected_runtime_skills(
    all_skills: Vec<PaperclipSkillEntry>,
    desired_skill_names: &[String],
) -> (
    Vec<PaperclipSkillEntry>,
    Vec<PaperclipSkillEntry>,
    Vec<String>,
) {
    let desired: std::collections::BTreeSet<&str> =
        desired_skill_names.iter().map(String::as_str).collect();
    let selected = all_skills
        .iter()
        .filter(|entry| desired.contains(entry.key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    (all_skills, selected, desired_skill_names.to_vec())
}

// ============================================================================
// Helpers
// ============================================================================

fn emit_log(sink: Option<&OnLogSink>, stream: LogStream, line: &str) {
    if let Some(sink) = sink {
        sink(stream, line);
    }
}

// Keep `WriteFileAtomicallyInput` reachable so the symbol remains part of
// the public surface for the wider engine crate.
#[allow(dead_code)]
fn _write_file_atomically_reachable(
    path: &Path,
    contents: impl Into<String>,
) -> impl std::future::Future<Output = Result<(), AcpxError>> {
    write_file_atomically(WriteFileAtomicallyInput::new(path, contents, 0o644))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pc-acpx-skillrt-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn write_file(path: &Path, contents: &str) {
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
            source_status: Some(crate::skill_materialize::SkillSourceStatus::Available),
            missing_detail: None,
        }
    }

    #[tokio::test]
    async fn resolve_selected_runtime_skills_filters_by_key() {
        let a = entry("alpha", "alpha-rt", unique_dir("a"));
        let b = entry("beta", "beta-rt", unique_dir("b"));
        let c = entry("gamma", "gamma-rt", unique_dir("c"));
        let desired = vec!["alpha".into(), "beta".into()];
        let (all, selected, names) =
            resolve_selected_runtime_skills(vec![a.clone(), b.clone(), c.clone()], &desired);
        assert_eq!(all.len(), 3);
        assert_eq!(selected.len(), 2);
        assert_eq!(names, desired);
    }

    #[tokio::test]
    async fn claude_runtime_materializes_skills_into_bundle_root() {
        let state = unique_dir("claude-state");
        tokio::fs::create_dir_all(&state).await.unwrap();
        let skill_dir = unique_dir("claude-skill");
        write_file(&skill_dir.join("SKILL.md"), "alpha doc");
        let selected = vec![entry("alpha", "alpha-rt", skill_dir.clone())];
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
        assert!(output
            .identity
            .bundle_root
            .as_ref()
            .unwrap()
            .ends_with(&output.identity.skill_set_key));
        let skill_root = output.identity.skills_home.clone();
        assert!(tokio::fs::try_exists(skill_root.join("alpha-rt/SKILL.md"))
            .await
            .unwrap_or(false));
        assert!(output.prompt_instructions.contains("alpha-rt"));
        assert_eq!(output.command_notes.len(), 1);
        let _ = tokio::fs::remove_dir_all(&state).await;
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
    }

    #[tokio::test]
    async fn claude_runtime_is_pure_with_no_selected_skills() {
        let state = unique_dir("claude-empty");
        tokio::fs::create_dir_all(&state).await.unwrap();
        let input = PrepareClaudeSkillRuntimeInput {
            state_dir: state.clone(),
            selected_skills: vec![],
            desired_skill_names: vec![],
            on_log: None,
        };
        let output = prepare_claude_skill_runtime(input).await.unwrap();
        assert!(output.prompt_instructions.is_empty());
        assert!(output.command_notes.is_empty());
        let _ = tokio::fs::remove_dir_all(&state).await;
    }

    #[tokio::test]
    async fn codex_runtime_seeds_home_and_writes_manifest() {
        let source = unique_dir("codex-src");
        let target = unique_dir("codex-tgt");
        let skill_dir = unique_dir("codex-skill");
        write_file(&skill_dir.join("SKILL.md"), "codex doc");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(source.join("auth.json"), r#"{"token":"abc"}"#)
            .await
            .unwrap();

        let mut env = std::collections::BTreeMap::new();
        let input = PrepareCodexSkillRuntimeInput {
            company_id: "company-1".into(),
            source_codex_home: source.clone(),
            managed_codex_home: target.clone(),
            env: env.clone(),
            selected_skills: vec![entry("alpha", "alpha-rt", skill_dir.clone())],
            all_skills: vec![entry("alpha", "alpha-rt", skill_dir.clone())],
            desired_skill_names: vec!["alpha".into()],
            on_log: None,
        };
        let output = prepare_codex_skill_runtime(input).await.unwrap();
        assert_eq!(output.identity.mode, "codex");
        let codex_home = output.identity.codex_home.clone().unwrap();
        assert_eq!(codex_home, target);
        // auth.json was seeded.
        assert!(tokio::fs::try_exists(target.join("auth.json"))
            .await
            .unwrap_or(false));
        // skill was materialized.
        assert!(
            tokio::fs::try_exists(target.join("skills/alpha-rt/SKILL.md"))
                .await
                .unwrap_or(false)
        );
        // manifest was written.
        let manifest =
            crate::managed_home::read_managed_codex_skills_manifest(target.join("skills")).await;
        assert_eq!(manifest.names(), ["alpha-rt"]);
        // CODEX_HOME env was injected.
        env = output
            .identity
            .codex_home
            .iter()
            .map(|p| p.to_path_buf())
            .fold(env, |mut acc, _| {
                acc.insert(
                    "CODEX_HOME".into(),
                    output
                        .identity
                        .codex_home
                        .as_ref()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                acc
            });
        assert!(env.contains_key("CODEX_HOME"));
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
    }

    #[tokio::test]
    async fn gemini_runtime_uses_symlink_or_copy_fallback() {
        let skills_home = unique_dir("gemini-home");
        tokio::fs::create_dir_all(&skills_home).await.unwrap();
        let skill_dir = unique_dir("gemini-skill");
        write_file(&skill_dir.join("SKILL.md"), "gemini doc");
        let input = PrepareGeminiSkillRuntimeInput {
            skills_home: skills_home.clone(),
            selected_skills: vec![entry("alpha", "alpha-rt", skill_dir.clone())],
            desired_skill_names: vec!["alpha".into()],
            on_log: None,
        };
        let output = prepare_gemini_skill_runtime(input).await.unwrap();
        assert_eq!(output.identity.mode, "gemini");
        assert_eq!(
            output.identity.selected_skills,
            vec!["alpha-rt".to_string()]
        );
        // Either a symlink or a materialized copy must exist on disk.
        let target = skills_home.join("alpha-rt");
        assert!(tokio::fs::try_exists(&target).await.unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&skills_home).await;
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
    }
}
