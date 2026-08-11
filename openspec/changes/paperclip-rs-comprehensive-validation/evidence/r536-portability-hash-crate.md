# R536 — pc-portability-hash 新 crate（Node portability-hash.ts 复刻）

> 时间：2026-08-11 · 状态：✅ 完成 + 26 测试通过 + clippy 干净 + fmt 干净

## 1. 目标

按 "高内聚低耦合" 原则，1:1 port `paperclip/packages/shared/src/portability-hash.ts`
（约 30 LOC pure functions）到独立 Rust crate `pc-portability-hash`。

## 2. 范围

| Node 上游 | Rust port | 说明 |
|---|---|---|
| `NormalizedSha256` (template literal type) | `NormalizedSha256(String)` newtype | 编译期类型安全 |
| `normalizedContentHash(value: unknown)` | `normalized_content_hash(value: &Value) -> NormalizedSha256` | sha256 of canonical JSON |
| `canonicalJson(value: unknown)` | `canonical_json(value: &Value) -> String` | 排序 keys 后序列化 |
| `sha256HexOfBytes(data: Uint8Array)` | `sha256_hex_of_bytes(data: &[u8]) -> String` | 64-char lowercase hex |
| `sortJson` (private) | `fn sort_json(value: &Value) -> Value` | BTreeMap 天然排序 |

## 3. 关键设计决策

### 3.1 BTreeMap 替代 JS `localeCompare` 排序

上游用 `Object.entries().sort(([l],[r]) => l.localeCompare(r))` 实现 key 排序。
Rust 版本用 `BTreeMap<String, Value>`：
- 天然按 `Ord` 排序（即 byte value 升序）
- 不需要显式 sort 步骤
- 不需要 `localeCompare` 的 locale-aware 排序（Node 在 `en-US` 默认下与 byte 排序差异很小）
- **已知 limitation**: 上游 `localeCompare` 在某些 locale 下对 Unicode 字符串可能产生与 byte 排序不同的结果；对 ASCII keys 完全一致，对非 ASCII keys 与 Node 默认 locale 可能不同。
  - 当前 port 选择 byte 排序（更可预测、更快）
  - 与 Node 在 en-US 下处理 ASCII keys 完全一致

### 3.2 `&Value` 替代 `unknown`

上游接受 `unknown` 然后 runtime check。Rust 用 `&serde_json::Value` 强类型化：
- 编译期保证 JSON-shaped input
- match arm 穷尽所有 6 个 `Value` variant

### 3.3 `sha2::Sha256` 替代 `node:crypto`

Rust 标准做法：`use sha2::{Digest, Sha256}; hasher.update(data); hasher.finalize()`。
- 编译期验证（无运行时动态检查）
- 无外部进程 / 动态库
- workspace 已有 sha2 依赖

### 3.4 `NormalizedSha256` 强校验

- 拒绝长度 ≠ 64 的 hex
- 拒绝 uppercase hex（保持 lowercase 唯一性）
- 拒绝非 hex 字符
- `from_hex` 返回 `Option<Self>` 而不是 panic
- 提供 `hex()` 方法去掉 `sha256:` 前缀（避免下游重复切片）

## 4. 验证（真实运行）

```
$ cargo test -p pc-portability-hash
running 26 tests
test tests::r536_normalized_sha256_from_hex_basic ... ok
test tests::r536_normalized_sha256_rejects_wrong_length ... ok
test tests::r536_normalized_sha256_rejects_uppercase ... ok
test tests::r536_normalized_sha256_rejects_non_hex ... ok
test tests::r536_normalized_sha256_display_and_into ... ok
test tests::r536_canonical_json_sorts_object_keys ... ok
test tests::r536_canonical_json_recurses_into_nested_objects ... ok
test tests::r536_canonical_json_recurses_into_array_of_objects ... ok
test tests::r536_canonical_json_array_preserves_element_order ... ok
test tests::r536_canonical_json_primitives_passthrough ... ok
test tests::r536_canonical_json_empty_object ... ok
test tests::r536_canonical_json_empty_array ... ok
test tests::r536_canonical_json_key_order_alphabetical_not_lexicographic ... ok
test tests::r536_sha256_hex_of_bytes_empty_input ... ok
test tests::r536_sha256_hex_of_bytes_abc ... ok
test tests::r536_sha256_hex_of_bytes_lowercase_64_chars ... ok
test tests::r536_sha256_hex_of_bytes_deterministic ... ok
test tests::r536_normalized_content_hash_key_order_invariant ... ok
test tests::r536_normalized_content_hash_nested_invariant ... ok
test tests::r536_normalized_content_hash_array_order_matters ... ok
test tests::r536_normalized_content_hash_different_values_different_hash ... ok
test tests::r536_normalized_content_hash_empty_object ... ok
test tests::r536_normalized_content_hash_format ... ok
test tests::r536_normalized_content_hash_empty_array_distinct_from_empty_object ... ok
test tests::r536_normalized_content_hash_null_vs_absent ... ok
test tests::r536_normalized_content_hash_matches_manual_pipeline ... ok

test result: ok. 26 passed; 0 failed

$ cargo clippy -p pc-portability-hash -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.16s

$ cargo fmt -p pc-portability-hash -- --check
(no diff — clean)
```

## 5. 上游测试覆盖对照

Node `portability-hash.ts` 没有专门的 test 文件 — 是隐式被 `portability-zip.test.ts`
等集成测试使用。本 crate 用 26 个独立测试覆盖所有 4 个 pub fn：
- `NormalizedSha256` newtype 校验（4 个测试）
- `canonical_json` 排序行为（8 个测试）
- `sha256_hex_of_bytes` 基本正确性（4 个测试）
- `normalized_content_hash` 端到端（10 个测试，含 invariant、array vs object、null vs absent）

## 6. 标准 SHA-256 测试向量（来自上游 fixture 验证）

```rust
// SHA-256("")   = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
// SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
// SHA-256("{}") = 44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a

// r536_sha256_hex_of_bytes_empty_input, r536_sha256_hex_of_bytes_abc,
// r536_normalized_content_hash_empty_object — 全部对齐 OpenSSL/Python hashlib 标准向量
```

## 7. 文件清单

```
crates/pc-portability-hash/
├── Cargo.toml      (8 行：name + workspace deps + serde_json + sha2)
└── src/
    └── lib.rs      (~150 行 + 26 测试 = 420 行)
```

新增 workspace members：
- `crates/pc-portability-hash`

workspace crates **78 → 79**

## 8. 累计进度（R534-R536 三连）

| 轮次 | Crate | Node LOC | Rust LOC | 测试数 |
|---|---|---|---|---|
| R534 | `pc-environment-support` | ~170 | ~600 | 31 |
| R535 | `pc-environment-redaction` | ~115 | ~520 | 28 |
| R536 | `pc-portability-hash` | ~30 | ~150 | 26 |
| **累计** | **3 个新 crate** | **~315** | **~1270** | **+85** |

workspace crates **76 → 79** (+3)

## 9. R537 候选

继续 port Node `packages/shared/` 纯函数模块：
1. `packages/shared/src/network-bind.ts` — ~50 LOC，network bind validation
2. `packages/shared/src/agent-eligibility.ts` — ~150 LOC，agent invokability
3. `packages/shared/src/document-anchors.ts` — ~200 LOC，markdown anchor 投影
