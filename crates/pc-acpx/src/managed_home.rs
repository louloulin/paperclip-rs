//! `pc-acpx` managed Codex home — port of `prepareManagedCodexHome` and the
//! `.paperclip-managed-skills.json` manifest helpers from Node
//! `acpx-engine/execute.ts`.
//!
//! The "managed Codex home" is a per-company copy of the user-level
//! `~/.codex` directory that the engine seeds on every adapter run. The
//! manifest file tracks which skills the engine itself dropped into
//! `skills/`, so subsequent runs can revoke stale entries without
//! touching skills the user added out-of-band.
//!
//! The home seeding is a pure filesystem pipeline; no JSON-RPC and no
//! subprocess. Callers plug in an [`OnLogSink`] to forward log lines
//! through their own observability bridge.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AcpxError;
use crate::fs_ops::{ensure_copied_file, ensure_parent_dir, ensure_symlink, path_exists};

// ============================================================================
// Manifest constant
// ============================================================================

/// File name of the per-`skills/` manifest. Matches the Node
/// `PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST` constant.
pub const PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST: &str = ".paperclip-managed-skills.json";

// ============================================================================
// Log sink
// ============================================================================

/// Log stream tags. Mirrors the Node `AdapterExecutionContext["onLog"]`
/// first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// Callback signature for forwarding engine log lines. Mirrors the Node
/// `onLog(stream, message)` shape. Implementations may be sync or async
/// (the sync wrapper is used here to keep the seeding pipeline simple).
pub type OnLogSink = std::sync::Arc<dyn Fn(LogStream, &str) + Send + Sync>;

// ============================================================================
// Manifest data type
// ============================================================================

/// On-disk shape of the managed-skills manifest. The `version` field is
/// reserved for future schema changes; the current shape is
/// `{ "version": 1, "managedSkillNames": [...] }`. The on-disk
/// `managedSkillNames` (camelCase) matches the Node source; the Rust
/// field is the conventional snake_case form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillsManifest {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(rename = "managedSkillNames", default)]
    pub managed_skill_names: Vec<String>,
}

fn default_manifest_version() -> u32 {
    1
}

impl ManagedSkillsManifest {
    /// Build an empty manifest with the default `version`.
    pub fn empty() -> Self {
        Self {
            version: 1,
            managed_skill_names: Vec::new(),
        }
    }

    /// Build a manifest from a set of skill runtime names. The names are
    /// sorted deterministically so the on-disk JSON diff is stable.
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut sorted: BTreeSet<String> = BTreeSet::new();
        for name in names {
            let trimmed = name.as_ref().trim();
            if !trimmed.is_empty() {
                sorted.insert(trimmed.to_string());
            }
        }
        Self {
            version: 1,
            managed_skill_names: sorted.into_iter().collect(),
        }
    }

    /// Return the manifest's managed skill names as an ordered `Vec`.
    pub fn names(&self) -> &[String] {
        &self.managed_skill_names
    }

    /// Build an in-memory `BTreeSet` view of the managed names. Useful
    /// for set arithmetic during reconciliation.
    pub fn name_set(&self) -> BTreeSet<String> {
        self.managed_skill_names.iter().cloned().collect()
    }
}

// ============================================================================
// prepareManagedCodexHome
// ============================================================================

/// Input for [`prepare_managed_codex_home`].
#[derive(Clone)]
pub struct PrepareManagedCodexHomeInput {
    pub company_id: String,
    pub source_home: PathBuf,
    pub target_home: PathBuf,
    pub on_log: Option<OnLogSink>,
}

impl std::fmt::Debug for PrepareManagedCodexHomeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepareManagedCodexHomeInput")
            .field("company_id", &self.company_id)
            .field("source_home", &self.source_home)
            .field("target_home", &self.target_home)
            .field("on_log", &self.on_log.as_ref().map(|_| "<sink>"))
            .finish()
    }
}

impl PrepareManagedCodexHomeInput {
    pub fn new(
        company_id: impl Into<String>,
        source_home: impl Into<PathBuf>,
        target_home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            company_id: company_id.into(),
            source_home: source_home.into(),
            target_home: target_home.into(),
            on_log: None,
        }
    }

    pub fn with_on_log(mut self, on_log: OnLogSink) -> Self {
        self.on_log = Some(on_log);
        self
    }
}

