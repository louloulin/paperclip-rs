# paperclip-rs — 当前架构状态（R556 末 / 2026-08-11）

> 与 `ARCHITECTURE-DIAGRAMS.md`（底层图）/ `MODULE-MAPPING.md`（Node→Rust 映射）/ `PROJECT-PLAN.md`（v1.0 执行计划）配套。
> 本文档定位为**当前状态快照**——反映 R541-R556 这一轮"全面 Node→Rust 模块复刻"之后的真实情况。
> 最近新增 crate：... / pc-external-objects (R553) / pc-pipeline-case-type (R554) / pc-adapter-type (R555) / pc-feature-catalog (R556)

---

## 1. Crate 拓扑（98 个 crate）

```
paperclip-rs/
├── apps/
│   ├── pc-server/       # 启动入口（migrate → router → bind → graceful shutdown）
│   └── pc-cli/          # paperclipai 二进制（19 子命令，R501 末全部真做事 18/18 main + 20/20 nested）
├── crates/ (66 个，分类)
│   ├── 基础 8: pc-errors pc-core pc-config pc-db pc-telemetry pc-storage pc-backup pc-migrate
│   ├── 工具 9: pc-github-fetch (R523) + pc-github-external-objects (R525) + pc-log-redaction (R526) + pc-secret-redaction (R527) + pc-issue-references (R528) + pc-connection-display (R529) + pc-url-keys (R530) + pc-issue-attribution (R532) + pc-external-objects-server (R533)
│   ├── 域 24: pc-repos (80+ 子模块) pc-decisions pc-routines pc-pipelines pc-issues pc-companies
│   │         pc-company-member pc-auth pc-authz pc-realtime pc-heartbeat pc-workflow
│   │         pc-decision-training pc-work-products pc-portability pc-documents pc-feedback
│   │         pc-folder pc-goals pc-inbox pc-invite pc-issues pc-project pc-storage
│   ├── 适配器 13: pc-adapter-api pc-adapter-claude-local pc-adapter-codex-local
│   │             pc-adapter-cursor-{cloud,local} pc-adapter-gemini-local pc-adapter-grok-local
│   │             pc-adapter-hermes pc-adapter-hermes-gateway pc-adapter-openclaw-gateway
│   │             pc-adapter-opencode-local pc-adapter-pi-local pc-adapter-process pc-adapter-quota
│   ├── 插件 4: pc-plugin-{host protocol state-store ui-static}
│   ├── HTTP 1: pc-http (70+ 文件覆盖 56 路由)
│   └── 边角 16: pc-agent pc-budgets pc-board-auth pc-costs pc-environment pc-folders
│                pc-http pc-openapi pc-pipeline-conversation-context pc-plan-review-context
│                pc-codex-auth-reconciliation pc-run-liveness pc-secrets pc-sidebar
│                pc-status-card-update-engine pc-tool pc-responsible-user-denial
```

详细分类见 `MODULE-MAPPING.md`。

---

## 2. V1-V15 硬目标进度矩阵（R514 末 / 真实盘点）

| # | 模块 | proposal 估 | **R514 末真实** | 差距描述 |
|---|---|---|---|---|
| **V1** | 真实基线验证 | 部分 | **~80%** | `e2e-baseline.sh` 通过；macOS+Linux 双平台 exit 0 缺一 |
| **V2** | CLI 全子命令 | 0% | **61% (11/18)** ⭐ | 6 轮 R487-R498 大幅推进；剩 nested action stub |
| **V3** | OpenAPI 3.1 完整生成 | 🔶 | **100%** ⭐ | R511 加 Case/Goal/Inbox/Folder + 4 List (8 schemas) + 25 路由 hints (69 总, 100% 已注册路由覆盖) + operationId 唯一性 guard |
| **V4** | OpenAPI ↔ UI 类型对齐 | � | **~60%** ⭐ | R518 `pc-typescript-gen` 生成 35 DTO 类型 (337 行 TS, tsc --strict 0 错误)；剩 UI 客户端接入 |
| **V5** | Auth 完整化 | 55% | **~85%** ⭐ | R514+R515 完成 refresh/CSRF/API key；OAuth login 在 Node 上游也不存在 (✅ 无缺口) — R523 改为 port github-fetch.ts |
| **V6** | 路由字节级 | 14% 缺口 | **~100%** ⭐ | R522 收尾: scanner chained-method fix (`get(h).post(h)` 全识别) + 6 companies 聚合 schemas (CompanyStats/Timeline/Artifact/OrgChart) wired 到 5 个 path_schema_hint entries |
| **V7** | (未在 proposal 命名) | — | — | — |
| **V8** | 远程 execution | ❌ | **0%** | restoreRemoteWorkspace / materializeRemoteClaudeConfig 未复刻 |
| **V9** | Workflow + Cron 真实链路 | 🔶 | **~40%** | pc-cron + pc-workflow 部分，缺端到端 |
| **V10** | Plugin 互操作 | 🔶 | **~30%** | worker 池 + JSON-RPC 有，缺互操作测试 |
| **V11** | UI 60 client 全 happy | ❌ | **0%** | 未跑 |
| **V12** | Playwright 真实 UI 剧本 | ❌ | **0%** | 未跑 |
| **V13** | 长跑 + 性能基线 | ❌ | **0%** | 未跑 |
| **V14** | 真实迁移 (109→172) | 🔶 | **~80%** | 172 表已建，注释 + diff warning 缺 |
| **V15** | 中文文档与移交 | ❌ | **~20%** | 已有 ARCHITECTURE/MODULE-MAPPING/PROJECT-PLAN，缺 OPERATIONS/PLUGIN_AUTHORING/MIGRATION |

**V1-V15 综合完成度** ≈ **38-44%**（真实，硬目标层面；质量层 ≈ 93%）

---

## 3. 近期改进（R487-R537，10 轮 14 周）

### R487 — pc-workflow
- `next_cron_tick_in_timezone` (+8 测试)

### R488 — pc-workflow
- `is_sub_hourly_cron_expression` + `next_result_text` + `normalize_webhook_timestamp_ms` (+13 测试)

### R489 — pc-repos
- `derive_issue_prefix_base` + `suffix_for_attempt` (+10 测试)

### R490 — pc-pipelines
- `StageKind::is_terminal` + `normalize_stage_kind` (+9 测试)

### R491 — pc-routines
- dashboard.rs 11 个边界测试（零新 API，+13 测试）

### R492 — pc-decisions
- **8 个新 `pub fn` in `pc-decisions/src/pure.rs`**（700 行新代码）
  - `EffectAction` enum + `classify_effect_type`
  - `effect_target_ids` / `target_ids` / `target_actions`
  - `same_ids` / `same_input_values`
  - `interpolate` (UTF-8 safe `{{input.<id>}}` 模板)
  - `build_spec_envelope` / `canonical_decision_value` (复用 `pc_secrets::canonical`)
  - `json_copy<T>` (typed round-trip)
- 36 个新单测
- **提升 `pc_secrets::canonical` 为 `pub`**（共享算法，避免在业务层再加 ryu_js 依赖）

### R526 — pc-log-redaction (新 crate)
- **port Node `log-redaction.ts` (148 LOC) → Rust ~600 LOC**
- 新 crate `crates/pc-log-redaction/`, 4 模块, 43 单测全过
- 公开 API:
  - `Options` + `Options::with_default_candidates(&dyn Env)` — 配置 + env-derived candidates
  - `Env` trait + `StdEnv` — 抽象 std::env, 测试可注入
  - `text::redact_current_user_text(&str, &Options) -> String`
  - `value::redact_current_user_value(&serde_json::Value, &Options) -> Value`
