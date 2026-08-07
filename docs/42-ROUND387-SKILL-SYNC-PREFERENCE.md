# R387 — Skill-Sync Preference (Node parity surface)

## 目标

按 `comet-open` + `RTK` 思路,精确镜像 Node `server-utils.ts` 中
`readPaperclipSkillSyncPreference` (L2794-2834) /
`canonicalizeDesiredPaperclipSkillReference` (L2842-2857) /
`resolvePaperclipDesiredSkillNames` (L2858-2869) /
`writePaperclipSkillSyncPreference` (L2870-2899) 四个函数,新建独立
模块 `pc-acpx::skill_sync_preference`,保持高内聚低耦合 — 一个模块
一个文件,纯函数 / 零 I/O / 零 unsafe。

## 范围

- 新增 `crates/pc-acpx/src/skill_sync_preference.rs`(839 行含 26 单测)
- `crates/pc-acpx/src/lib.rs` 增加模块声明 + re-export
- 新增 `crates/pc-acpx/tests/round387_skill_sync_preference.rs`(284 行 12 集成测试)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  L2794-2834 / L2842-2857 / L2858-2869 / L2870-2899 行精确对齐

## 复刻的 4 个 Node 函数 / 类型

### 1. `PaperclipDesiredSkillEntry` (Node L2794-2834)
```rust
pub struct PaperclipDesiredSkillEntry {
    pub key: String,
    pub version_id: Option<String>,
}
```

### 2. `readPaperclipSkillSyncPreference` (L2794-2834)
```rust
pub fn read_paperclip_skill_sync_preference(
    config: &Map<String, Value>,
) -> SkillSyncPreference
```
- `explicit`:`Map.contains_key("desiredSkills")`(等价 Node
  `Object.prototype.hasOwnProperty.call(raw, "desiredSkills")`)
- `desired_skills` / `desired_skill_entries`:dedup by first-seen key
- string input → `{ key, version_id: None }`
- object input → `{ key, version_id }`(versionId string 非空 → Some,否则 None)
- non-string non-object items → silently dropped
- raw 非 object / array → 全 default

### 3. `canonicalizeDesiredPaperclipSkillReference` (L2842-2857)
```rust
pub fn canonicalize_desired_paperclip_skill_reference(
    reference: &str,
    available_entries: &[AvailableSkillEntry],
) -> String
```
1. trimmed-lowercased reference
2. exact key match (case-insensitive) → entry.key
3. single runtime-name match → entry.key
4. single slug (last `/`-segment of key) match → entry.key
5. 无匹配 → trimmed-lowercased reference 本身(**不**返回 "")

### 4. `resolvePaperclipDesiredSkillNames` (L2858-2869)
```rust
pub fn resolve_paperclip_desired_skill_names(
    config: &Map<String, Value>,
    available_entries: &[AvailableSkillEntry],
) -> Vec<String>
```
- !explicit → []
- map → canonicalize
- filter 空字符串
- dedup(first-seen 顺序)

### 5. `writePaperclipSkillSyncPreference` (L2870-2899)
```rust
pub fn write_paperclip_skill_sync_preference(
    config: &Map<String, Value>,
    desired_skills: &[SkillSyncPreferenceInput],
) -> Map<String, Value>
```
- 不 mutate input(返回新 Map)
- 保留 `paperclipSkillSync` 其它字段
- 任一 entry 有 `version_id` → emit typed shape(`{ key, versionId }` 数组,`None` 字段显示为 `null`)
- 全 `None` → emit string shape(`["key1", "key2"]`)

## 关键设计决策

### 用 `serde_json::Map` 而非自定义类型
Node `Record<string, unknown>` 等价 Rust `Map<String, Value>`。
`pc-acpx` 已大量使用 `serde_json::Value` 形式 config
(`build_runtime`, `build_prompt`, `startup_timing`, `normalize`,
`transcript`),保持一致风格。

### 顺序保留 vs HashMap
Rust `HashMap` 不保证迭代顺序。原实现用 `HashMap` dedup 导致
Node 期望的 `[alpha, beta]` 变成 `[beta, alpha]`。改用
`HashSet<String> seen + Vec<PaperclipDesiredSkillEntry> entries`
保持 first-seen 顺序,与 Node `byKey.has(entry.key) ? skip : set`
一致。

### `SkillSyncPreferenceInput` enum
镜像 Node union `string | PaperclipDesiredSkillEntry`,提供
`Key(String)` + `Entry(PaperclipDesiredSkillEntry)` 变体,
让 caller 自由混合两种输入形态。

### `VersionId: None` emit 为 `Value::Null`
Node typed shape `{ key, versionId: null }` 保留 `versionId` 字段,
即使值为 null。Rust `Value::Null` 完美镜像,JSON 序列化后字段存在。

### 不可变更新
`write_paperclip_skill_sync_preference` 不修改 input config,
返回新 `Map`,匹配 Node `{ ...config }` + `{ ...raw }` spread 语义。
集成测试 `write_does_not_mutate_input_config` 显式验证。

### `unsafe_code = "forbid"` 兼容
纯 `serde_json::Map` + `Vec` 操作,无任何 unsafe。

## 测试

- 26 个新单元测试注入 `skill_sync_preference::tests`:
  - 7 个 `read_paperclip_skill_sync_preference`(missing / non-object / empty explicit / strings / typed entries / dedup / non-string items)
  - 7 个 `canonicalize_desired_paperclip_skill_reference`(blank / exact / runtime_name / ambiguous / slug / unresolved / trim)
  - 4 个 `resolve_paperclip_desired_skill_names`(not explicit / empty explicit / canonicalize / dedup)
  - 8 个 `write_paperclip_skill_sync_preference`(insert missing / preserve / string list / typed / dedup / trim / compact string / no mutate)
- 12 个新集成测试在 `tests/round387_skill_sync_preference.rs`:
  - 4 个 explicit / implicit 语义(含 `desiredSkills: null` hasOwnProperty parity)
  - 2 个 end-to-end round-trip(读 → 解析 → 写 → 再读)
  - 4 个 canonicalisation 规则(exact > runtime_name / unresolved / 嵌套 slug)
  - 2 个 dedup 顺序不变量(读 / 写 first-seen wins)

合计 R387 新增 **38 个测试**,全部绿色。

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:**694 个 pc-acpx tests 通过** (R386 是 656,+38),0 失败 0 回归。

```
cd paperclip-rs && cargo fmt --check
```

clean。

## 下一步

完成 R387 后,adapter-utils 剩余未实现的纯函数模块还剩:

### R388 候选(skill snapshot,复杂)
- `buildRuntimeMountedSkillSnapshot` (L2491-2608)
- `buildPersistentSkillSnapshot` (L2609-2734)

### R389 候选(async skill materialize,极复杂)
- `materializePaperclipSkillCopy` (L3038+)

### 后续路线
- R390+: 13 个 adapter stubs(`pc-adapter-gemini-local` /
  `pc-adapter-grok-local` / `pc-adapter-opencode-local` /
  `pc-adapter-pi-local` / `pc-adapter-cursor-cloud` / 等)的实质实现
- R400+: secrets AWS/GCP/Vault 真实解密 + plugin worker→host 回调