/// Prepare the managed Codex home directory for `company_id`. Returns
/// the resolved target home path.
///
/// Pipeline (mirrors Node):
/// 1. If `source_home == target_home` (resolved), no-op and return.
/// 2. `mkdir -p target_home`.
/// 3. Symlink `auth.json` from `source_home` into `target_home` when present.
/// 4. Copy `config.json` / `config.toml` / `instructions.md` (each file
///    is skipped individually when the source is missing).
/// 5. Emit a single `onLog` line announcing the seeded home.
pub async fn prepare_managed_codex_home(
    input: PrepareManagedCodexHomeInput,
) -> Result<PathBuf, AcpxError> {
    let source_resolved = normalize_path(&input.source_home);
    let target_resolved = normalize_path(&input.target_home);

    if source_resolved == target_resolved {
        return Ok(target_resolved);
    }

    tokio::fs::create_dir_all(&target_resolved)
        .await
        .map_err(|error| AcpxError::Io {
            path: target_resolved.clone(),
            error,
        })?;

    // auth.json — symlink (so credentials stay canonical).
    let source_auth = input.source_home.join("auth.json");
    let target_auth = input.target_home.join("auth.json");
    if path_exists(&source_auth).await {
        ensure_symlink(&target_auth, &source_auth).await?;
    }

    // config.{json,toml} and instructions.md — copy.
    for name in ["config.json", "config.toml", "instructions.md"] {
        let source = input.source_home.join(name);
        if !path_exists(&source).await {
            continue;
        }
        let target = input.target_home.join(name);
        ensure_copied_file(&target, &source).await?;
    }

    if let Some(on_log) = input.on_log.as_ref() {
        let line = format!(
            "[paperclip] Using Paperclip-managed ACPX Codex home {:?} (seeded from {:?}).\n",
            target_resolved, source_resolved
        );
        on_log(LogStream::Stdout, &line);
    }

    Ok(target_resolved)
}

/// `Path::canonicalize` for paths that may not exist. Falls back to the
/// input when canonicalize fails (matches Node `path.resolve` which
/// does not require existence).
fn normalize_path(candidate: &Path) -> PathBuf {
    candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf())
}

// ============================================================================
// Manifest IO
// ============================================================================

/// Read the `.paperclip-managed-skills.json` manifest from `skills_home`.
/// Returns an empty manifest when the file is absent or unreadable.
/// Mirrors the Node `readManagedCodexSkillsManifest` helper: any
/// read/parse failure is silently swallowed into an empty manifest.
pub async fn read_managed_codex_skills_manifest(
    skills_home: impl AsRef<Path>,
) -> ManagedSkillsManifest {
    let manifest_path = skills_home
        .as_ref()
        .join(PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST);
    let raw = match tokio::fs::read_to_string(&manifest_path).await {
        Ok(raw) => raw,
        Err(_) => return ManagedSkillsManifest::empty(),
    };
    match serde_json::from_str::<ManagedSkillsManifest>(&raw) {
        Ok(parsed) => parsed,
        Err(_) => ManagedSkillsManifest::empty(),
    }
}

