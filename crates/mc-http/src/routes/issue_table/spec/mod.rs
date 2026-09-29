//! `/api/issues/table/*` 的请求 DTO 与「已校验规格」构造（从 `issue_table.rs` 拆出，R7 单文件
//! 800 行上限；`scripts/file_size_check.py` + 门 ⑩ 执行）。
//!
//! 上游对应：`issue_table_query.go` 的 `decodeIssueTableJSON` / `normalizeIssueTablePage` /
//! `canonicalIssueTableFingerprint`，以及 `issue_table_group.go` 的 `resolveIssueTableGroup`。
//!
//! 职责边界：**JSON 请求形状（上游字段名逐字一致）→ `mc_repos::issue_table` 的规格类型**，
//! 外加指纹（`query_fingerprint`）与分组 key / value 的解析。路由与 handler 在 `super`，
//! cursor 编解码在 `super::cursor`。与上游的有意偏离逐条见 `docs/14-M2-TABLE.md` §4，
//! 支持面见 §5。
//!
//! 文件布局（R7 拆文件，门 ⑩ 的 800 行上限）：
//! - `dto.rs`：422 辅助函数、两个上限常量与全部请求 DTO、`decode_body`
//! - `build.rs`：`normalize_*` / `parse_*` 与 `build_scope` / `build_filters` / `build_order` /
//!   `build_group_spec` / `build_facets`
//! - `fingerprint.rs`：`query_fingerprint`、`group_identity` / `group_value` / `parse_group_key`

mod build;
mod dto;
mod fingerprint;

// 这三组重导出把 `dto.rs` / `build.rs` / `fingerprint.rs` 里的 `pub(crate)` 项重新汇到
// `spec::`（可见性逐字不变，`tests.rs` 的 `use super::spec::*` 依赖这一层）。
// 必须逐条放行 `unused_imports`：`tests.rs` 走的是 glob 导入，lint 看不见 glob 里的
// 使用点，会把 `tests.rs` 真正用到的 DTO（`TableQueryDto` / `GroupDto` / `PageDto` …）
// 误报成死导出 —— 删掉就会编译不过（E0433 / E0422）。
#[allow(unused_imports)]
pub(crate) use build::{
    build_facets, build_filters, build_group_spec, build_order, build_scope, build_search,
    has_any_property, normalize_actor_type, normalize_page, parse_actor, parse_actors,
    parse_project_id, parse_rfc3339,
};
#[allow(unused_imports)]
pub(crate) use dto::{
    decode_body, unsupported_filter_err, unsupported_group_err, ActorDto, DateFilterDto,
    FacetSpecDto, FacetsRequest, FiltersDto, GroupDto, GroupsRequest, HierarchyDto, PageDto,
    RowsRequest, ScopeDto, SortDto, TableQueryDto, MAX_BODY_BYTES, STATUS_KEY_MAX_LEN,
};
#[allow(unused_imports)]
pub(crate) use fingerprint::{
    group_identity, group_value, parse_group_key, query_fingerprint, sorted_unique_actors,
    sorted_unique_strings,
};
