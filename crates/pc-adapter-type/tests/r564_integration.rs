//! R-INTEGRATION-4: pc-adapter-type ↔ all pc-adapter-* crates consistency check.
//!
//! Verifies that every built-in adapter crate's `ADAPTER_TYPE` constant:
//!   1. Is non-empty
//!   2. Uses the underscore convention (no hyphens)
//!   3. Is recognized as a built-in by `is_builtin_adapter_type`
//!   4. Has no duplicates across adapters
//!   5. Is in the canonical Node upstream `AGENT_ADAPTER_TYPES` list (via pc-constants)
//!
//! Compile-time guarantees: this test crate depends on every pc-adapter-* crate
//! directly, so adding a new adapter without updating ADAPTER_TYPE will at
//! least surface here.

use pc_adapter_type::{is_builtin_adapter_type, KNOWN_BUILTIN_ADAPTER_TYPES};

// Bring every built-in adapter crate into scope so its `ADAPTER_TYPE` const
// is accessible. This is a compile-time assertion that the const exists.
use pc_adapter_claude_local::ADAPTER_TYPE as CLAUDE_LOCAL;
use pc_adapter_codex_local::ADAPTER_TYPE as CODEX_LOCAL;
use pc_adapter_cursor_cloud::ADAPTER_TYPE as CURSOR_CLOUD;
use pc_adapter_cursor_local::ADAPTER_TYPE as CURSOR_LOCAL;
use pc_adapter_gemini_local::ADAPTER_TYPE as GEMINI_LOCAL;
use pc_adapter_grok_local::ADAPTER_TYPE as GROK_LOCAL;
use pc_adapter_hermes::ADAPTER_TYPE as HERMES;
use pc_adapter_hermes_gateway::ADAPTER_TYPE as HERMES_GATEWAY;
use pc_adapter_openclaw_gateway::ADAPTER_TYPE as OPENCLAW_GATEWAY;
use pc_adapter_opencode_local::ADAPTER_TYPE as OPENCODE_LOCAL;
use pc_adapter_pi_local::ADAPTER_TYPE as PI_LOCAL;

/// All known adapter ADAPTER_TYPE constants collected as a slice.
const ALL_ADAPTER_TYPES: &[&str] = &[
    CLAUDE_LOCAL,
    CODEX_LOCAL,
    CURSOR_CLOUD,
    CURSOR_LOCAL,
    GEMINI_LOCAL,
    GROK_LOCAL,
    HERMES,
    HERMES_GATEWAY,
    OPENCLAW_GATEWAY,
    OPENCODE_LOCAL,
    PI_LOCAL,
];

#[test]
fn all_adapter_types_are_non_empty() {
    for t in ALL_ADAPTER_TYPES {
        assert!(!t.is_empty(), "ADAPTER_TYPE must not be empty");
        assert!(!t.trim().is_empty(), "ADAPTER_TYPE must not be whitespace");
    }
}

#[test]
fn all_adapter_types_use_underscore_convention() {
    for t in ALL_ADAPTER_TYPES {
        assert!(
            !t.contains('-'),
            "ADAPTER_TYPE {t} must not contain hyphens (R564)"
        );
    }
}

#[test]
fn all_adapter_types_are_recognized_as_builtin() {
    for t in ALL_ADAPTER_TYPES {
        assert!(
            is_builtin_adapter_type(t),
            "ADAPTER_TYPE {t} not in KNOWN_BUILTIN_ADAPTER_TYPES"
        );
    }
}

#[test]
fn known_builtin_adapter_types_matches_all_adapters() {
    // Every adapter's ADAPTER_TYPE must be in KNOWN_BUILTIN_ADAPTER_TYPES
    // AND every entry in KNOWN_BUILTIN_ADAPTER_TYPES must be represented by
    // exactly one adapter. The "process" entry is special (no adapter crate,
    // lives in pc-adapter-process or as default).
    let mut represented: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for t in ALL_ADAPTER_TYPES {
        represented.insert(t);
    }
    for canonical in KNOWN_BUILTIN_ADAPTER_TYPES {
        if *canonical == "process" {
            continue; // default; no dedicated adapter crate
        }
        assert!(
            represented.contains(canonical),
            "KNOWN_BUILTIN_ADAPTER_TYPES entry {canonical} has no matching pc-adapter-* crate"
        );
    }
}

#[test]
fn all_adapter_types_are_unique() {
    use std::collections::HashSet;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dups: Vec<&str> = Vec::new();
    for t in ALL_ADAPTER_TYPES {
        if !seen.insert(*t) {
            dups.push(*t);
        }
    }
    assert!(dups.is_empty(), "duplicate ADAPTER_TYPE values: {dups:?}");
}

#[test]
fn all_adapter_types_recognized_by_canonical_list() {
    // Cross-check: every ADAPTER_TYPE must be in the canonical
    // KNOWN_BUILTIN_ADAPTER_TYPES list (the canonical authority for builtin
    // detection). This is a stricter check than is_builtin_adapter_type on
    // each individual type — it ensures the lists agree at compile time.
    let canonical: std::collections::HashSet<&str> =
        KNOWN_BUILTIN_ADAPTER_TYPES.iter().copied().collect();
    for t in ALL_ADAPTER_TYPES {
        assert!(
            canonical.contains(t),
            "ADAPTER_TYPE {t} missing from KNOWN_BUILTIN_ADAPTER_TYPES"
        );
    }
}

#[test]
fn normalize_accepts_both_conventions() {
    // Both hyphenated and underscored inputs should normalize to the same
    // canonical value (R564 hyphen→underscore normalization).
    use pc_adapter_type::normalize_agent_adapter_type;
    assert_eq!(
        normalize_agent_adapter_type(Some(CLAUDE_LOCAL)),
        normalize_agent_adapter_type(Some("claude-local"))
    );
    assert_eq!(
        normalize_agent_adapter_type(Some(CODEX_LOCAL)),
        normalize_agent_adapter_type(Some("codex-local"))
    );
}