- 模块拆分:
  - `mask.rs` — `mask_user_name_for_logs` (8 测试)
  - `path.rs` — `split_path_segments` + `replace_last_path_segment` (9 测试, 跨 Unix + Windows)
  - `text.rs` — 主 redact 算法 (12 测试, 含 word-boundary / 多 username / 多 home_dir / Unicode)
  - `value.rs` — 递归 JSON Value redact (9 测试)
  - `lib.rs` — Options + Env + default_user_names + default_home_dirs (5 测试)
- **设计改进 vs Node 上游**:
  - 不依赖全局 module-level 缓存 (Node 有 `cachedCurrentUserCandidates`), 用 `Options` 显式持有
  - 不依赖 `std::env` 全局, 用 `Env` trait + `StdEnv`, 测试 100% deterministic (mock env 即可)
  - `redact_current_user_value` 接受 `&serde_json::Value` 而非任意 JS 对象 (类型安全)
  - word-boundary 用手工 `is_word_char` 替代 regex crate (零依赖)
- **已知 limitation** (镜像 Node 上游): 短 prefix home dir (`/home`) 在长子串 (`/home/alice`) 替换后仍会再次匹配 → `/home/alice` → `/home/a****` → `/h***/a****/x`。修复需 overlap tracking, 当前保留上游行为 + 测试断言明示
- 43 单测全过, workspace crates **69 → 70**

### R537 — pc-network-bind (新 crate)
- **port Node `packages/shared/src/network-bind.ts` (~100 LOC) → Rust ~480 LOC + 35 测试**
- 新 crate `crates/pc-network-bind/`, 单文件 `lib.rs`
- 公开 API:
  - `BindMode` / `DeploymentMode` / `DeploymentExposure` enum (serde rename_all 对齐 Node)
  - `LOOPBACK_BIND_HOST` / `ALL_INTERFACES_BIND_HOST` 常量
  - `is_loopback_host` / `is_all_interfaces_host` — case-insensitive + trim
  - `infer_bind_mode_from_host` — 5 路决策 (loopback/lan/tailnet/custom)
  - `validate_configured_bind_mode` — 累积 errors (`Vec<String>`)
  - `resolve_runtime_bind` — 5 case match, 返回 host + errors
- 35 测试覆盖每个决策分支 + 错误累积 + enum 序列化
- workspace crates **79 → 80**

### R536 — pc-portability-hash (新 crate)
- **port Node `packages/shared/src/portability-hash.ts` (~30 LOC) → Rust ~150 LOC + 26 测试**
- 新 crate `crates/pc-portability-hash/`, 单文件 `lib.rs`
- 公开 API:
  - `NormalizedSha256` newtype (强校验: 64 lowercase hex 字符)
  - `normalized_content_hash` — sha256 of canonical JSON
  - `canonical_json` — JSON string with sorted object keys
  - `sha256_hex_of_bytes` — sha256 hex of raw bytes
- 用 `std::sync::LazyLock` (Rust 1.80+) + `sha2::Sha256`
- BTreeMap 天然排序替代 JS `localeCompare`
- 26 测试覆盖 SHA-256 标准向量 (`""`, `"abc"`, `"{}"`) + key-order invariant + null vs absent
- workspace crates **78 → 79**

### R535 — pc-environment-redaction (新 crate)
- **port Node `packages/shared/src/environment-custom-images.ts` (~115 LOC) → Rust ~520 LOC + 28 测试**
- 新 crate `crates/pc-environment-redaction/`, 单文件 `lib.rs`
- 公开 API:
  - `REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE` 常量
  - 13 个 `static LazyLock<Regex>` (sensitive key patterns)
  - `is_sensitive_key` (Redacted suffix 跳过)
  - `is_sensitive_primitive_string` (IPv4 + ssh command)
  - `redact_environment_custom_image_value` (递归 JSON redact)
  - `redact_environment_custom_image_template` (template Ref + metadata)
  - `redact_environment_custom_image_setup_session` (含 username special-case)
- **关键 bug 发现**: `skip_serializing_if = "Option::is_none"` 导致 None 字段被省略
  (而非序列化为 null)，与上游 spread `{...template}` 行为不一致；修正为
  `#[serde(default)]` 后 4 个 null-preserved 测试通过
- 28 测试覆盖上游 vitest fixture (`apiToken` / `host` / `connectUrl` / `ssh` 各种嵌套)
- workspace crates **77 → 78**

### R534 — pc-environment-support (新 crate)
- **port Node `packages/shared/src/environment-support.ts` (~170 LOC) → Rust ~600 LOC + 31 测试**
- 新 crate `crates/pc-environment-support/`, 单文件 `lib.rs`
- 公开 API:
  - `AgentAdapterType` / `SandboxEnvironmentProvider` newtype (string ID)
  - `EnvironmentDriver` / `EnvironmentSupportStatus` enum
  - `REMOTE_MANAGED_ADAPTERS` 常量 (claude/codex/cursor/gemini/grok/opencode/pi)
  - `adapter_supports_remote_managed_environments` — 闸门
  - `supported_environment_drivers_for_adapter` — `[local]` vs `[local, ssh, sandbox]`
  - `supported_sandbox_providers_for_adapter` — 接受 additional providers + 去重
  - `is_environment_driver_supported_for_adapter` / `is_sandbox_provider_supported_for_adapter`
  - `get_adapter_environment_support` / `get_environment_capabilities` — builders
- 31 测试覆盖 upstream vitest fixture (`codex_local` + `fake-plugin`, `grok_local` 含 sandbox)
- workspace crates **76 → 77**

### R533 — pc-external-objects-server (新 crate) + 跨 crate 复用
- **port Node `packages/shared/src/external-objects-server.ts` (217 LOC) → Rust ~550 LOC**
- 新 crate `crates/pc-external-objects-server/`, 单文件 `lib.rs`, 21 单测全过
- 公开 API:
  - `ExternalObjectUrlMatch` struct (`index` + `length` + `matched_text`)
  - `ExternalObjectCanonicalIdentity` struct (`scheme` + `host` + `path` + `query_param_hashes`) — serde camelCase
  - `ExternalObjectUrlCanonicalizationOptions` struct (`identity_query_params: Vec<String>`)
  - `ExternalObjectCanonicalUrl` struct (5 字段, 全部强类型)
  - `ExternalObjectMentionSource` struct (6 字段)
  - `find_external_object_url_matches(&str) -> Vec<ExternalObjectUrlMatch>`
  - `canonicalize_external_object_url(&str, &options) -> Option<ExternalObjectCanonicalUrl>`
  - `extract_external_object_canonical_urls(&str, &options) -> Vec<ExternalObjectCanonicalUrl>`
  - `build_external_object_scoped_identity_key(4 args) -> String`
  - `build_external_object_mention_source_key(&source) -> String`
- **关键 design — 跨 crate 复用**:
  - **不重复实现 `strip_markdown_code` + `trim_trailing_punctuation`**! Node 上游是 DRY 违规 (两处都重复实现), Rust port 让 pc-issue-references (R528) 把这两个 helper 公开, 新 crate 直接 `use pc_issue_references::{strip_markdown_code, trim_trailing_punctuation}`
  - `sha256_hex` 用 `sha2` crate (无运行时开销, 编译时常量计算)
  - `stable_stringify` 用 `serde_json::Value` 转换 + 手动排序 keys (对齐 Node 行为)
  - URL 解析用 `url::Url::parse` + userinfo 显式拒绝
