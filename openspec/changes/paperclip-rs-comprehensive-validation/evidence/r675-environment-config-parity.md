# R675 — environment-config.ts 1:1 parity

## 目标

完整复刻 Node `server/src/services/environment-config.ts` (722 行) **pure
function subset** 到 `crates/pc-environment/src/config.rs`，覆盖 9 个
export function 中的 7 个 pure helper + 5 个 zod schema（strict 校验）。

DB- / Runtime- 注入依赖的 2 个函数（`normalizeEnvironmentConfigForPersistence`、
`resolveEnvironmentDriverConfigForRuntime`、`resolveSandboxProviderSecretRefPaths`、
`collectEnvironmentSecretRefs`）按当前 parity 留待依赖下沉后再补，保持单 module
边界。

## 工作产出

### 1. 新增文件

| 路径 | 行数 | 内容 |
|---|---:|---|
| `crates/pc-environment/src/config.rs` | 800 | 主模块：schemas + pure parse/normalize helpers |
| `crates/pc-environment/tests/config_tests.rs` | 412 | 44 个 unit test |

### 2. lib.rs 更新

`crates/pc-environment/src/lib.rs` 加入 `mod config;` 并 pub use 全部 18 个
对外类型 / 函数（保持对外契约稳定）。

### 3. 实现的 1:1 parity

| Node export | Rust 实现 | 状态 |
|---|---|---|
| `SecretRef` (zod schema) | `pc_environment::SecretRef` + `pc_environment::SecretRefVersion` | ✅ |
| `sshEnvironmentConfigSchema` | `pc_environment::parse_ssh_environment_config` → `SshEnvironmentConfig` | ✅ |
| `sshEnvironmentConfigProbeSchema` | `pc_environment::normalize_ssh_for_probe`（可选 `privateKey` 变体） | ✅ |
| `fakeSandboxEnvironmentConfigSchema` | `pc_environment::parse_fake_sandbox_environment_config` → `FakeSandboxEnvironmentConfig` | ✅ |
| `pluginSandboxEnvironmentConfigSchema` (catchall) | `pc_environment::parse_plugin_sandbox_environment_config` → `PluginSandboxEnvironmentConfig` (含 `extra: Map<String, Value>` 收 catchall) | ✅ |
| `pluginEnvironmentConfigSchema` | `pc_environment::parse_plugin_environment_config` → `PluginEnvironmentConfig` | ✅ |
| `parseEnvironmentDriverConfig` | `pc_environment::parse_environment_driver_config` → `ParsedEnvironmentConfig` enum | ✅ |
| `normalizeEnvironmentConfig` | `pc_environment::normalize_environment_config` → `NormalizedEnvironmentConfig` enum | ✅ |
| `stripSandboxProviderEnvelope` | `pc_environment::strip_sandbox_provider_envelope` | ✅ |
| `readSshEnvironmentPrivateKeySecretId` | `pc_environment::read_ssh_environment_private_key_secret_id` | ✅ |
| `getSandboxProvider` (私有) | `pc_environment::get_sandbox_provider` (pub use) | ✅ |
| (DB / Runtime 注入) | out-of-scope: `normalize…ForPersistence`、`resolve…ForRuntime`、`resolveSandboxProviderSecretRefPaths`、`collectEnvironmentSecretRefs` | ⏸ 依赖未下沉 |

### 4. 校验语义（与 Node zod 1:1）

- 拒绝 missing 必填字段（`host` / `username` / `remoteWorkspacePath` / `pluginKey` / `driverKey` / `provider`）
- 拒绝 invalid UUID
- 拒绝无效 provider key（regex `^[a-z0-9][a-z0-9._-]*$`）
- 拒绝 driver_key 同上 regex
- 拒绝 relative path（SSH remoteWorkspacePath）
- 拒绝 blank host/username/path
- 拒绝 non-null `privateKey`（canonical SSH schema）
- 允许 non-null `privateKey`（probe variant）
- `timeoutMs` 范围 1..=86_400_000
- `port` 范围 1..=65535
- `provider` 默认 `"fake"`、`image` 默认 `"ubuntu:24.04"`（matches zod `.default()`）
- `knownHosts` 空字符串 → `None`（zod `.transform()` 等价）
- `strictHostKeyChecking` 默认 `true`

### 5. 错误结构（matches Node `unprocessable` envelope）

`ConfigError` 提供稳定的 `{ message: String, issues: Vec<ConfigIssue> }` 形状，
`ConfigIssue` 含 `{ path: Vec<String>, message: String }`。下游可序列化到 Node
相同的 `unprocessable` 响应。

## 测试结果

### `cargo test -p pc-environment --test config_tests`

```
test result: ok. 44 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

测试覆盖：

| 类别 | 测试数 | 覆盖点 |
|---|---:|---|
| SecretRef | 4 | 最小 / latest / 错误 type / 错误 uuid |
| SSH canonical | 7 | 最小 / 完整 / 相对路径拒绝 / blank host/username/path / privateKey 拒绝 |
| SSH probe | 1 | 允许 privateKey |
| Fake sandbox | 2 | 默认 / 完整 |
| Plugin sandbox | 6 | 最小 / 完整 + extra / 无效 provider / 越界 timeout / max / above max |
| Plugin environment | 4 | 最小 / with driverConfig / 无效 driver_key / blank pluginKey |
| Sandbox dispatch | 4 | fake / plugin / 默认 provider / trim |
| strip envelope | 1 | 删除 provider 字段 |
| parse driver | 5 | local / ssh / sandbox / plugin / unsupported |
| normalize | 6 | local / ssh / sandbox / plugin / unsupported / issue 传播 |
| readSsh secret id | 3 | present / absent / invalid input |

### 回归

- `cargo test -p pc-environment --lib`：7 passed（无 regression；原有 R671 runtime_parity tests 仍然 PASS）
- `cargo build -p pc-server`：成功（无新 warning）

## 综合覆盖度（更新至 R675）

| 维度 | R674 终态 | R675 终态 |
|---|---|---|
| pc-http lib tests | 495 | **495**（无 regression） |
| pc-environment lib tests | 7 | **7**（无 regression） |
| pc-environment config_tests | — | **44 passed** |
| Node environment-config parity | partial | **完整（pure 7/9）** |
| 跨域 e2e blocks | 10 | **10**（保持） |

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（无 DB 注入依赖，pure 单测 44 PASS + pc-server build OK） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅ |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R676** | `pc-environment` DB / Runtime 部分下沉：当 `pc-environment-redaction` / `pc-plugin-database` 已具备 secret 服务 / plugin worker 抽象时，补齐 `normalize…ForPersistence`、`resolve…ForRuntime`、`resolveSandboxProviderSecretRefPaths`、`collectEnvironmentSecretRefs` 4 个 1:1 parity |
| **R677** | 探索其他 service parity 缺口（候选：`pc-pipeline-stages`、`pc-plugin-migrations`、`pc-issue-watchers` 等） |
