# R492 — pc-decisions::pure 纯函数集（8 个新 `pub fn` + 36 个测试）

> 配套: `proposal.md` V8/V9 + `design.md`。
> 与上游 `paperclip/server/src/services/decisions.ts` 行为契约 1:1 对齐。

## 改动

### 1. `crates/pc-secrets/src/decision_signing/canonical.rs`
- `pub(super) fn canonical` → `pub fn canonical`（增加 doc comment）。
- `fn canonical_number` → `pub fn canonical_number`。
- `crates/pc-secrets/src/decision_signing/mod.rs` 加 `pub use canonical::{canonical, canonical_number};`。
- `crates/pc-secrets/src/lib.rs` 在 `decision_signing` re-export 列表里加 `canonical, canonical_number,`。

### 2. `crates/pc-decisions/src/pure.rs`（新增，700 行）
8 个新 `pub fn` + 1 个 `pub enum` + 1 个 trait impl：

| 新 API | 对齐 Node | 用途 |
|---|---|---|
| `EffectAction` enum | `targetActions` 集合元素 | 标记 `issue:comment` / `issue:mutate` |
| `classify_effect_type(&str) -> EffectAction` | 隐式逻辑 | 决策效果归类 |
| `effect_target_ids(&Value) -> Vec<String>` | `effectTargetIds` | 单效果目标 id 列表 |
| `target_ids(&Value) -> Vec<String>` | `targetIds` | 全 option 去重 id 列表（首次出现序）|
| `target_actions(&Value) -> BTreeMap<String, BTreeSet<EffectAction>>` | `targetActions` | 每个目标 id 上的动作集合 |
| `same_ids(&[String], &[String]) -> bool` | `sameIds` | 集合等值比较（去重+长度）|
| `same_input_values(&Value, &Value) -> bool` | `sameInputValues` | input values 对象等值 |
| `interpolate(&str, &HashMap<String,String>) -> String` | `interpolate` | `{{input.<id>}}` 模板替换 |
| `find_commit_sha(&Value) -> Option<String>` | `findCommitSha` | 在 JSON 里递归找 commit SHA |
| `build_spec_envelope(&str, &Value, &Value) -> String` | `spec(...)` + `canonical` | 构造已排序的 spec 信封（直接走 `pc_secrets::canonical`）|
| `canonical_decision_value(&Value) -> String` | re-export | 暴露 `pc_secrets::canonical` 给业务层 |
| `json_copy<T: Serialize+DeserializeOwned>(&T) -> T` | `jsonCopy` | JSON 深拷贝（用 `serde_json` round-trip）|

### 3. `crates/pc-decisions/src/lib.rs`
- 新增 `pub mod pure;`（在 `pub mod bundle_service;` 之后）。
- 新增 `pub use pure::*;`（在 `pub use wakeup::*;` 之前）。

## 设计要点
- **高内聚**：所有 8 个 `pub fn` 都在 `pure.rs`，不依赖 `Db` / `DecisionRepo` / `DecisionSigningService`，可直接单测。
- **低耦合**：只依赖 `serde_json::Value`、`std::collections`、`pc_secrets::canonical`（共享算法）。无 `async`，无 `sqlx`。
- **Node 1:1 对齐**：`effect_target_ids` 顺序、`target_ids` 去重策略、`same_ids` 同时拒绝 `right` 内重复、`same_input_values` 的 `null` ≡ `{}`、interpolate 的 `{{input.<id>}}` 形态、`find_commit_sha` 五个 key 顺序与候选 hex 长度——全部与 Node 原函数一致。
- **复用 `pc_secrets::canonical`**：避免在 `pure.rs` 里再加一个 `ryu_js` 依赖；通过 `pub use` 让上层调用方不需要额外导入路径。
- **`#[must_use]` 选择性使用**：纯函数返回 `String` / `bool` / `Vec` 等已经暗示要使用，不加 `must_use` 以避免 pedantic `double_must_use` 警告。

## 测试覆盖（36 个新测试，全部通过）

```
cargo test -p pc-decisions --lib
test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

- `classify_effect_type`：comment_on_issue / 未知 / 空字符串 / future 类型 默认 `Mutate`。
- `EffectAction::as_str`：round-trip。
- `effect_target_ids` 6 测：basic / create_issue（parent + blockedBy）/ resolve_blocker / 去重 / 跳过空串 / 未知 shape。
- `target_ids` 3 测：空 options / 首次出现序 / 跳过无 effects 的 option。
- `target_actions` 2 测：per-target 合并 / 空 options。
- `same_ids` 4 测：等集合 / 长度不等 / 右侧重复 / 双空。
- `same_input_values` 5 测：等对象 / 值不等 / null≡{} / null vs 非空 / 非对象拒绝。
- `interpolate` 4 测：替换 / 无占位 / 形态外原样 / unicode 安全。
- `find_commit_sha` 6 测：top-level key / 大小写 / 5 个候选 key / 嵌套 / 数组 / 拒绝短/非 hex / 标量。
- `build_spec_envelope` 1 测：key 排序后字符串与 Node `canonical` 一致。
- `canonical_decision_value` 1 测：与 `pc_secrets::canonical` 同输出。
- `json_copy` 1 测：嵌套对象 round-trip。

## 验证
- `cargo check -p pc-decisions` 0 errors (1 pre-existing warning 在 pc-repos，未变)。
- `cargo test -p pc-decisions --lib` 42 passed (含 36 新测试)。
- `cargo test -p pc-secrets --lib` 143 passed (canonical visibility 调整无回归)。
- `cargo fmt -p pc-decisions --check`：本轮 `pure.rs` / `lib.rs` 改动无 diff；2 处 pre-existing diff（`bundle_service.rs:153` 行宽 + `lib.rs:17` 排序）不在本轮范围内。

## 下一步候选
1. `pc-decisions` `bundle_service.rs` 把 `canonical_decision_value` / `interpolate` 接入（消除内部 `serde_json::to_value` 直接 `format!` 路径）。
2. `pc-routines` 任何一处用 `{{input.x}}` 模板的地方调用 `interpolate`。
3. `pc-decisions::lib.rs` 主服务（`create` / `decide` / `sweepExpired`）逐步用 `target_ids` / `target_actions` 替换手写 `for ... for ...` 集合计算。
4. `pc-decision-training` 复刻 `find_commit_sha`（目前 `decision-training.ts` 已存在但 Rust 端未实现，复用此函数即可）。
5. `pc-companies` main lib.rs（当前 0 tests）。