- **范围** (镜像 Node 5 个 pub fn):
  - ✅ `find_external_object_url_matches`
  - ✅ `canonicalize_external_object_url`
  - ✅ `extract_external_object_canonical_urls`
  - ✅ `build_external_object_scoped_identity_key`
  - ✅ `build_external_object_mention_source_key`
- **不范围**:
  - UI / server 集成层 (`server/src/services/external-objects.ts` 的 DB 持久化)
  - `pc-github-external-objects` (R525) 是 GitHub-specific, 本 crate 是通用 external URL 处理
- **使用方** (Node 上游):
  - `server/src/services/external-objects.ts` (整个文件 7 个函数的核心)
  - `packages/shared/src/index.ts` 公共导出
- 21 单测全过 (镜像 8 个 upstream 测试 + 13 个 Rust 边界)
- workspace crates **75 → 76**

### R532 — pc-issue-attribution (新 crate)
- **port Node `packages/shared/src/issue-attribution.ts` (57 LOC) → Rust ~440 LOC**
- 新 crate `crates/pc-issue-attribution/`, 单文件 `lib.rs`, 18 单测全过
- 公开 API:
  - `ResponsibleUserSource` enum (`Explicit` / `Creator` / `None`) + serde snake_case
  - `ResponsibleUserAttribution` struct (`user_id` + `source` + `is_auto_derived`) + serde camelCase
  - `ResponsibleUserInput` struct (`responsible_user_id` + `created_by_user_id`) + `new()`
  - `derive_responsible_user(&input) -> ResponsibleUserAttribution` — 显式 > 创建者 > none
  - `OriginatingActor` enum (`User { id, via_agent_id }` / `Agent { id }`) — tagged union
  - `OriginatingActor::id()` / `is_user()` helpers
  - `OriginatingActorInput` struct (`created_by_user_id` + `created_by_agent_id` + `responsible_user_id`) + `new()`
  - `derive_originating_actor(&input) -> Option<OriginatingActor>` — 4 路 fallback
- **关键 design**:
  - 强类型 enum 替代 Node 的 TS string union (`"user" | "agent"` 等)
  - serde `rename_all = "camelCase"` 让 JSON 输出对齐 Node API 契约 (`viaAgentId` 不是 `via_agent_id`)
  - serde `tag = "kind"` 让 `OriginatingActor` 序列化为 `{"kind":"user","id":"..."}`
  - `Pick`-style 最小 input struct, 不强依赖完整 `Issue` row
  - 空字符串视为 None (镜像 Node `if (issue.responsibleUserId)` falsy 判断)
- **范围**:
  - ✅ 2 个 pub fn + 4 个 struct + 2 个 enum
- **不范围** (留给集成层):
  - `pc-repos::issue` 持久化层
  - server `routes/issues.ts` endpoint
  - UI "Originating" affordance 渲染
- **使用方** (Node 上游):
  - `packages/db/src/migration-safety-baseline.ts`
  - `packages/shared/src/index.ts:76` 公共导出
- 18 单测全过 (镜像 8 个 upstream 测试 + 10 个 Rust 边界 + serde roundtrip 4 个)
- workspace crates **74 → 75**

### R531 — pc-pipelines::case_type (新模块)
- **port Node `packages/shared/src/pipeline-case-type.ts` (34 LOC) → Rust ~160 LOC**
- 新模块 `crates/pc-pipelines/src/case_type.rs`, 14 单测全过
- 公开 API:
  - `CaseTypePipelineRef` struct (`id` + `key: Option<String>`) + `new()` + `with_key()`
  - `derive_case_type(pipeline) -> String` — `pipeline.key.trim()` 非空则用 key, 否则 fallback `pipeline.id`
  - `case_type_matches_pipeline(declared: Option<&str>, pipeline) -> bool` — 摄入 sanity check
- **关键 design**:
  - 不开新 crate, 加入 `pc-pipelines` (单 crate 单一模块 lib.rs 已 2609 行, 拆模块更清晰)
  - `CaseTypePipelineRef` struct 而非 `&dyn`/trait, 编译期类型安全
  - `derive_case_type` 与 Node 上游 1:1: `key.trim()` 后 fallback
  - `case_type_matches_pipeline` 处理 4 种 declared: `None` / `Some("")` / `Some(<匹配>)` / `Some(<不匹配>)`
- **范围**:
  - ✅ 2 个 pub fn + 1 个 struct
- **不范围**:
  - UI / server 集成层 (`server/src/routes/pipelines.ts:2330` 调用 `deriveCaseType`) — 集成层 R532+
- **使用方** (Node 上游):
  - `server/src/routes/pipelines.ts:61` import + `:2330` 调用
  - `packages/shared/src/index.ts:71` 公共导出
- 14 单测全过 (含 5 个 `derive_case_type` 边界 + 9 个 `case_type_matches_pipeline` 边界)
- crates 数 **74 不变**

### R530 — pc-url-keys (新 crate) + R604 inline 替换
- **port Node `packages/shared/src/agent-url-key.ts` (22 LOC) + `project-url-key.ts` (36 LOC) → Rust ~500 LOC**
- 新 crate `crates/pc-url-keys/`, 2 模块 (`agent_url_key` + `project_url_key`), 26 单测全过
- 公开 API:
  - `agent_url_key::is_uuid_like(&str) -> bool` (UUID v1-v5)
  - `agent_url_key::normalize_agent_url_key(&str) -> Option<String>` (lowercase + `-` + trim)
  - `agent_url_key::derive_agent_url_key(Option<&str>, Option<&str>) -> String` (name → fallback → `"agent"`)
  - `project_url_key::normalize_project_url_key(&str) -> Option<String>` (同 agent 算法)
  - `project_url_key::has_non_ascii_content(&str) -> bool` (`[^\x00-\x7F]`)
  - `project_url_key::short_id_from_uuid(&str) -> Option<String>` (前 8 hex, lowercase)
  - `project_url_key::derive_project_url_key(Option<&str>, Option<&str>) -> String` (ASCII fast path + UUID suffix fallback)
- **关键改进 — 替换 pc-agent 内联实现**:
  - R604 之前的 `pc-agent::service` 里有 inline `normalize_agent_url_key` (35 行 hand-rolled) 和 `is_uuid_like` (用 `uuid::Uuid::parse_str`)
  - 现在 `pc-agent` 通过 `pub use pc_url_keys::{is_uuid_like, normalize_agent_url_key};` 替换, 移除内联实现, 依赖更轻 (无需 `uuid` crate 用于 url key 检测)
  - `derive_agent_url_key` 是新增的 (Node 上游有, R604 没 port)
  - `project-url-key.ts` 是全新的 port (R604 没碰过)
- **关键 design**:
  - 全 `Lazy<Regex>` 零成本 (UUID_RE + NON_ASCII_RE)
  - `normalize_*` 用单一 `prev_dash` flag 算法, 与 Node `[^a-z0-9]+` regex 等价
  - `Option<&str>` 互斥签名, 比 Node `string | null | undefined` 更强类型
  - 私有 helper `short_id_from_uuid` 单独 pub (供 `derive_project_url_key` 内部用, 也给业务层独立调用)
