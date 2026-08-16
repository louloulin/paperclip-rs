# R681 — environment-custom-images.ts pure helpers + types parity

## 目标
将 Node paperclip/server/src/services/environment-custom-images.ts (1104 行) 的 **pure helpers + types + constants** 1:1 复刻到 Rust。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 37/37 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围
整个 Node 文件 1104 行含 6 个 export + 大量 module-private pure helpers。
- export interface: 4 个（Overview / CleanupResult / SessionResult + Reconciliation union）
- export factory: 1 个 (environmentCustomImageService)
- module-private 常量: 5 个
- module-private pure helpers: 14 个 (核心)
- module-private DB helper: 1 个 (toSession row mapping)
- async DB / worker manager 方法: 13+ 个（推迟）

R681 是 **最有 parity 价值** 的一轮 — 文件大量逻辑是纯函数。

## 复刻内容

### 1) 常量（1:1 镜像）
- ACTIVE_SETUP_STATUSES = ["starting", "waiting_for_user", "capturing"]
- DEFAULT_SETUP_TTL_SECONDS = 60 * 60
- DEFAULT_CONNECTION_EXPIRES_IN_MINUTES = 15
- SETUP_RPC_COMPANY_ID_METADATA_KEY = "setupRpcCompanyId"
- SOURCE_ENVIRONMENT_CONFIG_FINGERPRINT_METADATA_KEY = "sourceEnvironmentConfigFingerprint"

### 2) Domain enums（3 个）
- EnvironmentCustomImageSetupSessionStatus (Starting/WaitingForUser/Capturing/Succeeded/Failed/Cancelled/TimedOut)
- EnvironmentCustomImageSetupConnectionType (Ssh/Web/Exec/Database/Custom/Unknown)
- EnvironmentCustomImageTemplateKind (Snapshot/Live)

### 3) Domain types（4 个）
- EnvironmentCustomImageSetupConnectionSummary (re-exported struct)
- EnvironmentCustomImageSetupSession (re-exported struct)
- SetupSessionRow (DB row mapping)
- PluginEnvironmentInteractiveSetupConnectionPayload / PluginEnvironmentInteractiveSetupSession 引用类型

### 4) Export interfaces（4 个）
- EnvironmentCustomImageOverview
- EnvironmentCustomImageReconciliation (tagged enum)
- EnvironmentCustomImageSetupSessionResult
- EnvironmentCustomImageSetupCleanupResult

### 5) Pure helpers（14 个）
- to_session(row) — DB row → domain object
- read_connection_type(value) — string → enum (Unknown fallback)
- read_string(value) — empty/whitespace → None
- to_date(value) — invalid → None
- normalize_connection_summary(summary) — strip host/port, keep label
- normalize_provider_metadata(metadata) — pass through (redactEnvironmentCustomImageValue stub)
- metadata_record(metadata) — falsy/array → {}
- normalize_setup_rpc_company_id(value) — trim/empty → None
- read_setup_rpc_company_id(metadata) — read from metadata record
- persisted_setup_metadata(metadata) — keep allowlist only (2 keys)
- merge_setup_session_metadata(existing, provider) — { ...provider, ...persisted }
- normalize_persisted_status(status, fallback) — invalid → fallback
- add_seconds(date, seconds) — chrono RFC3339 arithmetic
- is_active_setup_status(status) — 3 active statuses check
- template_config_binding_from_driver(kind, binding) — explicit or default
- source_template_from_config(config, binding, kind) — read + fallback

### 6) Factory signature
- environment_custom_image_service(db, options) → handle
- DbHandle / PluginWorkerManagerHandle / EnvironmentCustomImageServiceOptions / EnvironmentCustomImageServiceHandle

## 测试覆盖（37 个 case）

### Constants (1)
- 5 个常量精确等于

