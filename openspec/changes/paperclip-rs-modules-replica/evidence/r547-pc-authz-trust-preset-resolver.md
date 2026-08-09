# R547 / M47 — pc-authz Trust preset + low-trust boundary 解析器

## 本轮完成

新增 `crates/pc-authz/src/trust.rs`，对齐 Node `services/trust-preset-resolver.ts` 的核心逻辑。

### 模块内容

- **`TrustPreset` enum**：`Standard` / `LowTrustReview`，对应原 `TRUST_PRESETS`
- **常量**：`LOW_TRUST_REVIEW_PRESET` / `LOW_TRUST_REVIEW_PRESET_VERSION` / `LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH` / `LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION`
- **`LowTrustBoundary` struct**（camelCase JSON）：`companyId / projectIds / rootIssueId / issueIds / allowedAgentIds / allowedSecretBindingIds / allowedToolClasses / outputPromotionTarget`
- **`TrustPresetResolution` enum**：`Standard { source_presets }` / `LowTrustReview { boundary, source_presets }` / `Denied { reason, source, detail, source_presets }`
- **`DenyReason` enum**：6 种 deny 原因（unsupported_trust_preset / invalid_authorization_policy / invalid_low_trust_boundary / cross_company_boundary / conflicting_low_trust_boundary / missing_low_trust_boundary_scope）
- **`resolve_core_trust_preset(input)`**：核心解析函数
- **`is_issue_within_boundary(boundary, issue_id, project_id, ancestors)`**：检查 issue 是否在 boundary 内
- **`is_agent_within_boundary(boundary, agent_id)`**：检查 agent
- **`is_tool_class_within_boundary(boundary, tool_class)`**：检查 tool class

### 决策逻辑

解析顺序：agent → project → issue → run
- 任何源配置 `low_trust_review` 即触发 low-trust 路径
- `low_trust_review` 必须配 `trustBoundary`，否则 deny `MissingLowTrustBoundaryScope`
- 跨公司 boundary → deny `CrossCompanyBoundary`
- 多个 source 的 boundary 合并（merge allowed_* 列表，union project_ids / issue_ids）
- 无 preset → standard

### 验证

- `cargo test -p pc-authz --lib`：**47 passed**（+14 个 trust preset 测试）
- `cargo test --workspace --lib -- --test-threads=1`：**4976 passed**（+14）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| 路由接入（12 个） | ✅ M42-M45 |
| e2e parity 测试 vs Node（22 + 6） | ✅ M46 |
| **Trust preset + low-trust boundary** | ✅ M47 |
| 全量接入所有受保护路由 | ⏳ 渐进 |

### Trust preset 测试覆盖（14）

| 测试 | 验证 |
|---|---|
| `trust_preset_round_trip` | 枚举字符串往返 |
| `resolve_defaults_to_standard_when_no_input` | 空输入 → standard |
| `resolve_low_trust_when_agent_has_preset` | agent preset → low_trust_review |
| `resolve_denies_cross_company_boundary` | 跨公司 deny |
| `resolve_denies_missing_boundary` | low_trust 无 boundary → deny |
| `resolve_merges_boundaries_from_multiple_sources` | 多 source boundary 合并 |
| `issue_within_boundary_via_issue_ids` | issue_ids 短路 |
| `issue_within_boundary_via_root_ancestor` | root 祖先链 |
| `issue_outside_boundary_is_denied` | 越界 deny |
| `agent_within_boundary_empty_allowed_means_open` | agent 列表为空 → 全开 |
| `agent_within_boundary_explicit_list` | agent 白名单 |
| `tool_class_within_boundary_empty_means_open` | tool class 列表为空 → 全开 |
| `tool_class_within_boundary_explicit_list` | tool class 白名单 |
| `boundary_serializes_and_deserializes` | JSON 双向 |