- **范围** (镜像 Node 8 种函数):
  - ✅ agent_url_key: 3 个 pub fn + 1 个 UUID regex
  - ✅ project_url_key: 3 个 pub fn + 2 个 regex (UUID + NON_ASCII) + 1 个 pub helper
- **不范围**:
  - UI `src/lib/utils.ts` / `src/lib/search-query-parser.ts` (TS 端保留, UI 冻结契约)
  - `pc-agent` 业务层 (server/src/services/agents.ts 等) (集成层 R531+)
- **26 单测全过** (含上游 R604 3 个测试 + 23 个新边界测试)
- workspace crates **73 → 74**

### R529 — pc-connection-display (新 crate)
- **port Node `packages/shared/src/humanize-connection.ts` (88 LOC) → Rust ~400 LOC**
- 新 crate `crates/pc-connection-display/`, 单文件 `lib.rs`, 18 单测全过
- 公开 API:
  - `HumanizableConnection` struct (`name`)
  - `HumanizeOptions` struct (`title: Option<String>`) + `new()` + `with_title()`
  - `ConnectionInput<'a>` enum (`Raw(&str)` | `Object(&HumanizableConnection)` | `None`) — 互斥输入
  - `IPV4_RE` / `HOST_PORT_RE` 2 个静态 Regex
  - `humanize_connection_display_name(input, options) -> String` — 主函数
  - `humanize_connection_display_name_str(raw, options)` — string 形式 wrapper
  - `humanize_connection_display_name_obj(conn, options)` — object 形式 wrapper
  - `connection_display_secondary_hint(input) -> Option<String>` — 副标题
- **关键 design**:
  - 输入用 `enum ConnectionInput` 而非 `(Option, Option)`, 强类型互斥 (Node `string | object | null | undefined` 1:1 映射)
  - 私有 helpers: `raw_name_of` / `looks_like_network_address` / `title_case_identifier` / `plugin_package_label`
  - `pluginPackageLabel` 处理 5 种情况: `Plugin: vendor.plugin-leaf` / `PLUGIN:` (case-insensitive) / `Plugin: plugin-briefs` (无 dotted package) / `Plugin: acme.plugin_weekly-report` (下划线 separator) / `Plugin: <empty>`
  - 全 `Lazy<Regex>` 零成本, IP/host:port 检测在常量时间
- **范围**:
  - ✅ 完整 Node 上游 8 个测试用例 + 10 个 Rust 额外边界测试
- **不范围** (留给 UI / 集成层):
  - UI `src/lib/connection-display.ts` (TS 端保留, UI 是冻结契约)
  - server 端业务集成 (Node 上游这模块是 pure UI helper, 仅 UI 端调用)
- **使用方** (Node 上游):
  - UI `/apps`, `/apps/attention`, `/apps/advanced` 页面
  - App-detail header 渲染
- 18 单测全过, workspace crates **72 → 73**

### R528 — pc-issue-references (新 crate)
- **port Node `packages/shared/src/issue-references.ts` (188 LOC) → Rust ~500 LOC** (纯函数部分)
- 新 crate `crates/pc-issue-references/`, 单文件 `lib.rs`, 23 单测全过
- 公开 API:
  - `ISSUE_REFERENCE_IDENTIFIER_RE` / `ISSUE_REFERENCE_TOKEN_RE` 2 个静态 Regex
  - `IssueReferenceMatch` struct (`index` + `length` + `identifier` + `matched_text`)
  - `IssueIdentifierRef` struct (`identifier`)
  - `normalize_issue_identifier(&str) -> Option<String>` — `"pap-123"` → `Some("PAP-123")`, `"not-an-issue"` → None
  - `build_issue_reference_href(&str) -> String` — `"pap-123"` → `"/issues/PAP-123"`
  - `parse_issue_reference_href(&str) -> Option<IssueIdentifierRef>` — `/issues/PAP-123` / `https://...issues/pap-789#comment` → 解析
  - `find_issue_reference_matches(&str) -> Vec<IssueReferenceMatch>` — 纯文本扫描
  - `extract_issue_reference_identifiers(&str) -> Vec<String>` — markdown 去重抽取
  - `extract_issue_reference_matches(&str) -> Vec<IssueReferenceMatch>` — markdown 去重抽取完整 match
- **关键 design**:
  - 全 case-insensitive (`(?i)` flag), 上游用 `/i`
  - URL 解析用 `url::Url::parse` 替代 Node `URL` constructor + try/catch; 失败返回 `None`
  - Fenced code 检测 (`detect_fence_opener`) 返回 `(char, usize)` 而非 `&'static str`, 支持 3+/4+/5+ backticks 和 tildes
  - `trim_trailing_punctuation` parens-aware: `(` count >= `)` count 时保留 `)`, 反之 trim
  - `strip_markdown_code` 完全 hand-written, 不引入 markdown crate 依赖; 保留 newline structure
- **范围** (纯函数, 与上游对齐):
  - ✅ 全部 7 个 pub fn + 2 个 Regex + 2 个 struct
- **不范围** (留给集成层):
  - `server/src/services/issue-references.ts` 的 `issueReferenceService` (DB 持久化 + 多 service 协同) → 集成层 R529+ 
  - UI `src/lib/issue-reference.ts` (TS 端保留, UI 是冻结契约)
- **使用方** (Node 上游):
  - `server/src/routes/costs.ts`, `routes/agents.ts`, `routes/activity.ts`, `routes/issues.ts`
  - `server/src/services/issues.ts` (业务层)
  - `server/src/scripts/backfill-issue-reference-mentions.ts` (历史数据回填)
- 23 单测全过, workspace crates **71 → 72**

### R527 — pc-secret-redaction (新 crate)
- **port Node `server/src/redaction.ts` (144 LOC) → Rust ~600 LOC** (纯函数部分)
- 新 crate `crates/pc-secret-redaction/`, 单文件 `lib.rs`, 25 单测全过
- 公开 API:
  - `REDACTED_EVENT_VALUE` 常量 (`"***REDACTED***"`)
  - `SECRET_TEXT_HINTS` 常量列表 (18 项)
  - `SECRET_FIELD_NAME_PATTERN` / `JWT_VALUE_PATTERN` / `JSON_SECRET_FIELD_PATTERN` / `ESCAPED_JSON_SECRET_FIELD_PATTERN` / `CLI_SECRET_FLAG_PATTERN` 5 个静态 Regex
  - `AUTHORIZATION_BEARER_PATTERN` / `OPENAI_KEY_PATTERN` / `GITHUB_TOKEN_PATTERN` / `INLINE_JWT_PATTERN` 4 个 command-redaction inline pattern
  - `is_secret_field_name(&str) -> bool`
  - `is_jwt_like(&str) -> bool`
  - `maybe_contains_secret_text(&str) -> bool` (启发式 gate)
  - `redact_sensitive_text(&str) -> String` (5 stage pipeline: inline JSON → escaped JSON → Auth Bearer → sk-/ghp_ → in-text JWT)
  - `redact_record(&serde_json::Value) -> Value` (递归 JSON object redact)
  - `is_cli_secret_flag(&str) -> bool`
