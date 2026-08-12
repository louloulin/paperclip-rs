# R634 — workspace-runtime-service-authz HTTP 接入

## Status

DONE — R633 pure-function module is now wired into real HTTP endpoints.

## Files added / modified

| Path | Status | Notes |
|---|---|---|
| crates/pc-http/src/authz_loaders.rs | new (241 LOC) | DB loaders for RuntimeServiceContext |
| crates/pc-http/src/authz_runtime_service.rs | new (253 LOC) | Compose loaders + assert + error mapping |
| crates/pc-http/src/lib.rs | modified | register both modules |
| crates/pc-http/src/routes/execution_workspaces.rs | modified | runtime_service_action gated via authz |
| crates/pc-http/src/routes/projects.rs | modified | workspace_runtime_action gated via authz |
| crates/pc-http/src/routes/workspace_runtime_service_authz.rs | modified | stub replaced with real authz matrix |
| crates/pc-repos/src/execution.rs | modified | added ExecutionRepo::company_id_for_workspace |

## Architecture

1. authz_loaders.rs — RuntimeServiceAuthzLoader with 7 pure-DB methods:
   - load_actor_agent / load_actor_run / load_run_issue
   - list_linked_scope_issues_for_project_workspace / for_execution_workspace
   - load_linked_assignee_issue_in_workspace
   - list_reporting_subtree_agent_ids (BFS over reports_to)
2. authz_runtime_service.rs — load_and_assert_runtime_service_manage:
   - Hydrates RuntimeServiceContext from DB + AuthContext
   - Calls assert_can_manage_* from pc_authz
   - map_authz_error_to_api for HTTP error mapping
3. HTTP endpoints now call the real helpers:
   - POST /api/companies/:id/execution-workspaces/:id/runtime-services/:action -> authz gate
   - POST /api/projects/:pid/workspaces/:wid/runtime/:action -> authz gate
   - GET /api/workspaces/:wid/runtime-service-authz -> real authz matrix

## Test results

cargo test -p pc-authz --lib              -> 85 passed
cargo test -p pc-http --lib authz          -> 7 passed
cargo test -p pc-http --lib                -> 401 passed
cargo test -p pc-repos --lib execution     -> 4 passed

Pre-existing failures (unrelated, confirmed on clean baseline):
- pc-plugin-host canonicalizes_existing_directory — needs /tmp writable
- pc-http access_http_contract board_key_create_* — DB FK on session.user_id

## Design decisions

1. Pure-function core stays in pc-authz — DB loaders live in pc-http so the
   authz decision engine has zero IO.
2. Single composition point — load_and_assert_runtime_service_manage hydrates
   the full context in one place; HTTP handlers stay thin.
3. Error mapping — RuntimeServiceAuthzError -> HTTP via map_authz_error_to_api.
4. Repos untouched for cross-crate safety — only added
   ExecutionRepo::company_id_for_workspace (needed for authz before enqueue).
