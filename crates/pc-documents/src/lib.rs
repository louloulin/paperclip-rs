#![forbid(unsafe_code)]

//! Document domain service layer.
//!
//! Provides [`DocumentService`] — a high-level facade over
//! [`pc_repos::document::DocumentRepo`] that:
//!
//! * Validates inputs (non-empty body, allowed format, locked documents are
//!   read-only until unlocked, annotation thread status transitions are
//!   one-way `open → resolved`)
//! * Routes writes through a [`DocumentHook`] chain so callers can layer
//!   activity / realtime / plugin side-effects without touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!
//! Documents have an append-only revision log (`document_revisions`) and may
//! be locked by a single actor (agent or user) to prevent concurrent edits.
//! Annotations attach to a specific range inside a document and accumulate
//! threaded comments.

pub mod pure;

mod service;

pub use pure::{
    is_allowed_author_type, is_allowed_format, normalize_document_key, validate_annotation_comment,
    validate_annotation_thread, validate_document_patch, validate_upsert_issue_document,
    NormalizedCreate, ALLOWED_FORMATS, ANNOTATION_AUTHOR_TYPES, DEFAULT_FORMAT,
};
pub use service::{
    CreateAnnotationComment, CreateAnnotationThreadInput, CreateDocument, DocumentHook,
    DocumentHookEvent, DocumentPatch, DocumentService, NoopDocumentHook, RecordingDocumentHook,
    UpsertIssueDocument,
};