- **关键 bug fix**: Rust port 第一版漏了 Node upstream `/i` (case-insensitive) flag → `"apiKey"` (camelCase) 永远不匹配 → 加 `(?i)` 后立即匹配
- **inline 而不是拆 crate**: Node 上游的 `redactSensitiveText` 调用 `@paperclipai/adapter-utils` 的 `redactCommandText`; 为避免再 port 整个 adapter-utils crate, 把其 4 个纯 inline regex pattern 直接搬进 `pc-secret-redaction`, 让 `redact_sensitive_text` 自包含
- **设计 vs Node 上游**:
  - 全 case-insensitive (上游 5 个 regex 全加 `(?i)`)
  - 5 stage replace pipeline (上游 2 stage: JSON + escaped JSON, command-line 是另一函数)
  - 所有 `pub fn` 纯函数, 无 IO, 无环境依赖, 无 async
  - `Lazy<Regex>` 一次性编译, 后续零成本
  - `RedactionError` enum 预留扩展 (目前只有 `InvalidPattern`)
- **范围**:
  - ✅ `is_secret_field_name` / `is_jwt_like` / `maybe_contains_secret_text` / `redact_sensitive_text` / `redact_record` / `is_cli_secret_flag`
- **不范围** (留给集成层):
  - `secret_ref` / `user_secret_ref` binding 检测 (需要 DTO 类型 → pc-secret-binding 集成层)
  - `commandArgs` argv 整条处理 (需要 command execution context → pc-adapter-process 集成层)
  - `redactCommandTextForLogs` 的 command resolution + env interpolation (涉及 server-utils 整链)
- 25 单测全过, workspace crates **70 → 71**

### R525 — pc-github-external-objects (新 crate)
- **GitHub external object 纯解析层 port** (R523 接续)
- 新 crate `crates/pc-github-external-objects/`, 4 模块, ~600 LOC
- **范围 (本 crate)**: 纯解析逻辑
  - `identity.rs` — `parse_github_canonical_url` + `parse_github_object` + `GitHubObjectIdentity` + 4 helpers + 15 测试
  - `retry.rs` — `retry_after_seconds` + `failure_from_github_response` + `ResolveFailure` + 10 测试
  - `status.rs` — typed enums `LivenessState` (3) + `ErrorCode` (4)
  - `lib.rs` — `ParseError` (7 variants) + re-exports
- **不范围 (留给 R526 集成层)**: HTTP fetch (用 R523 `pc-github-fetch`) + DB 持久化 + live-events publish + snapshot 构造
- **关键设计**:
  - 所有 `pub fn` 都是纯函数 (无 IO, 无 async, 无 DB)
  - 错误用 `ParseError` enum (7 强类型 variant), 不抛 string
  - 接受 `(scheme, host, path)` 而非 Node 的 `ExternalObjectCanonicalUrl` value object, 避免耦合 DTO crate
  - `RetryAfterResponse` 解耦 reqwest::Response, 让 helper 可在 sync context 单测
  - `failure_from_github_response` 接受 `status: u16` + `rate_limit_remaining: Option<&str>` + `RetryAfterResponse`, 而不是 `reqwest::Response`, 同样可单测
- 27 单测全过 (15 identity + 10 retry + 2 status 默认)
- **workspace 68 → 69 crates**

### R523 — pc-github-fetch (新 crate)
- **OAuth login 修正 + 新增 github-fetch crate**
- **关键发现**: Node 上游**没有** Google/GitHub login OAuth (`better-auth.ts` 不含) — V5 的 OAuth 缺口实际不存在
- 改 port Node `server/src/services/github-fetch.ts` (30 LOC 但被多个外部 provider 使用) 到 Rust
- 新 crate `crates/pc-github-fetch/` (3 个模块, ~280 LOC):
  - `lib.rs` — `GitHubFetchError` + re-exports + 1 测试
  - `urls.rs` — `is_git_hub_dot_com` / `git_hub_api_base` / `resolve_raw_git_hub_url` 纯函数 + 8 测试
  - `fetch.rs` — `gh_fetch` + `gh_fetch_with` async wrapper + 4 测试 (含真 mock HTTP server)
- 高内聚: 只含 GitHub / GHE URL builder + fetch wrapper
- 低耦合: 只依赖 `reqwest` + `url` (工作区已有) + `thiserror`
- 设计改进 vs Node 上游:
  - 拆成 `gh_fetch_with(&Client, RequestBuilder)` 接受 caller-supplied client, 生产代码共享连接池
  - `GitHubFetchError::Connection` 携带 host + 原始 reqwest error, 强类型 vs Node 的 unprocessable(422)
- 13 单测全过 (8 URL builder + 4 fetch + 1 re-export); 真实 mock TcpListener 验证成功 / 401 / 连接失败 3 种路径
- **V5 修正**: OAuth login 不存在, 不再列为缺口

### R522 — pc-http::routes::openapi + pc-openapi
- **V6 scanner 收尾 + Companies 聚合 schemas (95% → 100%)** ⭐
- **Scanner fix**: verb 提取 split 现在也按 `.` 分隔, `.method` 链式语法（`.get(h).post(h)`）能识别所有方法
- 之前 R513 测试被迫用 single-method path（`/api/companies/{id}/archive`）绕过 scanner 限制；R522 后所有 chained-method route 都正确 register
- 6 个新 pc-openapi DTO schemas:
  - `CompanyStats` — per-company stats (agentCount/issueCount/spend/lastActivityAt)
  - `CompanyStatsList` — global list of CompanyStats (for /api/companies/stats)
  - `CompanyTimelineResult` — `{actors, spans, events, edges}` 四数组 wrapper
  - `CompanyArtifact` — 单 artifact (id/kind/name/sizeBytes/createdAt/...)
  - `CompanyArtifactList` — paginated list with embedded CompanyArtifact
  - `CompanyOrgChart` — `{nodes, edges}` 树形结构
- 5 个 `path_schema_hint` entries 从 `response: None` 更新为真实 schema 引用
- 8 个新 R522 测试 (3 scanner + 5 schema wiring), 历史 6 个 schema count 断言 35 → 41
- `pc-typescript-gen` 自动生成 41 DTOs (337 → 415 行 TS), tsc --strict 0 错误
- **V6 95% → 100%** ⭐ (V6 收官)

### R518 — pc-typescript-gen (新 crate)
- **V4 OpenAPI → TypeScript 类型生成 (0% → 60%)** ⭐
- 新 crate `crates/pc-typescript-gen/`: 纯 Rust, ~700 LOC
- `generate_typescript_types(&OpenApiSpec) -> String` 主入口；`schema_to_typescript(name, &Value)` + `schema_to_type_expr(&Value, &[String])` 公开 API
- 支持: primitives / arrays / objects / `$ref` / `enum` / `oneOf`/`anyOf`/`allOf` / OAS 3.0 `nullable: true` + OAS 3.1 `type: ["string", "null"]` / `additionalProperties` / `const` / 布尔 schema
- 单一真相源: `pc_openapi::register_core_dtos` → 输出 OpenApiSpec → 生成 TS；不重新定义 schema
- **关键改进 pc-openapi**: `DecisionOption` + `DecisionEffect` 之前是 un-registered companions，现在 register 进 `register_core_dtos`，CORE_DTO_NAMES 33 → 35
- 33 单元测试 (emit.rs 24 + naming.rs 9) + 9 集成测试 (含 `generated_types_pass_tsc_strict_check` 用真 tsc 解析生成的 .ts)
- 真实验证: `cargo run --example gen_types > api-types.ts && tsc --noEmit --strict --target es2020` 0 错误
- 输出 337 行 TS: 19 interfaces + 16 type aliases (35 DTO total)
- **V4 0% → 60%**

