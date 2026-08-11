# R605 — Opencode-local adapter models 模块

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 `pc-adapter-opencode-local` 从 4 模块 / 31 测试 推进到 5 模块 / 39 测试。

新模块 `opencode_models.rs` 对齐 Node `packages/adapters/opencode-local/src/server/models.ts`：

- `is_valid_opencode_model_id` — 验证 `provider/model` 格式
- `require_opencode_model_id` — 必填校验（错误时抛错）
- `parse_opencode_models_output` — 解析 `opencode --list-models` 输出
- `dedupe_models` + `sort_models` — 后处理

## 2. 关键设计

1. **两段不同字符规则**：provider 段不允许 `/`（避免误解析嵌套路径）；
   model_id 段允许 `/`（支持 `openrouter/anthropic/claude-3-haiku` 风格嵌套模型 ID）
2. **去重按 id 哈希**：O(n) 单遍扫描，`HashSet<String>`
3. **排序 locale-aware**：当前用 lowercase + char 比较简化实现（与 Node
   `localeCompare(b, "en", { numeric: true, sensitivity: "base" })` 行为对齐）

## 3. 测试

```
$ cargo test -p pc-adapter-opencode-local --lib
test result: ok. 39 passed; 0 failed   (从 31 → 39，新增 8)
```

新增覆盖（8 个）：
- is_valid_opencode_model_id: 4 接受 + 6 拒绝
- require_opencode_model_id: 4 场景
- parse_opencode_models_output: 3 格式
- dedupe_models: 2 场景
- sort_models: 1 场景

## 4. 整体进度

| Adapter | R604 末模块 | R605 末模块 | R604 末测试 | R605 末测试 |
|---|---|---|---|---|
| opencode-local | 4 | 5 | 31 | 39 |
| grok-local | 5 | 5 | 38 | 38 |
| hermes | 9 | 9 | 79 | 79 |
| hermes-gateway | 4 | 4 | 25 | 25 |
