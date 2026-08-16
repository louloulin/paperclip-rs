# R677 — environment-custom-image-runtime.ts 1:1 parity

## 目标

完整复刻 Node `server/src/services/environment-custom-image-runtime.ts` (286 行) 的 **pure 子集**：
9 个 export function + 4 个常量 + 4 个类型。DB 注入函数
`resolveActiveEnvironmentCustomImageTemplateForRuntime` 按现有 parity 边界留待依赖下沉。

## 工作产出

### 1. 新增文件

| 路径 | 行数 | 内容 |
|---|---:|---|
| `crates/pc-environment/src/custom_image_runtime.rs` | 442 | 主模块：4 constants + 4 types + 9 pure functions + 私有 helpers |
| `crates/pc-environment/tests/custom_image_runtime_tests.rs` | 522 | 41 个 unit test |

### 2. 依赖挂接

- `crates/pc-environment/Cargo.toml` 加 `pc-secrets` + `sha2`

### 3. 1:1 parity 矩阵

| Node export | Rust 实现 | 状态 |
|---|---|---|
| `ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY` | `pc_environment::ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY` | ✅ |
| `ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS` | `pc_environment::ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS` | ✅ |
| `ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS` | `pc_environment::ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS` | ✅ |
| `ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS` | `pc_environment::ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS` | ✅ |
| `EnvironmentCustomImageRuntimeConfigBinding` | `pc_environment::EnvironmentCustomImageRuntimeConfigBinding` | ✅ |
| `EnvironmentCustomImageTemplateKind` | `pc_environment::EnvironmentCustomImageTemplateKind` | ✅ |
| `EnvironmentCustomImageConfigChangeKind` | `pc_environment::EnvironmentCustomImageConfigChangeKind` | ✅ |
| `EnvironmentCustomImageTemplate` | `pc_environment::EnvironmentCustomImageTemplate` | ✅ |
| `readEnvironmentCustomImageTemplateKind` | `read_environment_custom_image_template_kind` | ✅ |
| `defaultEnvironmentCustomImageRuntimeConfigBinding` | `default_environment_custom_image_runtime_config_binding` | ✅ |
| `normalizeEnvironmentCustomImageRuntimeConfigBinding` | `normalize_environment_custom_image_runtime_config_binding` | ✅ |
| `resolveEnvironmentCustomImageRuntimeConfigBinding` | `resolve_environment_custom_image_runtime_config_binding` | ✅ |
| `fingerprintEnvironmentSandboxProviderConfig` | `fingerprint_environment_sandbox_provider_config` | ✅ |
| `applyCustomImageTemplateToSandboxConfig` | `apply_custom_image_template_to_sandbox_config` | ✅ |
| `environmentCustomImageTemplateMatchesBaseConfig` | `environment_custom_image_template_matches_base_config` | ✅ |
| `classifyEnvironmentCustomImageConfigChange` | `classify_environment_custom_image_config_change` | ✅ |
| `environmentCustomImageTemplateFromRow` | `environment_custom_image_template_from_row` | ✅ |
| `resolveActiveEnvironmentCustomImageTemplateForRuntime` (DB) | out-of-scope (依赖 `Db`) | ⏸ |

### 4. 复用既有 helper

通过 `pc_secrets::json_schema_secret_refs` 直接复用既有 1:1 parity：

- `read_config_value_at_path(config, dot_path)`
- `write_config_value_at_path(config, dot_path, Option<&Value>)`

避免重复实现 Node `json-schema-secret-refs.ts` 的 5 个 helper。

### 5. 关键实现要点

- `stable_stringify` —— Node 私有 helper，递归按 key 字母序排序后 JSON encode
- `fingerprint` —— sha256(stable_stringify(strip_excluded_paths(config)))
- `is_valid_runtime_config_binding_field` —— Node 正则 `/^[A-Za-z_][A-Za-z0-9_-]*$/` 排除 `"provider"`
- `normalize_binding` —— 用 `HashSet` 去重（**不是** `BTreeSet`，保持插入顺序与 Node `Set` 1:1）
- `classify_config_change` —— 拆 5 类 breaking 路径（`provider` / binding.field / unset_fields / SOURCE_FIELDS / templateIdentityPaths）

## 测试结果

### `cargo test -p pc-environment --test custom_image_runtime_tests`

```
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 测试覆盖矩阵

| 类别 | 测试数 | 覆盖点 |
|---|---:|---|
| Constants | 4 | 4 个常量值精确断言 |
| read_kind | 2 | 已知 kind / 未知 + null |
| default_binding | 5 | 4 种 kind + null + 无效 kind |
| normalize_binding | 4 | 最小 / with unset / 无效 field / 非对象 |
| resolve_binding | 3 | metadata 优先 / 回落 default / null metadata |
| stable_stringify | 2 | key 排序 / 嵌套 |
| fingerprint | 4 | 跨 key 顺序稳定 / exclude 命中 / exclude 不命中 / 无 excludes |
| apply_template | 5 | snapshot / image / provider_template / 无 ref / metadata 覆盖 |
| matches_base_config | 4 | 缺指纹 → true / exclude 不影响 / secret-ref exclude / 真实字段改动 |
| classify_change | 5 | 已 detached → none / next 也匹配 → none / provider 改 → breaking / binding 改 → breaking / 非 breaking 字段 → relinkable + templateIdentityPaths 强制 breaking |
| row_mapper | 2 | 基本映射 / 未知 kind 归一化 |

### 关键 bug & 学习

**parity bug 修正**：`normalizeEnvironmentCustomImageRuntimeConfigBinding` 中 Node 用
`Array.from(new Set(...))` —— `Set` 保持**插入顺序**。初版 Rust 用 `BTreeSet`
按字母排序，结果 `unsetFields` 顺序与 Node 不一致。修正：换 `HashSet + Vec` 组合，
插入顺序与 Node `Set` 1:1。这条经验继续支持 R673 的"validators 先具体后放宽"
教训 —— **集合类型的 parity 要特别注意插入顺序与去重语义**。

## 回归

- `cargo test -p pc-environment --lib`：**7 passed**（R671 runtime_parity 仍 OK）
- `cargo test -p pc-environment --test config_tests`：**44 passed**（R675 仍 OK）
- `cargo test -p pc-http --lib`：**495 passed / 0 failed**
- `cargo test -p pc-plugin-database`：**47 passed / 0 failed**（R673 仍 OK）
- `cargo build -p pc-server`：成功（无新 warning）

## 综合覆盖度（更新至 R677）

| 维度 | R676 终态 | R677 终态 |
|---|---|---|
| pc-environment lib tests | 7 | **7** |
| pc-environment config_tests | 44 | **44** |
| pc-environment custom_image_runtime_tests | — | **41 passed** |
| pc-http lib tests | 495 | **495** |
| pc-plugin-database tests | 47 | **47** |
| pc-server build | ✅ | ✅ |

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（41 unit test PASS + 495 + 47 + 7 regression） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅ |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R678** | 找下一个完全未复刻 Node service parity 缺口（候选：`<environment-custom-image-terminal-sessions.ts (353 行) / `plugin-environment-driver.ts (570 行)`） |
| **R679** | pc-server prod-mode 真实启动 + OAUTH 模拟 |