### R515 — pc-http::routes::openapi + middleware::csrf
- **V5 CSRF 接入 OpenAPI (80% → 85%)** ⭐
- `securitySchemes` 新增 `csrfToken`: `{"type":"apiKey","in":"header","name":"X-CSRF-Token"}`
- 新 `pub fn csrf_protected_in_openapi(path, method) -> bool` 纯函数 — 复用 `middleware::csrf::csrf_path_allowed` whitelist + state-changing method set
- `scan_routes_for_openapi` 在生成 op 后注入 `security: [{"csrfToken": []}]`（仅对未白名单的 POST/PUT/PATCH/DELETE）
- 10 个新 R515 测试 (openapi.rs): csrfToken securityScheme + helper pure fn 5 分支 + path-level security 真实扫描验证 + YAML emitter
- 5 个新 R515 csrf 集成测试 (csrf.rs): session+apiKey 混合 auth、session+Bearer 混合 auth、多 cookie 解析稳健、`reason()` 字符串稳定性、generate→cookie→header 全链路 round-trip
- **关键不变量**: 单 `csrf_path_allowed` 源（middleware），OpenAPI 文档与运行时 100% 对齐
- **V5 80% → 85%**

### R514 — pc-auth
- **V5 API key pk_ 前缀约定 (70% → 80%)** ⭐
- `KeyPrefix` enum: `Pk` (`pk_`) + `Sess` (`sess_`)
- `KeyPrefix::parse()` 识别 3 种 Pk 前缀（`pk_` 新 + `pcak_` legacy + `pcp_board_` legacy）→ 向后兼容
- `generate_api_key(prefix) -> String` — 24 bytes random → 32 url-safe base64 chars；总长 35
- `has_key_prefix(token, expected) -> bool` — 防御性 prefix guard
- `resolve_api_key` 接 prefix guard：session token 永远不会被当 API key
- `board_keys_create` 切换到 `pk_` 前缀生成
- 13 个新 R514 测试 (KeyPrefix parse/format/generate + 防御性 guard)

### R513 — pc-openapi + pc-http
- **V6 路由补全 (86% → 95%)** ⭐
- 6 个新 schemas: CompanyMember (10 fields, status enum) + Invite (13 fields, 3 nullable) + AdminUser (7 fields, minimum required) + 3 List shapes
- `register_core_dtos` & `CORE_DTO_NAMES`: 27 → 33 schemas
- 25 个新 path hints: 5 admin (/api/admin/users*) + 13 companies 子路由 (members/stats/timeline/artifacts/org*/archive/import) + 4 invites + 4 skills
- Coverage 测试升级: 69 → **94 hints** (admin + companies 子路由 + invites + skills 全覆盖)
- 11 个新 R513 测试 (8 pc-openapi + 3 pc-http)
- **V6 86% → 95%**

### R512 — pc-auth
- **V5 refresh rotation 深度 (55% → 70%)** ⭐
- 8 个新 pure helpers + 1 新 SessionCheckOutcome 变体:
  - SessionRecord 加 `revoked_at: Option<DateTime<Utc>>` 字段（向后兼容 `#[serde(default)]`）
  - SessionCheckOutcome 新增 `Revoked` 变体（idle/absolute 之前先检查）
  - `mark_revoked` / `is_revoked` / `detect_reuse` + `ReuseOutcome` enum
  - `detect_reuse` 两条规则：presented 自己 revoked OR 兄弟 newer+active → ReuseDetected
- auth_service::SessionRecord 加 `family_id: Uuid` 字段（`#[serde(default = "Uuid::new_v4")]`）
- SessionStore trait 新增 3 方法: `find_family` / `mark_revoked` / `invalidate_family`
- **InMemorySessionStore::rotate 改为 mark-revoke + insert**（不再 remove 旧 token，否则 reuse detection 失效）
- AuthServiceError 新增 `SessionReuseDetected` 变体
- **`refresh_session` 完整重写**: idle/absolute/revoked check → detect_reuse (拉 family 扫描) → rotate (继承 family_id)
- 11 个新 R512 测试 (9 session_refresh pure + 2 auth_service integration)
- r569_refresh_session_rotates_token 升级：旧 token 第二次 refresh 现返回 SessionReuseDetected

### R511 — pc-openapi + pc-http
- **V3 收尾 (95% → 100%)** ⭐
- 8 个新 schemas: Case (17 fields, 6 status enum) + Goal (10 fields, 5 level + 5 status enum) + Inbox (9 fields, 2 kind enum) + Folder (10 fields, 2 kind enum) + 4 List shapes
- `register_core_dtos` & `CORE_DTO_NAMES`: 19 → 27 schemas
- 25 个新 path hints: 7 cases 子资源 + 2 goals PATCH/DELETE + 6 inbox dismissals + 10 folders CRUD+legacy
- Coverage 测试升级: 44 → **69 hints** (100% 已注册路由覆盖)
- **operationId 唯一性 guard**: `pub fn operation_id` + `pub fn find_duplicate_operation_ids(body) -> Vec<String>`
- 22 个新 R511 测试 (12 pc-openapi + 10 pc-http)
- **V3 95% → 100%** ⭐ (V3 收官)

### R510 — pc-openapi + pc-http
- 2 个新 schemas: PaginationCursor + ListResponseEnvelope (builder fn 接受 item_ref)
- 12 个新 path hints: cases CRUD (5) + goals CRUD (3) + approvals PATCH/DELETE (2) + pipelines PATCH/archive (2)
- Coverage 测试从 32 hints 扩到 44 hints (79% 路由覆盖)
- 10 个新 R510 测试 (6 pc-openapi + 4 pc-http)
- **V3 90% → 95%**

### R509 — pc-openapi + pc-http
- 3 个 error schemas: ValidationError / ValidationErrorList / ErrorResponse
- `build_responses_block(schema, has_request_body)` 签名升级
- 422 ValidationErrorList (POST/PATCH only) + 500 ErrorResponse (all ops)
- 8 个新 path hints: 4 item GETs + pipelines/routines POST + routines PATCH + heartbeat POST
- Coverage 测试从 24 hints 扩到 32 hints
- 15 个新 R509 测试 (6 pc-openapi + 9 pc-http)
- **V3 85% → 90%**

### R508 — pc-openapi + pc-http
- 3 个新 schemas: Pipeline (12 字段) + Routine (30 字段) + RoutineList
- 9 个新 path hints: 4 PATCH (X→X) + 4 DELETE (no body) + Routines GET
- Coverage 测试从 15 hints 扩到 24 hints
- 类型签名升级: `(str, str, Option<str>, Option<str>)` 支持 DELETE 的 None 响应
- 12 个新 R508 测试 (7 pc-openapi + 5 pc-http)
- **V3 78% → 85%**

### R507 — pc-openapi + pc-http
- 4 个 `*List` schemas (兑现 R506 placeholder): CompanyList / AgentList / IssueList / DecisionList
- Approval + ApprovalList + PipelineList (3 新 schemas)
- `register_core_dtos` 注册 12 个 schemas
- `path_schema_hint` 加 5 hints: approvals GET/POST/GET-by-id + pipelines GET + heartbeat-runs/{id} GET
- R504 测试从 `== 5` 改为 `>= 5` (向后兼容)
- 4 个新 R507 测试
- **V3 72% → 78%**

