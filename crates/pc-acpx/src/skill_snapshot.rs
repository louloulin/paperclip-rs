//! `pc-acpx` skill-snapshot builders — pure helpers that produce an
//! `AdapterSkillSnapshot` from a list of available Paperclip skill
//! entries plus the caller's desired-skills list and installation state.
//!
//! Rust port of Node `packages/adapter-utils/src/server-utils.ts`:
//! - `skillLocationLabel` (L294-298, internal)
//! - `buildManagedSkillOrigin` (L300-309, internal)
//! - `isPaperclipSkillSourceMissing` (L311-313, internal)
//! - `resolvePaperclipSkillMissingDetail` (L315-320, internal)
//! - `resolveSkillDetail` (L322-330, internal)
//! - `resolveInstalledEntryTarget` (L331-346, internal — only used by
//!   the async I/O helper `readInstalledSkillTargets`, which lives
//!   outside this module)
//! - `buildRuntimeMountedSkillSnapshot` (L2491-2608)
//! - `buildPersistentSkillSnapshot` (L2609-2734)
//!
//! All helpers are pure: no I/O, no async, no global state. The two
//! snapshot builders return [`AdapterSkillSnapshot`]; callers can use
//! them inside adapters (claude-local, codex-local, gemini-local,
//! grok-local, …) without coupling to any filesystem layer.
//!
//! `AdapterSkillEntry.detail` accepts either a plain string or a closure
//! `(entry) -> Option<String>`. The closure variant mirrors Node's
//! `(entry: PaperclipSkillEntry) => string | null` callback shape, so
//! callers can produce per-entry detail text without paying the cost
//! of formatting every detail for every entry.

use std::collections::{BTreeMap, BTreeSet};

// ============================================================================
// Enums
// ============================================================================

/// Where a Paperclip-managed skill source lives. Mirrors Node
/// `AdapterSkillOrigin` (L246-249).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterSkillOrigin {
    /// Skill ships from the company-managed Paperclip skill registry.
    CompanyManaged,
    /// Skill was installed outside Paperclip management (user action).
    UserInstalled,
    /// Skill was desired but cannot be located anywhere — surfaced
    /// so callers can warn the operator.
    ExternalUnknown,
}

impl AdapterSkillOrigin {
    /// Human-readable label that callers typically render in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::CompanyManaged => "Managed by Paperclip",
            Self::UserInstalled => "User-installed",
            Self::ExternalUnknown => "External or unavailable",
        }
    }
}

/// State of a single skill relative to the runtime. Mirrors Node
/// `AdapterSkillState` (L238-244).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterSkillState {
    /// Skill is available from Paperclip but not desired by the
    /// current call.
    Available,
    /// Skill is desired and the adapter has staged it for use
    /// (ephemeral mode).
    Configured,
    /// Skill has been symlinked / installed into the runtime skills
    /// directory (persistent mode).
    Installed,
    /// Skill source is missing from the registry.
    Missing,
    /// Skill is installed but no longer desired by the runtime
    /// (persistent mode).
    Stale,
    /// Skill lives outside Paperclip management (user-installed).
    External,
}

/// Skill sync mode for an adapter. Mirrors Node `AdapterSkillSyncMode`
/// (L236).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterSkillSyncMode {
    /// Adapter cannot apply skills at runtime.
    Unsupported,
    /// Adapter symlinks / installs skills into a managed directory.
    Persistent,
    /// Adapter stages skills ephemerally per call.
    Ephemeral,
}

/// How an installed skill is materialised on disk. Mirrors Node
/// `InstalledSkillTarget.kind` (L246-249).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstalledSkillTargetKind {
    Symlink,
    Directory,
    File,
}

// ============================================================================
// Source / entry structs
// ============================================================================

/// A skill entry sourced from the Paperclip-managed skill registry.
/// Mirrors Node `PaperclipSkillEntry` (L231-238).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperclipSkillEntry {
    pub key: String,
    pub runtime_name: String,
    pub source: String,
    pub version_id: Option<String>,
    pub current_version_id: Option<String>,
    pub source_status: PaperclipSkillSourceStatus,
    pub missing_detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaperclipSkillSourceStatus {
    Available,
    Missing,
}

impl Default for PaperclipSkillSourceStatus {
    fn default() -> Self {
        Self::Available
    }
}

/// Resolved install target for a single skill name. Mirrors Node
/// `InstalledSkillTarget` (L246-249).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSkillTarget {
    pub target_path: Option<String>,
    pub kind: InstalledSkillTargetKind,
}

// ============================================================================
// Output entry / snapshot
// ============================================================================

/// Single skill entry in an [`AdapterSkillSnapshot`]. Mirrors Node
/// `AdapterSkillEntry` (L251-264). Optional fields are typed as
/// `Option<T>` to faithfully reflect the Node interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSkillEntry {
    pub key: String,
    pub runtime_name: Option<String>,
    pub version_id: Option<String>,
    pub current_version_id: Option<String>,
    pub desired: bool,
    pub managed: bool,
    pub state: AdapterSkillState,
    pub origin: Option<AdapterSkillOrigin>,
    pub origin_label: Option<String>,
    pub location_label: Option<String>,
    pub read_only: bool,
    pub source_path: Option<String>,
    pub target_path: Option<String>,
    pub detail: Option<String>,
}

