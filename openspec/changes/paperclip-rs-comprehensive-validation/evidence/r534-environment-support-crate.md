# R534 — pc-environment-support 新 crate（Node environment-support.ts 复刻）

> 时间：2026-08-11 · 状态：✅ 完成 + 测试通过 + clippy 干净

## 1. 目标

按 "高内聚低耦合" 原则，1:1 port `paperclip/packages/shared/src/environment-support.ts`
（约 170 LOC pure functions）到独立 Rust crate `pc-environment-support`。

## 2. 范围

| Node 上游 | Rust port | 说明 |
|---|---|---|
| `AgentAdapterType` (string union + plugin 扩展) | `AgentAdapterType(String)` newtype | 编译期类型安全 |
| `SandboxEnvironmentProvider` | `SandboxEnvironmentProvider(String)` newtype | 同上 |
| `EnvironmentDriver` (literal union) | `EnvironmentDriver` enum (Local/Ssh/Sandbox/Plugin) | 穷尽匹配 |
| `EnvironmentSupportStatus` | `EnvironmentSupportStatus` enum | 同上 |
| `REMOTE_MANAGED_ADAPTERS` (Set) | `REMOTE_MANAGED_ADAPTERS` const slice | 编译期常量 |
| `adapterSupportsRemoteManagedEnvironments` | `adapter_supports_remote_managed_environments` | 纯函数 |
| `supportedEnvironmentDriversForAdapter` | `supported_environment_drivers_for_adapter` | 纯函数 |
| `supportedSandboxProvidersForAdapter` | `supported_sandbox_providers_for_adapter` | 纯函数 + 去重 |
| `isEnvironmentDriverSupportedForAdapter` | `is_environment_driver_supported_for_adapter` | 纯函数 |
| `isSandboxProviderSupportedForAdapter` | `is_sandbox_provider_supported_for_adapter` | 接受 None |
| `getAdapterEnvironmentSupport` | `get_adapter_environment_support` | builder |
| `getEnvironmentCapabilities` | `get_environment_capabilities` | builder |
| `EnvironmentProviderCapability` (11 bool fields + 7 optional) | `EnvironmentProviderCapability` struct | serde camelCase |
| `EnvironmentCapabilities` | `EnvironmentCapabilities` struct | 聚合 |

## 3. 设计决策

### 3.1 Newtype vs enum

- `AgentAdapterType` / `SandboxEnvironmentProvider` 是 newtype String — 上游允许 plugin
  扩展任意字符串，enum 表达不了
- `EnvironmentDriver` / `EnvironmentSupportStatus` 是 enum — 上游是固定 4 个 / 2 个字面量
  集合，编译期穷尽匹配带来安全性

### 3.2 Vec 替代 Record / Map

上游 `sandboxProviders` 是 `Record<K, V>` (object)，JS 迭代顺序是插入顺序。
Rust 用 `Vec<(K, V)>` 保持：
- `"fake"` 始终在前
- 追加 providers 按上游 `additional_providers` 顺序
- JSON 序列化结果一致（serde 对 tuple struct 转 object 时顺序一致）

### 3.3 接受 `&[T]` 而非 `readonly T[]`

Rust 习惯。调用方传 slice 即可，无须 `.into_iter()` 包装。

### 3.4 零依赖

仅依赖 `serde` + `serde_json`（用于 `EnvironmentProviderCapability` 的 JSON 序列化）。
不引入 `pc-environment` 等业务 crate — 本 crate 是纯函数库，无 DB / IO / async。

### 3.5 错误约定

纯函数不返回 Result — 不存在的 case 返回 `false` / 空 `Vec` / `None`。
与上游一致：`isSandboxProviderSupportedForAdapter(null)` → `false`，
`getAdapterEnvironmentSupport("cursor_cloud")` → 只有 `local` driver。

## 4. 验证（真实运行）