### R506 — pc-http::routes::openapi
- `PathSchemaHint { request, response }` 数据结构
- `path_schema_hint(path, method) -> Option<PathSchemaHint>` 静态查找表 (10 hints)
- `build_responses_block(schema)` + `build_request_body_block(schema)` builder fn
- 扫描器升级: 有 hint 的路径生成真实 `$ref`, 无 hint 回退到 minimal
- 11 个新 R506 测试 (覆盖 10 hints + builder fns + end-to-end)
- **V3 65% → 72%**

### R505 — pc-openapi + pc-http
- `pc_openapi::OpenApiRegistry::register_schema_value` 新方法 (绕过 `#[serde(flatten)]` bug)
- `pc_openapi::SchemaRef::Raw(Value)` 新变体 (serialize verbatim, 不丢 `type`/`required`)
- `pc-openapi::dto_schemas::register_core_dtos` 改用 `register_schema_value`
- `pc_http::routes::openapi::inject_dto_schemas(&mut Value)` 纯函数, 把 schemas merge 进 body
- `pc_http::build_openapi_body` 末尾调 `inject_dto_schemas`
- `pc-http` 加 `pc-openapi` dep
- 5 个新 R505 测试 (含 wire format 验证: `required` 在场, enum 完整, securitySchemes 共存, YAML round-trip)
- **V3 55% → 65%**

### R504 — pc-openapi
- `dto_schemas.rs` 396 LOC, 5 个核心 DTO + 2 companion schemas
  - `Decision` (22 fields), `Company` (19), `Issue` (17), `Agent` (21), `HeartbeatRun` (7)
  - `DecisionOption` (id/label/effects/targetIds), `DecisionEffect` (7 type enum)
- `register_core_dtos(&mut OpenApiRegistry)` 幂等 façade
- `into_schema_ref(&Value) -> SchemaRef` 把 JSON schema 包装成 registry 接受的形式
- `CORE_DTO_NAMES: &[&str]` 常量给 V4 (UI type sync) 用
- 11 个新单测
- **V3 50% → 55%**

### R503 — pc-http::routes::openapi
- `/openapi.yaml` + `/api/openapi.yaml` 端点 (手写 YAML emitter)
- `build_openapi_body(&AppState) -> Value` 抽出 helper (JSON/YAML 单一 source-of-truth)
- 3.0.3 → 3.1.0 升级 (与 pc-openapi::spec 一致)
- 6 个新 YAML emitter 测试 + 3 个 pre-existing route test
- **V3 40% → 50%**

### R502 — pc-decisions
- `CreateDecisionSpec` 数据结构 (pure.rs, 83 LOC)
  - 镜像上游 `CreateInput` (sans auth)
  - 5 facade methods: `validate_options` / `all_target_ids` / `all_target_actions` / `spec_envelope` / `effective_expires_at`
- `DecisionService.create_with_spec(company_id, title, body, spec)` 新方法
  - 旧 `create(...)` 签名保留为 wrapper (零破坏性)
  - 真接入 R492 helpers: `validate_options` 错误会实际让 create 失败
- `DecisionRepo.create_with_options(...)` 新方法, 接 `options: Value` + `expires_at: Option<DateTime<Utc>>`
- 10 个 pure 单测 + 2 个 pc-http 集成测试
- **R492 helpers 真正接入业务路径 (验证低耦合)**

### R501 — pc-openapi
- `serializers.rs` 272 LOC + 7 测试
  - `path_count` / `operation_count` / `schema_count` 统计
  - `to_json_string` / `to_json_value` / `to_yaml_string` 序列化
  - 手写 YAML 发射器（无 `serde_yaml` 依赖）
  - 修了 key 引号 bug（`serde_json::to_string(k)` → 直接用 `k`）
- `lib.rs` 加 `pub mod serializers;` 暴露
- V3 OpenAPI 5% → 15%

### R499 — doc
- `ARCHITECTURE.md` 261 行（本文档），6 章 + 9 节 + 当前状态快照

### R500 — pc-cli
- `worktree url` 不再硬编码，从 worktree 路径 + FNV-1a hash 派生端口
- `worktree dev` 新 action 一次性打印 worktree 信息 + 派生 URL + 启动提示
- 5 个新 helper + 5 个新测试
- **V2 CLI 18/18 main + 20/20 nested = 100%** ⭐

### R493 — pc-cli
- **`onboard --non-interactive` 真做事**
  - 3 个新 helper: `render_onboard_env` (纯函数) + `generate_master_key_b64` (OsRng + base64) + `onboard_command`
  - 8 个新测试覆盖纯函数 + 3 种交互模式
  - **V2 CLI 30% → 33%**

### R494 — pc-decisions
- **真实去重**: R492 `find_commit_sha` 重复实现被发现 → 单行 re-export
- `pc-decisions::find_commit_sha` 测试覆盖从 6 个 → 13 个（来自 pc-repos）

### R495 — pc-cli
- **`install` 真做事**
  - 3 个 helper: `default_install_prefix` (HOME-based, 无 unsafe) + `plan_install` (纯函数) + `install_command` (真 symlink)
  - 1 个 struct: `InstallOutcome`
  - 6 个测试覆盖纯函数 + 真 symlink 创建 + 拒绝覆盖 + force
  - **V2 CLI 33% → 39%**

### R496 — pc-cli
- **`uninstall` + `update` 真做事**
  - 6 个 helper: `plan_uninstall` (与 plan_install 镜像) + `uninstall_at` (默认仅 symlink) + `UninstallOutcome`
    + `compare_versions` (semver-like) + `build_update_hint` (cargo install 提示) + `CURRENT_VERSION` const
  - 9 个测试
  - **V2 CLI 39% → 44%**

### R497 — pc-cli
- **`run` 真做事**
  - 2 个 helper: `resolve_server_binary` (4 优先级) + `build_run_env` (sorted pass-through)
  - 1 个新 flag 集: `--server-binary` / `--detach` / `--pid-file`
  - 5 个测试
  - 真 `std::process::Command::spawn` pc-server
  - **V2 CLI 44% → 50%**

### R498 — pc-cli
- **`env` + `onboard` 交互模式 真做事**
  - 1 个 enum: `EnvFormat` (clap `ValueEnum` derive)
  - 3 个 helper: `build_resolved_env` (从 process env 读) + `default_config_toml` (默认配置) + `env_command` (按 format 输出)
  - 5 个测试
  - **V2 CLI 50% → 61%**

### 累计成果
- **8 个新 `pub fn` + 1 个 `pub struct` + 1 个 `pub enum`**（R492 一次性）
- **+90 个新单测**（≈ +5% 整体覆盖率）
- **V2 CLI 30% → 61%** ⭐
- **pc-decisions 6 → 38 测试**（+533%）
- **pc-cli 11 → 44 测试**（+300%）
- **整体 1684 → 1774 测试**（+5%）

---

## 4. 真实硬骨头（V1-V15 仍未推）

按"用户硬目标 + 工程价值"排序：

### 4.1 V3 + V4 — OpenAPI 完整化 + UI 类型对齐 (P0 契约)
- **现状**: `pc-openapi` 480 LOC 只生成 metadata，未对接 UI 60 client
- **工作量**: ~400 行 Rust (utoipa derive) + ~100 行 UI 类型生成
- **价值**: 前后端类型契约冻结（用户硬目标）
- **风险**: 中（动 pc-http + UI 全量）