/// Serialize `manifest` as JSON and write it to
/// `<skills_home>/.paperclip-managed-skills.json`. The skills home is
/// created if missing.
pub async fn write_managed_codex_skills_manifest(
    skills_home: impl AsRef<Path>,
    manifest: &ManagedSkillsManifest,
) -> Result<(), AcpxError> {
    let skills_home = skills_home.as_ref();
    ensure_parent_dir(skills_home.join(PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST)).await?;
    tokio::fs::create_dir_all(skills_home)
        .await
        .map_err(|error| AcpxError::Io {
            path: skills_home.to_path_buf(),
            error,
        })?;
    let serialized = serde_json::to_string_pretty(manifest).map_err(|error| AcpxError::Io {
        path: skills_home.join(PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST),
        error: std::io::Error::other(error.to_string()),
    })?;
    let path = skills_home.join(PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST);
    tokio::fs::write(&path, format!("{serialized}\n"))
        .await
        .map_err(|error| AcpxError::Io { path, error })?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn unique_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pc-acpx-managed-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[tokio::test]
    async fn manifest_round_trips_through_disk() {
        let dir = unique_dir("roundtrip");
        let manifest = ManagedSkillsManifest::from_names(["alpha", "beta", "gamma"]);
        write_managed_codex_skills_manifest(&dir, &manifest)
            .await
            .unwrap();
        let read = read_managed_codex_skills_manifest(&dir).await;
        assert_eq!(read, manifest);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn manifest_read_returns_empty_when_missing() {
        let dir = unique_dir("missing");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let read = read_managed_codex_skills_manifest(&dir).await;
        assert_eq!(read, ManagedSkillsManifest::empty());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn manifest_read_falls_back_on_corrupt_json() {
        let dir = unique_dir("corrupt");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join(PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST),
            "not valid json",
        )
        .await
        .unwrap();
        let read = read_managed_codex_skills_manifest(&dir).await;
        assert_eq!(read, ManagedSkillsManifest::empty());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn manifest_from_names_sorts_and_dedupes() {
        let manifest = ManagedSkillsManifest::from_names(["zeta", "alpha", "alpha", ""]);
        assert_eq!(manifest.names(), ["alpha", "zeta"]);
        assert_eq!(manifest.name_set().len(), 2);
    }

    #[tokio::test]
    async fn prepare_managed_codex_home_is_noop_when_paths_match() {
        let dir = unique_dir("noop");
        let input = PrepareManagedCodexHomeInput::new("company-1", dir.clone(), dir.clone());
        let result = prepare_managed_codex_home(input).await.unwrap();
        assert_eq!(result, dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn prepare_managed_codex_home_creates_target_and_copies_files() {
        let source = unique_dir("seed-src");
        let target = unique_dir("seed-tgt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(source.join("auth.json"), r#"{"token":"abc"}"#)
            .await
            .unwrap();
        tokio::fs::write(source.join("config.json"), r#"{"model":"x"}"#)
            .await
            .unwrap();
        tokio::fs::write(source.join("config.toml"), "model = \"y\"\n")
            .await
            .unwrap();
        tokio::fs::write(source.join("instructions.md"), "do the thing")
            .await
            .unwrap();
        tokio::fs::write(source.join("ignored.bin"), "skip me")
            .await
            .unwrap();

        let input = PrepareManagedCodexHomeInput::new("company-1", source.clone(), target.clone());
        let resolved = prepare_managed_codex_home(input).await.unwrap();

        // The resolved target must exist and contain the four expected
        // files. `ignored.bin` must NOT be copied.
        assert!(path_exists(&resolved).await);
        assert!(path_exists(resolved.join("auth.json")).await);
        assert!(path_exists(resolved.join("config.json")).await);
        assert!(path_exists(resolved.join("config.toml")).await);
        assert!(path_exists(resolved.join("instructions.md")).await);
        assert!(!path_exists(resolved.join("ignored.bin")).await);

        let config_json = tokio::fs::read_to_string(resolved.join("config.json"))
            .await
            .unwrap();
        assert_eq!(config_json, r#"{"model":"x"}"#);
        #[cfg(unix)]
        {
            // auth.json must be a symlink, not a copy.
            let meta = tokio::fs::symlink_metadata(resolved.join("auth.json"))
                .await
                .unwrap();
            assert!(
                meta.file_type().is_symlink(),
                "auth.json should be a symlink"
            );
        }

        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn prepare_managed_codex_home_emits_log_line() {
        let source = unique_dir("log-src");
        let target = unique_dir("log-tgt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        let captured: Arc<Mutex<Vec<(LogStream, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_closure = captured.clone();
        let sink: OnLogSink = Arc::new(move |stream, line| {
            captured_for_closure
                .lock()
                .unwrap()
                .push((stream, line.to_string()));
        });
        let input = PrepareManagedCodexHomeInput::new("company-2", source.clone(), target.clone())
            .with_on_log(sink);
        prepare_managed_codex_home(input).await.unwrap();
        let log = captured.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, LogStream::Stdout);
        assert!(log[0].1.contains("Using Paperclip-managed"));
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn prepare_managed_codex_home_skips_missing_config_files() {
        let source = unique_dir("partial-src");
        let target = unique_dir("partial-tgt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(source.join("config.json"), "{}")
            .await
            .unwrap();
        // config.toml + instructions.md + auth.json are all missing.

        let input = PrepareManagedCodexHomeInput::new("company-3", source.clone(), target.clone());
        let resolved = prepare_managed_codex_home(input).await.unwrap();

        assert!(path_exists(resolved.join("config.json")).await);
        assert!(!path_exists(resolved.join("config.toml")).await);
        assert!(!path_exists(resolved.join("instructions.md")).await);
        assert!(!path_exists(resolved.join("auth.json")).await);

        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[test]
    fn manifest_default_version_is_one() {
        let json = r#"{"managedSkillNames":["a"]}"#;
        let parsed: ManagedSkillsManifest = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.names(), ["a"]);
    }
}
