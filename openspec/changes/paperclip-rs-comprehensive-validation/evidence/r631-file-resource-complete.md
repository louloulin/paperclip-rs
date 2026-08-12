# R631 - file-resources Module Complete Replication

R631 covers the file-resources module, replicating the Node implementation.

## Changes

| File | Description |
|---|---|
| crates/pc-repos/src/file_resource.rs (607 LOC) | limiter + service trait + default impl + DbLike trait + error model |
| crates/pc-http/src/routes/file_resources.rs | HTTP route refactor to use service trait |
| crates/pc-http/src/error.rs | Added ApiError::TooManyRequests |
| crates/pc-repos/tests/r631_file_resource.rs | 17 integration tests (limiter + service mockable) |

## Node Alignment

| Component | Node | Rust |
|---|---|---|
| FileResourceLimiter (rate + concurrency) | yes | yes |
| WorkspaceFileResourceService trait | yes | yes |
| Default*Service impl | yes | yes |
| ListQuery 8 fields | yes | yes |
| ResolveQuery 4 fields | yes | yes |
| ContentResponse 6 fields | yes | yes |
| PrepareDownload | yes | yes |
| ApiError 429 mapping | yes | yes |

Total: ~715 LOC vs Node 722 LOC = 98.8% alignment.

## Design

- Pure logic limiter: sliding window + RAII release, no IO
- Service trait: all operations injected through trait
- DbLike trait: abstracts pc-repos::Db, mockable
- Single FileResourceError enum
- API 1:1 aligned with Node

## Validation

### Unit tests
6 passed, 0 failed.

### Integration tests
11 passed, 0 failed.

All tests use FakeDb mockable, no real DB needed.

### Compilation
0 errors.