impl AdapterSkillEntry {
    fn managed(key: String, runtime_name: Option<String>, state: AdapterSkillState) -> Self {
        Self {
            key,
            runtime_name,
            version_id: None,
            current_version_id: None,
            desired: false,
            managed: true,
            state,
            origin: Some(AdapterSkillOrigin::CompanyManaged),
            origin_label: Some(AdapterSkillOrigin::CompanyManaged.label().to_string()),
            location_label: None,
            read_only: false,
            source_path: None,
            target_path: None,
            detail: None,
        }
    }
}

/// Full skill snapshot returned by the runtime/persistent builders.
/// Mirrors Node `AdapterSkillSnapshot` (L268-275).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSkillSnapshot {
    pub adapter_type: String,
    pub supported: bool,
    pub mode: AdapterSkillSyncMode,
    pub desired_skills: Vec<String>,
    pub desired_skill_entries: Vec<AdapterDesiredSkillEntry>,
    pub entries: Vec<AdapterSkillEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterDesiredSkillEntry {
    pub key: String,
    pub version_id: Option<String>,
}

// ============================================================================
// Detail (string | closure)
// ============================================================================

/// Per-entry detail override. Mirrors Node
/// `string | ((entry) => string | null) | null | undefined`. The
/// closure variant lets callers compute detail from the source entry
/// without paying for strings they will not emit.
///
/// `Debug` is intentionally hand-rolled because the closure variant
/// holds a `dyn Fn` which does not implement `Debug`.
#[derive(Clone)]
pub enum SkillDetail {
    None,
    Static(String),
    Dynamic(std::sync::Arc<dyn Fn(&PaperclipSkillEntry) -> Option<String> + Send + Sync>),
}

impl std::fmt::Debug for SkillDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("SkillDetail::None"),
            Self::Static(s) => f.debug_tuple("SkillDetail::Static").field(s).finish(),
            Self::Dynamic(_) => f.write_str("SkillDetail::Dynamic(<fn>)"),
        }
    }
}

impl Default for SkillDetail {
    fn default() -> Self {
        Self::None
    }
}

impl From<&'static str> for SkillDetail {
    fn from(value: &'static str) -> Self {
        Self::Static(value.to_string())
    }
}

impl From<String> for SkillDetail {
    fn from(value: String) -> Self {
        Self::Static(value)
    }
}

impl From<Option<String>> for SkillDetail {
    fn from(value: Option<String>) -> Self {
        match value {
            Some(s) => Self::Static(s),
            None => Self::None,
        }
    }
}

// ============================================================================
// Internal helpers (mirrored from Node L286-345)
// ============================================================================

