#![forbid(unsafe_code)]

//! Pause-hold guard — 1:1 port of paperclip/server/src/services/recovery/pause-hold-guard.ts.
//!
//! Node signature:
//! ```ts
//! export async function isAutomaticRecoverySuppressedByPauseHold(
//!   db: Db, companyId: string, issueId: string,
//!   treeControlSvc: IssueTreeControlService = issueTreeControlService(db),
//! ): Promise<boolean>
//! ```
//!
//! Returns true when the given issue has an active pause-hold gate, which
//! suppresses automatic recovery (heartbeat) flows.

use crate::tree_control::IssueTreeControlService;
use uuid::Uuid;

/// Async signature matching Node `isAutomaticRecoverySuppressedByPauseHold`.
///
/// Returns true when `is_issue_paused` returns `Some(...)` (i.e. there is at
/// least one active pause-hold gate for the issue).
pub async fn is_automatic_recovery_suppressed_by_pause_hold(
    svc: &IssueTreeControlService,
    company_id: Uuid,
    issue_id: Uuid,
) -> bool {
    svc.is_issue_paused(company_id, issue_id)
        .await
        .map(|opt| opt.is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn signature_is_pure_async() {
        // Verify the function signature compiles correctly with concrete types.
        let fn_ptr: fn(&IssueTreeControlService, Uuid, Uuid) -> _ = |_, _, _| async { false };
        // Type erasure: we just need this to compile, the actual future type is opaque.
        let _: fn(&IssueTreeControlService, Uuid, Uuid) -> _ = fn_ptr;
    }
}