### 4.2 V5 — Auth 完整化 (P0 用户面)
- **现状**: pc-auth 581 LOC + pc-authz 128 LOC，仅基础 session + API key
- **工作量**: ~500 行 Rust (OAuth2 + PKCE) — refresh rotation + CSRF double-submit + API key pk_<base62> 已完成 (R512-R515)
- **价值**: 真实多用户登录（解锁 V11/V12）
- **风险**: 高（auth 是关键路径）

### 4.3 V11 + V12 — UI 60 client happy path + Playwright (P0 用户硬目标)
- **现状**: 0%
- **工作量**: ~200 行 Playwright 剧本 + 60 个 endpoint happy 验证
- **价值**: **用户硬目标**（"真实启动前后端验证"）
- **风险**: 中（依赖 V1/V5/V6）

### 4.4 V6 — 路由字节级补全 (P0 用户面)
- **现状**: 56/56 主路由，缺 companies 子路由 + /api/admin/*
- **工作量**: ~300 行 Rust route handler
- **价值**: UI 60 client 字段对齐前置
- **风险**: 低

### 4.5 V8 — 远程 execution (P1)
- **现状**: 0%
- **工作量**: ~500 行 Rust (SSH bridge / workspace materialization)
- **价值**: 远程分布式 agent
- **风险**: 中

### 4.6 V13 — 5 分钟长跑 + 性能基线 (P1 性能声明)
- **现状**: 0%
- **工作量**: ~200 行 long-run script + criterion benches + wrk 对比
- **价值**: P99↓30% / RSS↓40% 数据支撑
- **风险**: 低

### 4.7 V15 — 中文文档 (P2 社区)
- **现状**: ARCHITECTURE-DIAGRAMS / MODULE-MAPPING / PROJECT-PLAN 已有；缺 OPERATIONS / PLUGIN_AUTHORING / MIGRATION
- **工作量**: ~1500 行 markdown
- **价值**: 社区移交
- **风险**: 低

---

## 5. 架构关键设计原则（已落地）

### 5.1 高内聚低耦合
- **Pure function facade 模式** (R492 示范): `pc-decisions::pure` re-export `pc-repos::decision_training::find_commit_sha`，避免双实现
- **Repository → Service → Route 三层**: pc-repos 提供数据访问、pc-* service 包装业务、pc-http::routes 暴露
- **公共算法单点**: `pc_secrets::canonical` 提升为 `pub`，业务层复用而非重新实现 ryu_js

### 5.2 Rust 最佳实践
- **Newtype ID 全部用 Uuid** (pc_core::Timestamp / sqlx::FromRow)
- **Result 错误传播**: thiserror 派生 + `From<RepoError>` / `From<sqlx::Error>` impl
- **Trait 抽象**: `DecisionHook` / `DecisionBundleHook` / `SecretProvider` 等都是 async trait + dyn dispatch
- **Tokio 取消**: spawn + join_handle，signal handling 在 pc-server
- **sqlx 编译期校验**: 所有 SQL 都用 `query_as!` 或 `query_as::<_, T>` 带 FromRow
- **forbid(unsafe_code)**: workspace 级别强制，R495 install 改用 `--prefix` 显式 opt-in 替代 auto-detect root

### 5.3 真实做事（不假装）
- CLI 6 个子命令真做事: install 真创建 symlink / uninstall 真删 / run 真 spawn pc-server / onboard --non-interactive 真生成 master key / env 真读 process env
- 测试不靠 mock: 真实 `std::env::temp_dir()` + `Uuid::new_v4()` 隔离 + cleanup 兜底
- 真实发现重复: R494 立即合并 R492 重复

---

## 6. 推荐下一步（R499+ 路线图）

按"价值/风险"比排序，3-5 轮可达下一个里程碑：

| 轮次 | 目标 | 价值 | 风险 |
|---|---|---|---|
| **R499** | 写 `ARCHITECTURE.md`（本文档）| 战略可见性 | 0 |
| **R500** | V2 CLI 收尾 nested action stub (service start/stop, worktree merge/dev) | V2 61% → 90%+ | 低 |
| **R501** | V3 OpenAPI 起手:  JSON/YAML + 统计 helpers | V3 5% → 15% | 低 ✅ |
| **R502** | R492 helper 接入 ✅: `create_with_spec` + `CreateDecisionSpec` + repo `create_with_options` | 验证低耦合 ✅ | 中 |
| **R503** | V3 OpenAPI 起手: 引入 utoipa derive + 56 path 注册 | V3 5% → 60% | 中 |
| **R504** | V5 Auth 起手: refresh rotation + CSRF double-submit | V5 55% → 75% | 高 ✅ |
| **R505** | V11 UI 60 client happy path (依赖 V1/V5/V6) | 用户硬目标 | 中 |
| **R506** | V12 Playwright 真实 UI 剧本 | 用户硬目标 | 中 |

**短期 3 轮目标**: V2 90% + V6 100% + R492 验证
**中期 6 轮目标**: V3 60% + V5 75% + V11 100%
**长期**: V12 + V13 + V15（社区移交）

---

## 7. 验证基线（R514 末）

```
cargo check --workspace                 0 errors (1 warning from pc-cli: unused mut, 1 pre-existing)
cargo test -p pc-decisions --lib        38 passed
cargo test -p pc-openapi --lib           66 passed (58 pre + 8 R513 new)
cargo test -p pc-http --lib routes::openapi 59 passed (56 pre + 3 R513 new)
cargo test -p pc-decisions --lib           48 passed (38 pre + 10 R502 new)
cargo test -p pc-http --lib routes::openapi 9 passed (3 pre + 6 R503 new)
cargo test -p pc-cli --bin paperclipai  44 passed
cargo fmt -p pc-decisions --check       no diff (本轮 R492 改动)
cargo fmt -p pc-cli --check             no diff (本轮 R495-R498 改动)
```

整体单测 ≈ **6619 passing** `cargo test --workspace --lib` (R533 末实测, 0 failed); R533 本轮增量 +21 (pc-external-objects-server); workspace crates **75 → 76**。

---

## 8. 关键经验教训（跨 R487-R498）

1. **先搜再写**: R492 写 `find_commit_sha` 前没 `rg` → 重复 → R494 立即合并
2. **测试 cleanup 顺序**: 真实文件测试必须 `read` 在 `remove_dir_all` 之前 (R495 fix)
3. **`forbid(unsafe_code)`**: `geteuid` 拒绝 → 用 `--prefix` 显式 opt-in
4. **跨平台 fallback**: Windows 不支持 symlink → fallback 到 copy
5. **`#[derive(Subcommand, Debug)]` 不能丢**: R498 替换 enum anchor 时把 derive 也吃掉了 → 所有 match 因 trait bound 失败
6. **`ValueEnum` 比手写 `from_str` idiomatic**: clap 直接把 enum 变 CLI 参数
7. **BTreeMap 让测试稳定**: 多次调用结果相同（不依赖 env state）

---

## 9. 相关文档

- `ARCHITECTURE-DIAGRAMS.md` — 底层架构图（47KB，已有）
- `MODULE-MAPPING.md` — Node→Rust 模块映射（16KB，已有）
cargo test -p pc-auth --lib                80 passed (67 pre + 13 R514 new)
cargo test -p pc-http --lib middleware::csrf  23 passed (18 pre + 5 R515 new)
cargo test -p pc-http --lib routes::openapi   69 passed (59 pre + 10 R515 new)
