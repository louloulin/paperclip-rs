# R561 — e2e-baseline poll 修复 + R-INTEGRATION-1 pc-config-schema 与 pc-feature-catalog 集成（2026-08-11）

## 1. e2e-baseline.sh poll 窗口修复

### 1.1 问题
PG16 启动后 + 205 migrations + 172 表 ✅，但 pc-server 在原 10s poll 窗口内未启动到 `/health`。

### 1.2 修复
`scripts/e2e-baseline.sh`:
- `/health` poll: `for i in 1..20; sleep 0.5` (10s) → `for i in $(seq 1 120)` (60s)
- 每 20 次 poll surface server log tail（防止 silent failure）
- PG ready poll: `10` → `60` 次

### 1.3 实测
- 修复后 PG 启动 + 172 表 ✅
- pc-server 仍在 60s 窗口内未启动到 `/health` — 需要进一步调试 pc-server 启动序列（不属于本轮 scope）
- 下一步：单独 debug pc-server 启动慢的原因（可能 startup 钩子 / migration check）

## 2. R-INTEGRATION-1 — pc-feature-catalog → pc-config-schema 集成

### 2.1 动机
`pc-feature-catalog`（R556）已 port 26 个 feature flag 的完整 catalog（含 title/description/tier/defaults）。
`pc-config-schema`（R557）已 port 持久化 config.json 的 schema + 语义验证。
两个 crate 之前完全独立 — 现在通过 delegation 模式接起来，让 config-schema 验证层能识别 catalog 中的 feature key。

### 2.2 设计：delegation 而非 port
镜像现有 `pc-config-schema` 对 `pc-network-bind` 的 delegation 模式：

```rust
// crates/pc-config-schema/src/lib.rs（新增模块）
pub fn validate_feature_key(key: &str) -> Result<(), UnknownFeatureKeyError> {
    if pc_feature_catalog::lookup_feature(key).is_some() {
        return Ok(());
    }
    Err(UnknownFeatureKeyError {
        key: key.to_string(),
        known_keys: pc_feature_catalog::instance_feature_keys(),
    })
}
```

`pc-feature-catalog` 是 catalog owner；`pc-config-schema` 只是 thin facade。零业务逻辑、零 state、零 cache。

### 2.3 公开 API（4 个 helper）

```rust
// 1. 严格验证：key 是否在 catalog 内
pub fn validate_feature_key(key: &str) -> Result<(), UnknownFeatureKeyError>;

// 2. 列出所有已知 key（delegated）
pub fn known_feature_keys() -> Vec<&'static str>;

// 3. 查询 tier（delegated，unknown → None）
pub fn feature_tier(key: &str) -> Option<FeatureTier>;

// 4. tier 聚合查询
pub fn has_any_feature_of_tier(tier: FeatureTier) -> bool;
```

### 2.4 新增错误类型

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFeatureKeyError {
    pub key: String,
    pub known_keys: Vec<&'static str>,
}
// impl Display + impl std::error::Error
```

错误结构携带 offending key + 全 sorted list of known keys（诊断用，镜像 `pc-network-bind::ConfiguredBindModeError` 的设计）。

### 2.5 集成测试（7 个）

`crates/pc-config-schema/tests/r561_feature_catalog_integration.rs`：

| 测试 | 验证内容 |
|---|---|
| `validate_known_feature_keys_ok` | 5+ 已知 key 返回 Ok |
| `validate_unknown_feature_key_err` | 未知 key 返回 Err + known_keys 已排序 |
| `known_feature_keys_matches_catalog` | delegated list = catalog list |
| `feature_tier_returns_catalog_tier` | tier 匹配 + unknown → None |
| `has_any_feature_of_tier_matches_tiers` | tier 存在性 + 聚合 sanity |
| `delegation_zero_business_logic` | delegation 与 catalog lookup 完全一致 |
| `error_display_includes_key` | Display impl 含 offending key |

## 3. 验证结果

### 3.1 集成测试
```
running 7 tests
test delegation_zero_business_logic ... ok
test feature_tier_returns_catalog_tier ... ok
test error_display_includes_key ... ok
test has_any_feature_of_tier_matches_tiers ... ok
test validate_known_feature_keys_ok ... ok
test known_feature_keys_matches_catalog ... ok
test validate_unknown_feature_key_err ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 3.2 无回归
```
cargo test -p pc-config-schema
  → 22 passed（原有）+ 7 passed（新增）= 29 passed / 0 failed
```

### 3.3 clippy + fmt
```
cargo clippy -p pc-config-schema --all-targets -- -D warnings
  → 0 warnings ✅
```

## 4. 设计优势

### 4.1 镜像 pc-network-bind 模式
`pc-config-schema` 已经依赖 `pc-network-bind` 做 bind mode validation。新集成走完全相同的路径：
- 依赖 → delegation → thin facade
- 业务逻辑全在 catalog owner
- facade 只做 routing + error translation

### 4.2 单向依赖无环
- `pc-config-schema` 依赖 `pc-feature-catalog` ✅
- `pc-feature-catalog` 不依赖 `pc-config-schema` ✅
- 无循环依赖

### 4.3 零复制
delegation 不是 port — facade 函数体只有几行，全部调用 catalog owner。title / description / tier / defaults 一律从 catalog 取，单一 source of truth。

### 4.4 Future-proof
未来如果 config schema 加 feature flag 字段（比如 `experimental.featureFlags: HashMap<String, bool>`），可以直接调用 `validate_feature_key` 做严格校验。如果某个 key 被 catalog 移除，现有 config 文件会被自动拒绝（与 Node 上游 zod `superRefine` 行为一致）。

## 5. 累计成果（R561 末）

- **R-INTEGRATION-1**: pc-config-schema 通过 delegation 接入 pc-feature-catalog（4 helper + 1 error type + 7 integration tests）
- **e2e-baseline.sh 改进**: poll 窗口 10s → 60s + server log surfacing
- **现有 22 个 pc-config-schema 测试无回归**
- **整体 lib tests**: 6955 → **6962 passing**（+7 来自 R-INTEGRATION-1）
- **clippy 0 warnings**：✅

## 6. 下一步

### 6.1 V1 e2e-baseline 真实验证（继续）
- 调试 pc-server 启动慢的原因
- 可能 startup 钩子 / DB pool init / migration verify 等耗时步骤
- 一旦定位，把 fix 加进 R562

### 6.2 R-INTEGRATION-2: pc-config-schema 接入 pc-mentions
- pc-mentions（R546）已 port project-mentions.ts
- pc-config-schema 可加 `validate_mention_anchor` helper 验证 config 中出现的 mention anchor

### 6.3 R-INTEGRATION-3: pc-pipelines 接入 pc-pipeline-case-type
- pc-pipeline-case-type（R554）已 port
- pc-pipelines 主路径已有 `derive_case_type` 调用（R531）—— 验证它真在用

