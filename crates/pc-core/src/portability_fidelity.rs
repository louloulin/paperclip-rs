//! Thin re-export of `pc-portability-fidelity` (R547).
//!
//! R-INTEGRATION-5 (R565): `pc-core/src/portability_fidelity.rs` previously
//! duplicated the type definitions and helpers from `pc-portability-fidelity`.
//! The two were kept in sync by hand and drifted subtly. Now `pc-portability-fidelity`
//! is the single source of truth, and this module just re-exports its public
//! surface so existing `pc_core::portability_fidelity::*` import paths keep
//! compiling unchanged.
//!
//! What stays local:
//! - This module file itself (1 line of doc + 1 line of re-export), so the
//!   `pc_core::portability_fidelity::*` API surface is preserved
//!
//! What moved:
//! - All types (ExportFidelityCounts, PortabilityFidelityWarning, etc.)
//! - All functions (build_export_fidelity_warnings, normalize_export_fidelity_counts)
//! - All constants (EXPORT_FIDELITY_REPORT_SCHEMA, EXPORT_FIDELITY_COUNT_KEYS)
//! - All lints (#![forbid(unsafe_code)], #![allow(clippy::doc_markdown)])
//! → canonical home: `pc_portability_fidelity`

pub use pc_portability_fidelity::*;
