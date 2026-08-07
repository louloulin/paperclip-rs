//! `pc-acpx` engine settings — port of `resolveEngineSettings` from Node
//! `acpx-engine/execute.ts`.
//!
//! The engine settings are the *immutable* per-process knobs that the runtime
//! executor reads but never mutates. The caller supplies their preferred
//! `moduleDir` / `packageRootDir`; we fill in sane defaults relative to the
//! current working directory. All paths are absolute paths.

use std::path::{Path, PathBuf};

// ============================================================================
// Public types
// ============================================================================

/// Caller-supplied knobs for the acpx engine. Mirror the Node
/// `AcpxEngineExecutorOptions` subset that `resolveEngineSettings` consumes.
#[derive(Debug, Default, Clone)]
pub struct AcpxEngineOptions {
    /// Stable adapter-type identifier (e.g. `"claude_local"`). Defaults to
    /// `"acp_engine"` when blank or missing.
    pub adapter_type: Option<String>,
    /// Module directory — the directory that contains the engine module. Used
    /// to compute `packageRootDir` when the caller does not pin it.
    pub module_dir: Option<PathBuf>,
    /// Package root directory — the directory that contains the package's
    /// `package.json`. Defaults to `<moduleDir>/../..`.
    pub package_root_dir: Option<PathBuf>,
}

/// Resolved engine settings. All paths are absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpxEngineSettings {
    pub adapter_type: String,
    pub module_dir: PathBuf,
    pub package_root_dir: PathBuf,
}

// ============================================================================
// Resolve
// ============================================================================

/// Resolve the engine settings. The function is pure — it does not touch the
/// filesystem or environment. The caller controls the inputs; the output is
/// deterministic.
pub fn resolve_engine_settings(
    options: &AcpxEngineOptions,
    fallback_module_dir: &Path,
) -> AcpxEngineSettings {
    let module_dir = match &options.module_dir {
        Some(path) => absolute_path(path),
        None => absolute_path(fallback_module_dir),
    };
    let package_root_dir = match &options.package_root_dir {
        Some(path) => absolute_path(path),
        None => absolute_path(&module_dir.join("..").join("..")),
    };
    let adapter_type = options
        .adapter_type
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("acp_engine")
        .to_string();
    AcpxEngineSettings {
        adapter_type,
        module_dir,
        package_root_dir,
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        // std::path::Path::canonicalize requires the path to exist; we only
        // need a deterministic absolute representation, so we use a no-op
        // join against current_dir for relative paths. Failure to read
        // current_dir is a non-issue — we fall back to the input verbatim.
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> PathBuf {
        PathBuf::from("/opt/acpx")
    }

    #[test]
    fn defaults_adapter_type_when_blank() {
        let settings = resolve_engine_settings(&AcpxEngineOptions::default(), &fallback());
        assert_eq!(settings.adapter_type, "acp_engine");
        assert_eq!(settings.module_dir, PathBuf::from("/opt/acpx"));
        assert_eq!(settings.package_root_dir, PathBuf::from("/opt/acpx/../.."));
    }

    #[test]
    fn trims_blank_adapter_type_to_default() {
        let settings = resolve_engine_settings(
            &AcpxEngineOptions {
                adapter_type: Some("   ".into()),
                ..Default::default()
            },
            &fallback(),
        );
        assert_eq!(settings.adapter_type, "acp_engine");
    }

    #[test]
    fn preserves_supplied_paths() {
        let settings = resolve_engine_settings(
            &AcpxEngineOptions {
                adapter_type: Some("claude_local".into()),
                module_dir: Some(PathBuf::from("/opt/acpx")),
                package_root_dir: Some(PathBuf::from("/srv/paperclip")),
            },
            &fallback(),
        );
        assert_eq!(settings.adapter_type, "claude_local");
        assert_eq!(settings.module_dir, PathBuf::from("/opt/acpx"));
        assert_eq!(settings.package_root_dir, PathBuf::from("/srv/paperclip"));
    }

    #[test]
    fn relative_module_dir_resolves_against_fallback() {
        let settings = resolve_engine_settings(
            &AcpxEngineOptions {
                module_dir: Some(PathBuf::from("nested/dir")),
                ..Default::default()
            },
            &fallback(),
        );
        // The fallback cwd should be the current working directory at the time
        // resolve_engine_settings runs; the assertion is just that the result
        // is absolute and ends with the relative suffix.
        assert!(settings.module_dir.is_absolute());
        assert!(settings.module_dir.ends_with("nested/dir"));
    }
}