/// Trim and normalise a user-supplied location label. Mirrors Node
/// `skillLocationLabel` (L294-298): `null` for empty / non-string input,
/// trimmed string otherwise.
pub fn skill_location_label(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Origin triplet used by every managed Paperclip skill. Mirrors Node
/// `buildManagedSkillOrigin` (L300-309).
pub fn build_managed_skill_origin() -> (AdapterSkillOrigin, String, bool) {
    (
        AdapterSkillOrigin::CompanyManaged,
        AdapterSkillOrigin::CompanyManaged.label().to_string(),
        false,
    )
}

/// Test for missing source. Mirrors Node `isPaperclipSkillSourceMissing`
/// (L311-313).
pub fn is_paperclip_skill_source_missing(entry: &PaperclipSkillEntry) -> bool {
    matches!(entry.source_status, PaperclipSkillSourceStatus::Missing)
}

/// Pick the most specific missing detail. Mirrors Node
/// `resolvePaperclipSkillMissingDetail` (L315-320).
pub fn resolve_paperclip_skill_missing_detail(
    entry: &PaperclipSkillEntry,
    fallback: &str,
) -> String {
    match entry.missing_detail.as_deref().map(str::trim) {
        Some(detail) if !detail.is_empty() => detail.to_string(),
        _ => fallback.to_string(),
    }
}

/// Resolve a [`SkillDetail`] against a source entry. Mirrors Node
/// `resolveSkillDetail` (L322-330).
pub fn resolve_skill_detail(detail: &SkillDetail, entry: &PaperclipSkillEntry) -> Option<String> {
    match detail {
        SkillDetail::None => None,
        SkillDetail::Static(s) => Some(s.clone()),
        SkillDetail::Dynamic(f) => f(entry),
    }
}

// ============================================================================
// Options structs
// ============================================================================

/// Options for [`build_runtime_mounted_skill_snapshot`]. Mirrors Node
/// `RuntimeMountedSkillSnapshotOptions` (L270-284).
#[derive(Debug, Clone, Default)]
pub struct RuntimeMountedSkillSnapshotOptions {
    pub adapter_type: String,
    pub available_entries: Vec<PaperclipSkillEntry>,
    pub desired_skills: Vec<String>,
    pub configured_detail: SkillDetail,
    pub missing_detail: Option<String>,
    pub mode: Option<AdapterSkillSyncMode>,
    pub supported: Option<bool>,
    pub unsupported_detail: Option<SkillDetail>,
    pub warnings: Option<Vec<String>>,
    pub external_installed: Option<BTreeMap<String, InstalledSkillTarget>>,
    pub external_location_label: Option<String>,
    pub external_detail: Option<String>,
    pub skills_home: Option<String>,
}

impl RuntimeMountedSkillSnapshotOptions {
    fn missing_detail_value(&self) -> String {
        self.missing_detail.clone().unwrap_or_else(|| {
            "Paperclip cannot find this skill in the local runtime skills directory.".to_string()
        })
    }
    fn external_detail_value(&self) -> String {
        self.external_detail
            .clone()
            .unwrap_or_else(|| "Installed outside Paperclip management.".to_string())
    }
    fn mode_value(&self) -> AdapterSkillSyncMode {
        self.mode.unwrap_or(AdapterSkillSyncMode::Ephemeral)
    }
    fn supported_value(&self) -> bool {
        self.supported
            .unwrap_or_else(|| self.mode_value() != AdapterSkillSyncMode::Unsupported)
    }
    fn warnings_value(&self) -> Vec<String> {
        self.warnings.clone().unwrap_or_default()
    }
}

/// Options for [`build_persistent_skill_snapshot`]. Mirrors Node
/// `PersistentSkillSnapshotOptions` (L256-268).
#[derive(Debug, Clone)]
pub struct PersistentSkillSnapshotOptions {
    pub adapter_type: String,
    pub available_entries: Vec<PaperclipSkillEntry>,
    pub desired_skills: Vec<String>,
    pub installed: BTreeMap<String, InstalledSkillTarget>,
    pub skills_home: String,
    pub location_label: Option<String>,
    pub installed_detail: Option<String>,
    pub missing_detail: String,
    pub external_conflict_detail: String,
    pub external_detail: String,
    pub warnings: Option<Vec<String>>,
}

impl PersistentSkillSnapshotOptions {
    fn warnings_value(&self) -> Vec<String> {
        self.warnings.clone().unwrap_or_default()
    }
}

// ============================================================================
// buildRuntimeMountedSkillSnapshot
// ============================================================================

/// Build a skill snapshot for an adapter that applies skills at
/// runtime (ephemeral or unsupported mode). Mirrors Node
/// `buildRuntimeMountedSkillSnapshot` (L2491-2608).
///
/// The output is sorted by `key.localeCompare(...)` to match the Node
/// reference ordering.
pub fn build_runtime_mounted_skill_snapshot(
    options: &RuntimeMountedSkillSnapshotOptions,
) -> AdapterSkillSnapshot {
    let adapter_type = options.adapter_type.clone();
    let missing_detail = options.missing_detail_value();
    let external_detail = options.external_detail_value();
    let mode = options.mode_value();
    let supported = options.supported_value();
    let mut warnings = options.warnings_value();

    let available_by_key: BTreeMap<String, &PaperclipSkillEntry> = options
        .available_entries
        .iter()
        .map(|entry| (entry.key.clone(), entry))
        .collect();
    let desired_set: BTreeSet<String> = options.desired_skills.iter().cloned().collect();

    let mut entries: Vec<AdapterSkillEntry> = Vec::new();

    // Pass 1: every available entry becomes a managed entry.
    for available in &options.available_entries {
        let desired = desired_set.contains(&available.key);
        if is_paperclip_skill_source_missing(available) {
            let mut entry = AdapterSkillEntry::managed(
                available.key.clone(),
                Some(available.runtime_name.clone()),
                AdapterSkillState::Missing,
            );
            entry.version_id = clone_some(available.version_id.as_deref());
            entry.current_version_id = clone_some(available.current_version_id.as_deref());
            entry.desired = desired;
            entry.detail = Some(resolve_paperclip_skill_missing_detail(
                available,
                &missing_detail,
            ));
            entries.push(entry);
            continue;
        }

        let configured = supported && mode == AdapterSkillSyncMode::Ephemeral && desired;
        let (origin, origin_label, read_only) = build_managed_skill_origin();
        let default_unsupported_detail: SkillDetail = SkillDetail::Static(
            "Desired state is stored in Paperclip only; this adapter cannot apply skills at runtime."
                .to_string(),
        );
        let detail = if desired {
            if configured {
                resolve_skill_detail(&options.configured_detail, available)
            } else {
                let unsupported_detail = options
                    .unsupported_detail
                    .as_ref()
                    .unwrap_or(&default_unsupported_detail);
                resolve_skill_detail(unsupported_detail, available)
            }
        } else {
            None
        };

        let mut entry = AdapterSkillEntry::managed(
            available.key.clone(),
            Some(available.runtime_name.clone()),
            if configured {
                AdapterSkillState::Configured
            } else {
                AdapterSkillState::Available
            },
        );
        entry.version_id = clone_some(available.version_id.as_deref());
        entry.current_version_id = clone_some(available.current_version_id.as_deref());
        entry.desired = desired;
        entry.source_path = Some(available.source.clone());
        entry.detail = detail;
        // Sanity: `build_managed_skill_origin` is the only call site
        // here, so the triplet matches.
        entry.origin = Some(origin);
        entry.origin_label = Some(origin_label);
        entry.read_only = read_only;
        entries.push(entry);
    }

    // Pass 2: each desired skill that is not in the available table
    // becomes an external-unknown entry plus a warning.
    for desired_skill in &options.desired_skills {
        if available_by_key.contains_key(desired_skill) {
            continue;
        }
        warnings.push(format!(
            "Desired skill \"{desired_skill}\" is not available from the Paperclip skills directory."
        ));
        let mut entry =
            AdapterSkillEntry::managed(desired_skill.clone(), None, AdapterSkillState::Missing);
        entry.desired = true;
        entry.detail = Some(missing_detail.clone());
        entry.origin = Some(AdapterSkillOrigin::ExternalUnknown);
        entry.origin_label = Some(AdapterSkillOrigin::ExternalUnknown.label().to_string());
        entries.push(entry);
    }

    // Pass 3: external-installed entries that are not also available.
    if let Some(external_installed) = &options.external_installed {
        for (name, installed_entry) in external_installed {
            let already_known = options
                .available_entries
                .iter()
                .any(|entry| entry.runtime_name == *name);
            if already_known {
                continue;
            }
            let mut entry = AdapterSkillEntry::managed(
                name.clone(),
                Some(name.clone()),
                AdapterSkillState::External,
            );
            entry.desired = false;
            entry.managed = false;
            entry.origin = Some(AdapterSkillOrigin::UserInstalled);
            entry.origin_label = Some(AdapterSkillOrigin::UserInstalled.label().to_string());
            entry.location_label = skill_location_label(options.external_location_label.as_deref());
            entry.read_only = true;
            entry.source_path = None;
            entry.target_path = installed_entry.target_path.clone().or_else(|| {
                options
                    .skills_home
                    .as_deref()
                    .map(|home| format!("{home}/{name}"))
            });
            entry.detail = Some(external_detail.clone());
            entries.push(entry);
        }
    }

    entries.sort_by(|left, right| left.key.cmp(&right.key));

    let desired_skill_entries: Vec<AdapterDesiredSkillEntry> = options
        .desired_skills
        .iter()
        .map(|key| AdapterDesiredSkillEntry {
            key: key.clone(),
            version_id: available_by_key
                .get(key)
                .and_then(|entry| entry.version_id.clone()),
        })
        .collect();

    AdapterSkillSnapshot {
        adapter_type,
        supported,
        mode,
        desired_skills: options.desired_skills.clone(),
        desired_skill_entries,
        entries,
        warnings,
    }
}

// ============================================================================
// buildPersistentSkillSnapshot
// ============================================================================

/// Build a skill snapshot for an adapter that symlinks skills into a
/// persistent directory. Mirrors Node `buildPersistentSkillSnapshot`
/// (L2609-2734).
pub fn build_persistent_skill_snapshot(
    options: &PersistentSkillSnapshotOptions,
) -> AdapterSkillSnapshot {
    let adapter_type = options.adapter_type.clone();
    let mut warnings = options.warnings_value();

    let available_by_key: BTreeMap<String, &PaperclipSkillEntry> = options
        .available_entries
        .iter()
        .map(|entry| (entry.key.clone(), entry))
        .collect();
    let desired_set: BTreeSet<String> = options.desired_skills.iter().cloned().collect();

    let mut entries: Vec<AdapterSkillEntry> = Vec::new();

    for available in &options.available_entries {
        let installed_entry = options.installed.get(&available.runtime_name);
        let desired = desired_set.contains(&available.key);
        if is_paperclip_skill_source_missing(available) {
            let mut entry = AdapterSkillEntry::managed(
                available.key.clone(),
                Some(available.runtime_name.clone()),
                AdapterSkillState::Missing,
            );
            entry.version_id = clone_some(available.version_id.as_deref());
            entry.current_version_id = clone_some(available.current_version_id.as_deref());
            entry.desired = desired;
            entry.target_path = Some(format!(
                "{}/{}",
                options.skills_home, available.runtime_name
            ));
            entry.detail = Some(resolve_paperclip_skill_missing_detail(
                available,
                &options.missing_detail,
            ));
            entries.push(entry);
            continue;
        }

        let mut state = AdapterSkillState::Available;
        let mut managed = false;
        let mut detail: Option<String> = None;

        if installed_entry
            .map(|target| target.target_path.as_deref() == Some(&available.source))
            .unwrap_or(false)
        {
            managed = true;
            state = if desired {
                AdapterSkillState::Installed
            } else {
                AdapterSkillState::Stale
            };
            detail = options.installed_detail.clone();
        } else if installed_entry.is_some() {
            state = AdapterSkillState::External;
            detail = Some(if desired {
                options.external_conflict_detail.clone()
            } else {
                options.external_detail.clone()
            });
        } else if desired {
            state = AdapterSkillState::Missing;
            detail = Some(options.missing_detail.clone());
        }

        let (origin, origin_label, read_only) = build_managed_skill_origin();
        let mut entry = AdapterSkillEntry::managed(
            available.key.clone(),
            Some(available.runtime_name.clone()),
            state,
        );
        entry.version_id = clone_some(available.version_id.as_deref());
        entry.current_version_id = clone_some(available.current_version_id.as_deref());
        entry.desired = desired;
        entry.managed = managed;
        entry.source_path = Some(available.source.clone());
        entry.target_path = Some(format!(
            "{}/{}",
            options.skills_home, available.runtime_name
        ));
        entry.detail = detail;
        entry.origin = Some(origin);
        entry.origin_label = Some(origin_label);
        entry.read_only = read_only;
        entries.push(entry);
    }

    for desired_skill in &options.desired_skills {
        if available_by_key.contains_key(desired_skill) {
            continue;
        }
        warnings.push(format!(
            "Desired skill \"{desired_skill}\" is not available from the Paperclip skills directory."
        ));
        let mut entry =
            AdapterSkillEntry::managed(desired_skill.clone(), None, AdapterSkillState::Missing);
        entry.desired = true;
        entry.detail = Some(
            "Paperclip cannot find this skill in the local runtime skills directory.".to_string(),
        );
        entry.origin = Some(AdapterSkillOrigin::ExternalUnknown);
        entry.origin_label = Some(AdapterSkillOrigin::ExternalUnknown.label().to_string());
        entries.push(entry);
    }

    for (name, installed_entry) in &options.installed {
        let already_known = options
            .available_entries
            .iter()
            .any(|entry| entry.runtime_name == *name);
        if already_known {
            continue;
        }
        let mut entry = AdapterSkillEntry::managed(
            name.clone(),
            Some(name.clone()),
            AdapterSkillState::External,
        );
        entry.desired = false;
        entry.managed = false;
        entry.origin = Some(AdapterSkillOrigin::UserInstalled);
        entry.origin_label = Some(AdapterSkillOrigin::UserInstalled.label().to_string());
        entry.location_label = skill_location_label(options.location_label.as_deref());
        entry.read_only = true;
        entry.source_path = None;
        entry.target_path = Some(
            installed_entry
                .target_path
                .clone()
                .unwrap_or_else(|| format!("{}/{}", options.skills_home, name)),
        );
        entry.detail = Some(options.external_detail.clone());
        entries.push(entry);
    }

    entries.sort_by(|left, right| left.key.cmp(&right.key));

    let desired_skill_entries: Vec<AdapterDesiredSkillEntry> = options
        .desired_skills
        .iter()
        .map(|key| AdapterDesiredSkillEntry {
            key: key.clone(),
            version_id: available_by_key
                .get(key)
                .and_then(|entry| entry.version_id.clone()),
        })
        .collect();

    AdapterSkillSnapshot {
        adapter_type,
        supported: true,
        mode: AdapterSkillSyncMode::Persistent,
        desired_skills: options.desired_skills.clone(),
        desired_skill_entries,
        entries,
        warnings,
    }
}

fn clone_some(value: Option<&str>) -> Option<String> {
    value.map(|s| s.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, runtime_name: &str, source: &str) -> PaperclipSkillEntry {
        PaperclipSkillEntry {
            key: key.to_string(),
            runtime_name: runtime_name.to_string(),
            source: source.to_string(),
            version_id: None,
            current_version_id: None,
            source_status: PaperclipSkillSourceStatus::Available,
            missing_detail: None,
        }
    }

    fn missing_entry(
        key: &str,
        runtime_name: &str,
        source: &str,
        detail: Option<&str>,
    ) -> PaperclipSkillEntry {
        PaperclipSkillEntry {
            key: key.to_string(),
            runtime_name: runtime_name.to_string(),
            source: source.to_string(),
            version_id: None,
            current_version_id: None,
            source_status: PaperclipSkillSourceStatus::Missing,
            missing_detail: detail.map(|s| s.to_string()),
        }
    }

    // ----- AdapterSkillOrigin::label -----

    #[test]
    fn origin_labels_match_node() {
        assert_eq!(
            AdapterSkillOrigin::CompanyManaged.label(),
            "Managed by Paperclip"
        );
        assert_eq!(AdapterSkillOrigin::UserInstalled.label(), "User-installed");
        assert_eq!(
            AdapterSkillOrigin::ExternalUnknown.label(),
            "External or unavailable"
        );
    }

    // ----- skillLocationLabel -----

    #[test]
    fn skill_location_label_drops_blank_input() {
        assert!(skill_location_label(None).is_none());
        assert!(skill_location_label(Some("")).is_none());
        assert!(skill_location_label(Some("   ")).is_none());
    }

    #[test]
    fn skill_location_label_trims_whitespace() {
        assert_eq!(
            skill_location_label(Some("  /home/alice/skills  ")),
            Some("/home/alice/skills".to_string())
        );
    }

    // ----- buildManagedSkillOrigin -----

    #[test]
    fn managed_skill_origin_returns_stable_triplet() {
        let (origin, label, read_only) = build_managed_skill_origin();
        assert_eq!(origin, AdapterSkillOrigin::CompanyManaged);
        assert_eq!(label, "Managed by Paperclip");
        assert!(!read_only);
    }

    // ----- isPaperclipSkillSourceMissing -----

    #[test]
    fn source_missing_detection() {
        let available = entry("alpha", "alpha", "/skills/alpha");
        assert!(!is_paperclip_skill_source_missing(&available));
        let missing = missing_entry("alpha", "alpha", "/skills/alpha", None);
        assert!(is_paperclip_skill_source_missing(&missing));
    }

    // ----- resolvePaperclipSkillMissingDetail -----

    #[test]
    fn missing_detail_prefers_entry_supplied_value() {
        let entry = missing_entry("a", "a", "/a", Some("custom detail"));
        assert_eq!(
            resolve_paperclip_skill_missing_detail(&entry, "fallback"),
            "custom detail".to_string()
        );
    }

    #[test]
    fn missing_detail_falls_back_when_entry_blank() {
        let entry = missing_entry("a", "a", "/a", Some("   "));
        assert_eq!(
            resolve_paperclip_skill_missing_detail(&entry, "fallback"),
            "fallback".to_string()
        );
        let entry = missing_entry("a", "a", "/a", None);
        assert_eq!(
            resolve_paperclip_skill_missing_detail(&entry, "fallback"),
            "fallback".to_string()
        );
    }

    // ----- resolveSkillDetail -----

    #[test]
    fn resolve_skill_detail_handles_string_and_closure_and_none() {
        let entry = entry("alpha", "alpha", "/skills/alpha");
        assert_eq!(resolve_skill_detail(&SkillDetail::None, &entry), None);
        assert_eq!(
            resolve_skill_detail(&SkillDetail::Static("static".to_string()), &entry),
            Some("static".to_string())
        );
        let dynamic: SkillDetail =
            SkillDetail::Dynamic(std::sync::Arc::new(|e| Some(format!("dyn-{}", e.key))));
        assert_eq!(
            resolve_skill_detail(&dynamic, &entry),
            Some("dyn-alpha".to_string())
        );
    }

    // ----- buildRuntimeMountedSkillSnapshot -----

    #[test]
    fn runtime_snapshot_marks_available_entries() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec![],
            configured_detail: SkillDetail::Static("configured".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(snapshot.adapter_type, "test");
        assert!(snapshot.supported);
        assert_eq!(snapshot.mode, AdapterSkillSyncMode::Ephemeral);
        assert_eq!(snapshot.entries.len(), 1);
        let only = &snapshot.entries[0];
        assert_eq!(only.key, "alpha");
        assert_eq!(only.state, AdapterSkillState::Available);
        assert!(!only.desired);
        assert!(only.managed);
        assert!(only.detail.is_none());
        assert!(snapshot.warnings.is_empty());
    }

    #[test]
    fn runtime_snapshot_marks_desired_supported_ephemeral_as_configured() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string()],
            configured_detail: SkillDetail::Static("applied at runtime".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Configured);
        assert!(snapshot.entries[0].desired);
        assert_eq!(
            snapshot.entries[0].detail,
            Some("applied at runtime".to_string())
        );
        assert_eq!(
            snapshot.desired_skill_entries,
            vec![AdapterDesiredSkillEntry {
                key: "alpha".to_string(),
                version_id: None,
            }]
        );
    }

    #[test]
    fn runtime_snapshot_unsupported_mode_marks_desired_with_unsupported_detail() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string()],
            configured_detail: SkillDetail::Static("applied at runtime".to_string()),
            mode: Some(AdapterSkillSyncMode::Unsupported),
            supported: Some(false),
            unsupported_detail: Some(SkillDetail::Static("cannot apply".to_string())),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert!(!snapshot.supported);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Available);
        assert!(snapshot.entries[0].desired);
        assert_eq!(snapshot.entries[0].detail, Some("cannot apply".to_string()));
    }

    #[test]
    fn runtime_snapshot_handles_missing_source() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![missing_entry(
                "alpha",
                "alpha",
                "/skills/alpha",
                Some("source disappeared"),
            )],
            desired_skills: vec!["alpha".to_string()],
            configured_detail: SkillDetail::Static("applied at runtime".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Missing);
        assert_eq!(
            snapshot.entries[0].detail,
            Some("source disappeared".to_string())
        );
        assert!(snapshot.entries[0].source_path.is_none());
        assert!(snapshot.entries[0].target_path.is_none());
    }

    #[test]
    fn runtime_snapshot_emits_warning_for_unavailable_desired_skills() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string(), "ghost".to_string()],
            configured_detail: SkillDetail::Static("applied".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.warnings.len(), 1);
        assert!(snapshot.warnings[0].contains("ghost"));
        let ghost = snapshot.entries.iter().find(|e| e.key == "ghost").unwrap();
        assert_eq!(ghost.state, AdapterSkillState::Missing);
        assert_eq!(ghost.origin, Some(AdapterSkillOrigin::ExternalUnknown));
    }

    #[test]
    fn runtime_snapshot_includes_external_installed_entries() {
        let mut external = BTreeMap::new();
        external.insert(
            "user-skill".to_string(),
            InstalledSkillTarget {
                target_path: Some("/external/user-skill".to_string()),
                kind: InstalledSkillTargetKind::Directory,
            },
        );
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string()],
            configured_detail: SkillDetail::Static("applied".to_string()),
            external_installed: Some(external),
            external_location_label: Some("  /external  ".to_string()),
            external_detail: Some("outside Paperclip".to_string()),
            skills_home: Some("/skills/home".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        let external_entry = snapshot
            .entries
            .iter()
            .find(|e| e.key == "user-skill")
            .unwrap();
        assert_eq!(external_entry.state, AdapterSkillState::External);
        assert_eq!(
            external_entry.origin,
            Some(AdapterSkillOrigin::UserInstalled)
        );
        assert!(external_entry.read_only);
        assert!(!external_entry.managed);
        assert_eq!(external_entry.location_label, Some("/external".to_string()));
        assert_eq!(
            external_entry.target_path,
            Some("/external/user-skill".to_string())
        );
        assert_eq!(external_entry.detail, Some("outside Paperclip".to_string()));
    }

    #[test]
    fn runtime_snapshot_skips_external_when_runtime_name_matches_available() {
        let mut external = BTreeMap::new();
        external.insert(
            "alpha".to_string(),
            InstalledSkillTarget {
                target_path: Some("/external/alpha".to_string()),
                kind: InstalledSkillTargetKind::Directory,
            },
        );
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec![],
            configured_detail: SkillDetail::Static("applied".to_string()),
            external_installed: Some(external),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(snapshot.entries.len(), 1);
    }

    #[test]
    fn runtime_snapshot_sorts_entries_by_key() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![
                entry("charlie", "charlie", "/c"),
                entry("alpha", "alpha", "/a"),
                entry("bravo", "bravo", "/b"),
            ],
            desired_skills: vec![],
            configured_detail: SkillDetail::Static("applied".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        let keys: Vec<&str> = snapshot.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn runtime_snapshot_preserves_desired_skill_entries_in_order() {
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/a"), entry("beta", "beta", "/b")],
            desired_skills: vec!["beta".to_string(), "alpha".to_string()],
            configured_detail: SkillDetail::Static("applied".to_string()),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(
            snapshot.desired_skills,
            vec!["beta".to_string(), "alpha".to_string()]
        );
        assert_eq!(
            snapshot
                .desired_skill_entries
                .iter()
                .map(|e| e.key.as_str())
                .collect::<Vec<_>>(),
            vec!["beta", "alpha"]
        );
    }

    #[test]
    fn runtime_snapshot_default_warnings_can_be_extended() {
        let mut extra_warnings = vec!["preset warning".to_string()];
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/a")],
            desired_skills: vec!["ghost".to_string()],
            configured_detail: SkillDetail::Static("applied".to_string()),
            warnings: Some(std::mem::take(&mut extra_warnings)),
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(snapshot.warnings.len(), 2);
        assert_eq!(snapshot.warnings[0], "preset warning");
        assert!(snapshot.warnings[1].contains("ghost"));
    }

    #[test]
    fn runtime_snapshot_uses_closure_detail_when_desired() {
        let dynamic: SkillDetail = SkillDetail::Dynamic(std::sync::Arc::new(|entry| {
            Some(format!("applied-{}", entry.key))
        }));
        let options = RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/a")],
            desired_skills: vec!["alpha".to_string()],
            configured_detail: dynamic,
            ..Default::default()
        };
        let snapshot = build_runtime_mounted_skill_snapshot(&options);
        assert_eq!(
            snapshot.entries[0].detail,
            Some("applied-alpha".to_string())
        );
    }

    // ----- buildPersistentSkillSnapshot -----

    #[test]
    fn persistent_snapshot_marks_available_entries() {
        let mut installed = BTreeMap::new();
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec![],
            installed,
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        installed = options.installed.clone();
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.mode, AdapterSkillSyncMode::Persistent);
        assert!(snapshot.supported);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Available);
        assert!(!snapshot.entries[0].managed);
        assert_eq!(
            snapshot.entries[0].target_path,
            Some("/skills/home/alpha".to_string())
        );
        let _ = installed;
    }

    #[test]
    fn persistent_snapshot_marks_installed_when_target_matches_source() {
        let mut installed = BTreeMap::new();
        installed.insert(
            "alpha".to_string(),
            InstalledSkillTarget {
                target_path: Some("/skills/alpha".to_string()),
                kind: InstalledSkillTargetKind::Symlink,
            },
        );
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string()],
            installed,
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: Some("installed".to_string()),
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Installed);
        assert!(snapshot.entries[0].managed);
        assert_eq!(snapshot.entries[0].detail, Some("installed".to_string()));
    }

    #[test]
    fn persistent_snapshot_marks_stale_when_installed_but_not_desired() {
        let mut installed = BTreeMap::new();
        installed.insert(
            "alpha".to_string(),
            InstalledSkillTarget {
                target_path: Some("/skills/alpha".to_string()),
                kind: InstalledSkillTargetKind::Symlink,
            },
        );
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec![],
            installed,
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Stale);
        assert!(snapshot.entries[0].managed);
        assert!(snapshot.entries[0].detail.is_none());
    }

    #[test]
    fn persistent_snapshot_marks_external_conflict_when_target_differs() {
        let mut installed = BTreeMap::new();
        installed.insert(
            "alpha".to_string(),
            InstalledSkillTarget {
                target_path: Some("/other/alpha".to_string()),
                kind: InstalledSkillTargetKind::Symlink,
            },
        );
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string()],
            installed,
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict-detail".to_string(),
            external_detail: "external-detail".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::External);
        assert!(!snapshot.entries[0].managed);
        assert_eq!(
            snapshot.entries[0].detail,
            Some("conflict-detail".to_string())
        );
    }

    #[test]
    fn persistent_snapshot_marks_external_when_not_desired_and_target_differs() {
        let mut installed = BTreeMap::new();
        installed.insert(
            "alpha".to_string(),
            InstalledSkillTarget {
                target_path: Some("/other/alpha".to_string()),
                kind: InstalledSkillTargetKind::Symlink,
            },
        );
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec![],
            installed,
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict-detail".to_string(),
            external_detail: "external-detail".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::External);
        assert_eq!(
            snapshot.entries[0].detail,
            Some("external-detail".to_string())
        );
    }

    #[test]
    fn persistent_snapshot_marks_missing_when_desired_but_not_installed() {
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["alpha".to_string()],
            installed: BTreeMap::new(),
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing-detail".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Missing);
        assert!(!snapshot.entries[0].managed);
        assert_eq!(
            snapshot.entries[0].detail,
            Some("missing-detail".to_string())
        );
    }

    #[test]
    fn persistent_snapshot_emits_warning_for_unavailable_desired_skills() {
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec!["ghost".to_string()],
            installed: BTreeMap::new(),
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.warnings.len(), 1);
        assert!(snapshot.warnings[0].contains("ghost"));
    }

    #[test]
    fn persistent_snapshot_handles_missing_source_with_target_path() {
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![missing_entry(
                "alpha",
                "alpha",
                "/skills/alpha",
                Some("missing"),
            )],
            desired_skills: vec![],
            installed: BTreeMap::new(),
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        assert_eq!(snapshot.entries[0].state, AdapterSkillState::Missing);
        assert_eq!(
            snapshot.entries[0].target_path,
            Some("/skills/home/alpha".to_string())
        );
        assert_eq!(snapshot.entries[0].detail, Some("missing".to_string()));
    }

    #[test]
    fn persistent_snapshot_includes_external_installed_entries() {
        let mut installed = BTreeMap::new();
        installed.insert(
            "user-skill".to_string(),
            InstalledSkillTarget {
                target_path: None,
                kind: InstalledSkillTargetKind::Directory,
            },
        );
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
            desired_skills: vec![],
            installed,
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external-detail".to_string(),
            warnings: None,
            location_label: Some("  /external  ".to_string()),
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        let external = snapshot
            .entries
            .iter()
            .find(|e| e.key == "user-skill")
            .unwrap();
        assert_eq!(external.state, AdapterSkillState::External);
        assert_eq!(
            external.target_path,
            Some("/skills/home/user-skill".to_string())
        );
        assert_eq!(external.detail, Some("external-detail".to_string()));
        assert_eq!(external.location_label, Some("/external".to_string()));
    }

    #[test]
    fn persistent_snapshot_sorts_entries_by_key() {
        let options = PersistentSkillSnapshotOptions {
            adapter_type: "test".to_string(),
            available_entries: vec![
                entry("charlie", "charlie", "/c"),
                entry("alpha", "alpha", "/a"),
            ],
            desired_skills: vec![],
            installed: BTreeMap::new(),
            skills_home: "/skills/home".to_string(),
            missing_detail: "missing".to_string(),
            external_conflict_detail: "conflict".to_string(),
            external_detail: "external".to_string(),
            warnings: None,
            location_label: None,
            installed_detail: None,
        };
        let snapshot = build_persistent_skill_snapshot(&options);
        let keys: Vec<&str> = snapshot.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["alpha", "charlie"]);
    }
}