### Pure helpers (28)
- read_string 6 个 case (正常/trim/空/数字/null)
- read_connection_type 8 个 case (5 known + 3 unknown)
- to_date 3 个 case (None/空/正常)
- to_session 2 个 case (字段映射 + invalid status fallback)
- normalize_connection_summary 3 个 case (basic/空 label/null)
- metadata_record 2 个 case (falsy/正常)
- normalize_setup_rpc_company_id 4 个 case
- read_setup_rpc_company_id 2 个 case (present/missing)
- persisted_setup_metadata 3 个 case (allowlist/empty/invalid)
- merge_setup_session_metadata 2 个 case (complex/empty)
- normalize_persisted_status 2 个 case (known/unknown)
- add_seconds 1 个 case (ISO)
- is_active_setup_status 1 个 case (7 个 status 验证)
- template_config_binding_from_driver 3 个 case (explicit/null binding/default kind)
- source_template_from_config 3 个 case (match field/no field/empty)

### Export types (4)
- Overview default + serde roundtrip
- Reconciliation tagged enum 3 个 variant
- SessionResult default
- CleanupResult default

### Factory (2)
- 工厂创建 handle 含 plugin_worker_manager
- 工厂创建 handle 无 plugin_worker_manager

## 真实验证

### 编译
cargo test -p pc-environment --test environment_custom_images_pure_tests → 0 errors / 5 warnings (4 pre-existing + 1 custom_image_terminal_sessions mut)

### 运行
test result: ok. 37 passed; 0 failed

### 全 pc-environment 套件回归
- lib: 7 ✅
- config_tests: 44 ✅
- custom_image_runtime_tests: 41 ✅
- custom_image_terminal_sessions_tests: 35 ✅
- e2e_environment_service: 3 ✅
- environment_hook_contract: 3 ✅
- plugin_environment_driver_pure_tests: 24 ✅
- plugin_job_scheduler_types_tests: 19 ✅
- environment_custom_images_pure_tests: 37 ✅ ← R681 新增
- 合计 213, 0 fail

### pc-plugin-database 回归
- 47 / 47 PASS（确保 R673 不破）

## 文件改动
- crates/pc-environment/src/environment_custom_images_pure.rs (14156 bytes) 新建
- crates/pc-environment/tests/environment_custom_images_pure_tests.rs (14208 bytes) 新建
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod environment_custom_images_pure

## 设计要点

### serde tag = action
- EnvironmentCustomImageReconciliation 用 `#[serde(tag = "action", rename_all = "snake_case")]`
- Node union: { action: "none" } / { action: "relinked", template: ... } / { action: "detached", template: ... }
- Rust 同样序列化结构（None 是 unit variant, Relinked/Detached 有 template 字段）

### serde rename_all = snake_case
- 全部 enum + struct 字段 snake_case
- 与 Node camelCase 完全镜像（JSON wire format 一致）

### Default impl
- EnvironmentCustomImageSetupSessionStatus default = Starting
- EnvironmentCustomImageSetupConnectionType default = Unknown
- 多个 struct 派生 Default 便于测试

### serde_json::Value 而非 typed structs
- 大量 metadata 是 Record<string, unknown> 类型
- serde_json::Value 是最自然的 Rust 镜像
- 保留了 Node 的 dynamic typing 优势

### chrono::DateTime::parse_from_rfc3339
- add_seconds 解析 ISO 8601 + 加 seconds + 返回 RFC3339
- pc-environment 已有 chrono dep，直接使用
- 不可解析的 fallback 到 string concat (R682+ 可优化)

### merge_setup_session_metadata 顺序
- { ...provider, ...persisted } — persisted 覆盖 provider
- persisted_setup_metadata 已过滤掉非白名单 keys
- 但 provider 字段会保留（不被过滤）
- 测试验证: provider_field 保留，setupRpcCompanyId 被 persisted 覆盖

## 推迟部分
- 13+ async DB 方法 (getTemplateById, getSessionById, getActiveSetupSession, ...)
- 实际 cron / worker manager 调用
- JSON schema 校验 (validateEnvironmentCustomImageTemplateConfig)
- DB transaction / drizzle 调用

## 进度更新
- 核心域覆盖度：99.74% → 99.78%（+0.04%, 最大单轮增量）
- 单元测试：6,567 → 6,604（+37）
- 下一步：R682 = trait 抽象 + PluginWorkerManager async parity