```
$ cargo check -p pc-environment-support
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.41s

$ cargo test -p pc-environment-support
running 31 tests
test tests::r534_builtin_fake_capability_matches_upstream ... ok
test tests::r534_environment_capabilities_empty_adapters_returns_empty_vec ... ok
test tests::r534_driver_as_str_round_trips ... ok
test tests::r534_environment_capabilities_global_drivers_always_three_supported ... ok
test tests::r534_environment_capabilities_includes_grok_local_sandbox_supported ... ok
test tests::r534_environment_capabilities_no_plugin_overrides_only_fake ... ok
test tests::r534_environment_capabilities_sandbox_providers_fake_then_plugin ... ok
test tests::r534_environment_drivers_support_empty_iter_is_all_unsupported ... ok
test tests::r534_environment_drivers_support_from_supported_iter ... ok
test tests::r534_get_adapter_environment_support_drivers_match_supported_set ... ok
test tests::r534_get_adapter_environment_support_includes_fake_first_then_additional ... ok
test tests::r534_get_adapter_environment_support_non_remote_drivers_only_local ... ok
test tests::r534_grok_local_supports_remote_managed ... ok
test tests::r534_is_driver_supported_basic ... ok
test tests::r534_is_driver_supported_unknown_driver_returns_false ... ok
test tests::r534_is_sandbox_provider_supported_accepts_additional_for_remote_managed ... ok
test tests::r534_is_sandbox_provider_supported_null_provider_returns_false ... ok
test tests::r534_is_sandbox_provider_supported_provider_not_in_additional_returns_false ... ok
test tests::r534_is_sandbox_provider_supported_rejects_for_non_remote_managed ... ok
test tests::r534_newtype_round_trip ... ok
test tests::r534_non_remote_managed_adapters_rejected ... ok
test tests::r534_plugin_override_defaults_match_upstream ... ok
test tests::r534_plugin_override_status_unsupported_overrides_override ... ok
test tests::r534_remote_managed_adapters_set ... ok
test tests::r534_supported_drivers_grok_local_includes_sandbox ... ok
test tests::r534_serialization_camel_case_for_capability ... ok
test tests::r534_supported_drivers_non_remote_returns_only_local ... ok
test tests::r534_supported_drivers_remote_managed_returns_three ... ok
test tests::r534_supported_sandbox_providers_empty_additional_returns_empty ... ok
test tests::r534_supported_sandbox_providers_non_remote_returns_empty ... ok
test tests::r534_supported_sandbox_providers_remote_managed_dedupes ... ok

test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo clippy -p pc-environment-support -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s

$ cargo fmt -p pc-environment-support -- --check
(no diff — clean)
```

## 5. 文件清单

```
crates/pc-environment-support/
├── Cargo.toml      (8 行：name + workspace deps + serde + serde_json)
└── src/
    └── lib.rs      (~600 行 + 31 测试 = 995 行)
```

新增 workspace members：
- `crates/pc-environment-support`

workspace crates **76 → 77**

## 6. 上游测试覆盖对照

Node `environment-support.test.ts` 4 个 test case（vitest）：
1. ✅ accepts additional sandbox providers for remote-managed adapters
   → `r534_is_sandbox_provider_supported_accepts_additional_for_remote_managed`
2. ✅ rejects providers for adapters without remote-managed environment support
   → `r534_is_sandbox_provider_supported_rejects_for_non_remote_managed`
3. ✅ treats grok_local as a remote-managed local adapter
   → `r534_grok_local_supports_remote_managed` + `r534_supported_drivers_grok_local_includes_sandbox`
4. ✅ includes grok_local sandbox support in environment capabilities
   → `r534_environment_capabilities_includes_grok_local_sandbox_supported` + `r534_environment_capabilities_sandbox_providers_fake_then_plugin`

**100% 上游 test case 覆盖**，额外补充 27 个 Rust 边界测试（空 string、null provider、空 additional_providers、空 adapters、序列化 round-trip、newtype round-trip 等）。

## 7. 不范围（明确延后）

- DB 持久化（`server/src/services/environments.ts` 的 capability 拉取） — 留给 R535+ 集成层
- UI 渲染（`ui/src/lib/environment-support.ts` TS 端） — UI 是冻结契约
- Plugin manifest 解析 — 留给 plugin-host 集成层

## 8. 后续

R535 推荐下一个 port 候选：
- `packages/shared/src/environment-custom-images.ts` (115 LOC) — redact 工具，独立 crate `pc-environment-redaction`
- 或继续 port 其他 packages/shared/ 未完成模块（portability-hash, network-bind, agent-eligibility 等）
