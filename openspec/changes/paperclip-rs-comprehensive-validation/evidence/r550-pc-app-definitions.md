# R550 — pc-app-definitions（Node app-definitions.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/app-definitions.ts` (67 LOC) 完整复刻到新 crate
`crates/pc-app-definitions`。workspace crates 91 → **92**。

## 设计原则

### 1. 强类型 structs 替代 TypeScript 接口
- `AppDefinition`, `ConnectionMethodDef`, `FieldDef`, `MethodDefaults` 等都建模为 struct
- enum 替代字符串字面量：`AppCategory`, `FieldType`, `ConnectionAuth`, `RiskTier`, `ToolConnectionOwnership`
- 编译期类型安全，避免 `categories: ["invalid"]` 等错误

### 2. `HashSet<&'static str>` for connectable slugs
- Node 用 `Set<string>`（hash set）
- Rust 用 `HashSet<&'static str>`（零拷贝，零分配）
- 通过 `connectable_app_slugs()` 函数暴露

### 3. 通配符 URL 匹配（无 regex 依赖）
- Node 用 `new RegExp(\`^${escaped}$\`, "i")` + `*` 转 `.*`
- Rust 手写 wildcard matcher（`wildcard_match_recursive`）避免 `regex` crate
- 11 行代码支持 `*` 通配符 + 完整转义 + case-insensitive
- 性能：O(n*m)，比 regex engine 更轻量

### 4. `&serde_json::Value` 入口 + typed 出口
- 提供 `find_app_definition_by_slug` / `filter_app_catalog_by_slugs` / `find_app_definition_for_url`
  接受原始 JSON catalog（业务侧可自由接入）
- 也提供 typed API（`AppDefinition` struct）用于强类型场景

### 5. `recommended_defaults_for_app` 输出 `serde_json::Map`
- 替代 Node `Record<string, unknown>` 字面量
- 返回类型确定 (`Map<String, Value>`)，可序列化为 JSON 直接喂给 client

### 6. RiskTier-aware defaults
- S1 (read-only) → `askFirstRiskLevels: []`（无需 ask first）
- S2/S3/S4 → `["write", "destructive"]`（需要确认）
- 镜像 Node 完全行为

## 公开 API

```rust
// ----- enums -----
pub enum AppCategory { Ai, Analytics, Commerce, Communication, Content, Data, Developer, Productivity, Other }
pub enum FieldType { Text, Password, Textarea, Datetime, Select, Checkbox }
pub enum KeyPlacementLocation { Header, Query, BodyJson, Env }
pub enum ConnectionAuth { OAuth, ApiKey, None }
pub enum RiskTier { S1, S2, S3, S4 }
pub enum ToolConnectionOwnership { PlatformShared, PlatformProvisioned, Customer, Dcr }

// ----- structs -----
pub struct FieldDef { key, label, field_type, required, placeholder, helper_md, secret, prefix, validation, options }
pub struct FieldValidation { pattern, max_length }
pub struct FieldOption { value, label }
pub struct KeyPlacement { location, name, prefix }
pub struct MethodDefaults { server_url, discovery_url, service_host, ..., scopes_hint }
pub struct ConsoleLinks { register, keys, settings, docs }
pub struct MethodVariant { key, label, when_to_use, tenant_fields }
pub struct ConnectionMethodDef { key, transport, auth, ownership_modes, ..., risk_tier, required_resource_filters }
pub struct AppBranding { logo_url, dark_logo_url, background_color, accent_color }
pub struct AppAvailability { available, reason, robot_email }
pub struct AppDefinition { schema_version, slug, name, ..., url_patterns, methods, ownership_availability }

// ----- helpers -----
pub fn default_ownership_availability() -> HashMap<ToolConnectionOwnership, bool>
pub fn connectable_app_slugs() -> HashSet<&'static str>
pub fn connectable_app_definitions(all: &[AppDefinition]) -> Vec<AppDefinition>
pub fn get_connectable_app_definition(slug: &str, definitions: &[AppDefinition]) -> Option<&AppDefinition>
pub fn get_app_definition_for_url(link: &str, definitions: &[AppDefinition]) -> Option<&AppDefinition>
pub fn get_available_connection_method(app: &AppDefinition) -> Option<&ConnectionMethodDef>
pub fn credential_config_path(field: &FieldDef) -> String  // "credentials.{key}"
pub fn recommended_defaults_for_app(app: &AppDefinition) -> Map<String, Value>

// ----- JSON helpers -----
pub fn find_app_definition_by_slug(catalog: &Value, slug: &str) -> Option<&Value>
pub fn filter_app_catalog_by_slugs(catalog: &Value, slugs: &HashSet<&str, S>) -> Vec<Value>
pub fn find_app_definition_for_url(catalog: &Value, link: &str) -> Option<&Value>
```

## 与上游 Node 差异

- **snake_case 字段名**：Node camelCase，Rust snake_case
- **`&'static str` 替代 string**：connectable_app_slugs 返回零拷贝 HashSet
- **手写 wildcard matcher**：避免 `regex` crate 依赖
- **struct 强制类型化**：`AppDefinition` 不允许未知字段

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-app-definitions` | **25 passed** (5 internal + 20 integration) |
| `cargo fmt -p pc-app-definitions` | ✅ 通过 |
| `cargo clippy -p pc-app-definitions --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（25 个）

- **internal** (5): wildcard exact / star / case-insensitive / bare host normalize / default ownership
- **integration** (20): connectable slugs / default ownership / filter / find by slug / url match exact / url match no / url invalid / url bare host normalize / method availability (default/customer/override) / credential path / recommended defaults (S1/S2/no-method) / method defaults / JSON helpers (find/filter/url)

## 集成待办（不在本轮范围）

- `pc-tools` / `pc-connections`：用 `find_app_definition_by_slug` / `filter_app_catalog_by_slugs`
- `pc-server`：用 `recommended_defaults_for_app` 提供新连接预设
- `pc-portability`：`credential_config_path` 帮助 zip 导出 / 导入
- 端到端：UI 连接器页面 → 后端用 typed API 渲染
