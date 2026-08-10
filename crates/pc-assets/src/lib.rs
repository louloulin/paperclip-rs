#![forbid(unsafe_code)]

//! Asset domain service layer.
//!
//! Provides [`AssetService`] — a high-level facade over
//! [`pc_repos::asset::AssetRepo`] that:
//!
//! * Validates inputs (non-nil company, non-empty provider / object_key /
//!   content_type / sha256, non-negative byte_size)
//! * Routes writes through an [`AssetHook`] chain so callers can layer
//!   activity / realtime / storage side-effects without touching SQL
//! * Translates repo `sqlx::Error` into [`pc_errors::Error`] so HTTP / CLI
//!   layers only need to handle one error type
//!
//! Assets are stored by external providers (`local` / `s3` / etc.). Each asset
//! belongs to exactly one company. Creation and deletion are the only write
//! paths — there is no `update` operation in this domain (immutable storage
//! references).

mod service;

pub use service::{
    AssetHook, AssetHookEvent, AssetRow as PublicAssetRow, AssetService, CreateAssetRecord,
    NoopAssetHook, RecordingAssetHook,
};
