# R633 — workspace-runtime-service-authz module

## Status

DONE — module source landed, compiled, all 12 unit tests passing.

## Files added / modified

| Path | Status | Notes |
|---|---|---|
| `crates/pc-authz/src/runtime_service.rs` | new (663 LOC) | Pure-function helpers |
| `crates/pc-authz/src/lib.rs` | modified | `pub mod runtime_service;` + re-exports |

## Module public API

```
use pc_authz::{
    assert_can_manage_project_workspace_runtime_services,
    assert_can_manage_execution_workspace_runtime_services,
    RuntimeServiceContext, RuntimeServiceActor, RuntimeServiceAuthzError,
    AgentContextRow, RunContextRow, IssueContextRow, ProjectContextRow,
    run_execution_policy, read_run_issue_id,
    WORKSPACE_RUNTIME_ELIGIBLE_ISSUE_STATUSES,
};
```

Six error variants (mirrors Node `workspace-runtime-service-authz.ts`):
- AgentRequired (code: agent_required)
- CrossCompany (code: cross_company)
- MissingPermission (code: missing_permission)
- LowTrustDenied(String) (code: low_trust_denied)
- WorkspaceNotFound (code: workspace_not_found)
- CompanyAccessDenied (code: company_access_denied)

## Decision tree (mirrors Node)

1. Board / instance-admin: always allowed (no DB lookup)
2. Agent: must satisfy
   - company_id matches
   - trust-preset resolution passes (via pc_authz::trust::resolve_core_trust_preset)
   - actor run has trustBoundary that grants runtime.manage
     (boundary has at least one of allowed_tool_classes, allowed_agent_ids, root_issue_id, issue_ids)
   - CEO role shortcut: standard preset + linked scope issue -> allow
   - Engineer shortcut: linked assignee issue (with active assignee in subtree) -> allow
   - any linked scope issue (active status, not hidden) -> allow
3. User: must have company_ids containing target company

## Test coverage (12 cases)

| # | Test | Scenario |
|---|---|---|
| 1 | board_user_is_always_allowed | Board user -> ok |
| 2 | instance_admin_actor_with_no_company_ids_is_allowed | Instance admin -> ok |
| 3 | ceo_agent_with_linked_scope_issue_is_allowed | CEO + linked issue -> ok |
| 4 | engineer_without_assignment_is_denied | Engineer + no linked issue -> missing_permission |
| 5 | engineer_with_active_assignment_is_allowed | Engineer + linked assignee -> ok |
| 6 | completed_issue_does_not_count_as_linked_scope | Done status -> missing_permission |
| 7 | cross_company_user_is_denied | User without membership -> company_access_denied |
| 8 | cross_company_agent_is_denied | Agent with different companyId -> cross_company |
| 9 | low_trust_ceo_without_boundary_is_denied | Low-trust run without boundary -> low_trust_denied |
| 10 | low_trust_ceo_with_full_boundary_is_allowed | Low-trust + full boundary + linked -> ok |
| 11 | read_run_issue_id_handles_both_paths | Direct issueId + nested paperclipIssue.id + None + invalid |
| 12 | run_execution_policy_extracts_policy_field | Extract from contextSnapshot.executionPolicy |

## Test results

```
cargo test -p pc-authz --lib runtime_service
cargo test: 12 passed, 73 filtered out (1 suite, 0.00s)

cargo test -p pc-authz --lib
cargo test: 85 passed (1 suite, 0.00s)
```

## Key design decisions

1. Pure-function core: RuntimeServiceContext carries pre-fetched rows; the helper
   never touches DB. Caller is responsible for IO.
2. Hand-written Default: RuntimeServiceActor is an enum with data, so auto-Default
   is impossible. A hand-written Default for RuntimeServiceContext returns a
   board/instance-admin actor so ..Default::default() ergonomics work in tests.
3. Re-export of trust types: imports crate::trust::{ResolveInput, ...} so
   callers dont need to know the inner module.
4. Single source of truth for WORKSPACE_RUNTIME_ELIGIBLE_ISSUE_STATUSES: pub const
   matches Node string literal array.

## Next: HTTP wiring (R634 candidate)

The existing stub `crates/pc-http/src/routes/workspace_runtime_service_authz.rs` still
returns a fake empty `services` array for URL compatibility. A future R634 round
should add `load_runtime_service_context_for_workspace` (DB loaders) + wire
`assert_can_manage_*` into the existing
`runtime_service_action` / `workspace_runtime_action` handlers.
