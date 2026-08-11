# R556 — pc-feature-catalog（Node feature-catalog.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/feature-catalog.ts` (282 LOC) 完整复刻到新 crate
`crates/pc-feature-catalog`。workspace crates 97 → **98**。

## 设计原则

### 1. 静态 `&[(&str, FeatureCatalogEntry)]` 替代 `Record<key, entry>`
- Node 用 `Record<InstanceFeatureKey, FeatureCatalogEntry>` + `as const` 推断类型
- Rust 用 `const &[(&'static str, FeatureCatalogEntry)]` — 编译期常量、零运行时分配
- 业务侧通过 `lookup_feature(key)` 访问单个 entry

### 2. enum `FeatureTier` 替代字符串字面量
- `FeatureTier::Preference / Managed / Floor` + `as_str()` / `parse()` round-trip
- 编译期保证只有合法 tier

### 3. `serde_json::Value` 替代 zod schema
- `build_feature_catalog_artifact(catalog_version)` 返回 `serde_json::Value`
- 业务侧可用 `serde_json::to_string_pretty` 序列化
- `render_feature_catalog_artifact(catalog_version)` 提供 deterministic JSON 输出（2-space indent + 末尾 newline）

### 4. `instance_feature_keys()` 排序镜像 Node
- `Object.keys(INSTANCE_FEATURE_CATALOG).sort()` → `Vec<&'static str>` 排序后列表
- 排序保证 artifact 序列化稳定

### 5. 解耦 zod schema 依赖
- Node 用 `z.infer<typeof instanceExperimentalSettingsSchema>` 派生 key 类型
- Rust 用显式 `&'static str` 常量作为 key（更简单，无 zod 依赖）
- 业务侧维护 26 个 key 的 catalog 时编译期即可捕获拼写错误

## 公开 API

```rust
pub const FEATURE_TIERS: [&str; 3]  // ["preference", "managed", "floor"]

pub enum FeatureTier { Preference, Managed, Floor }
impl FeatureTier { pub fn as_str / pub fn parse }

pub struct FeatureCatalogEntry {
    pub title: &'static str,
    pub description: &'static str,
    pub tier: FeatureTier,
    pub cloud_default: bool,
    pub self_hosted_default: bool,
}

pub const INSTANCE_FEATURE_CATALOG: &[(&'static str, FeatureCatalogEntry)]  // 26 个 flag

pub fn lookup_feature(key: &str) -> Option<&'static FeatureCatalogEntry>
pub fn instance_feature_keys() -> Vec<&'static str>  // 排序后
pub fn build_feature_catalog_artifact(catalog_version: &str) -> Result<serde_json::Value, &'static str>
pub fn render_feature_catalog_artifact(catalog_version: &str) -> Result<String, &'static str>
```

## 与上游 Node 差异

- **&'static str 替代 zod schema 派生 key**：无 zod 依赖
- **serde_json::Value**：artifact 用 JSON Value 而非 zod inferred type
- **Result 错误**：替代 Node `throw new Error(...)`

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-feature-catalog` | **23 passed** (8 internal + 15 integration) |
| `cargo fmt -p pc-feature-catalog` | ✅ 通过 |
| `cargo clippy -p pc-feature-catalog --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（23 个）

- **constants** (1): FEATURE_TIERS 三值
- **enum round-trip** (1): tier 3 个
- **catalog size** (1): 26 个
- **lookup** (3): 全部 entry / 已知 key / 未知 key
- **keys sorted** (1): 字母排序
- **build artifact** (3): empty version 拒绝 / 包含所有 key / 数量一致
- **render artifact** (3): 拒绝空 / deterministic / valid JSON
- **data consistency** (2): cloud/self-hosted 默认值 / tier 合法
- **known flags** (1): 7 个关键 flag 都在

## 集成待办（不在本轮范围）

- `pc-config`：用 `lookup_feature` 在 experimental settings UI 渲染 metadata
- `pc-cloud` / 云端管理面板：消费 `build_feature_catalog_artifact` 写 release artifact
- `pc-pipelines` / `pc-environments`：用 `tier` 决定是否在云端 / 自托管暴露某 flag
- 端到端：写一个 `feature-catalog.json` → 用 `render_feature_catalog_artifact` 输出 diff
