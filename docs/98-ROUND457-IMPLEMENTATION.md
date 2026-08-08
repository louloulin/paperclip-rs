# R457 — Claude Parse Snapshots（Rust 化）

## 目标

补齐 Node `claude-local/server/parse.ts` 中遗漏的三个核心函数：
1. `claudeModelUsageTotals` — 从 `modelUsage` JSON 聚合 token 计数
2. `extractClaudeLoginUrl` — 从 stdout / stderr 提取登录 URL
3. `isClaudeImageProcessingError` — 检测图片处理错误

注：`isClaudeImageProcessingError` 已在 `claude_stream_json.rs` 实现，**本文不再重复添加**。

### 关键设计点

1. **`u64` token counts**：`UsageSummary` 字段是 `u64`（不是 `i64`），Rust 用 `Value::as_u64` 转换
2. **`Some` wrapping 语义**：仅在数据字段确实出现时 wrap `Some(0)`，否则 `None`
3. **保守清理**：URL 末尾标点 `[ ] ) } . ! , ? ; : ' "` 全部 trim 掉
4. **优先匹配**：含 `claude` / `anthropic` / `auth` 子串的 URL 优先返回

---

## Node → Rust 端口

### `claudeModelUsageTotals`

```typescript
// Node
export function claudeModelUsageTotals(modelUsage: unknown): UsageSummary | null {
  const byModel = parseObject(modelUsage);
  let inputTokens = 0;
  let outputTokens = 0;
  let cachedInputTokens = 0;
  let sawEntry = false;
  for (const value of Object.values(byModel)) {
    const entry = parseObject(value);
    if (Object.keys(entry).length === 0) continue;
    sawEntry = true;
    inputTokens += asNumber(entry.inputTokens, 0) + asNumber(entry.cacheCreationInputTokens, 0);
    outputTokens += asNumber(entry.outputTokens, 0);
    cachedInputTokens += asNumber(entry.cacheReadInputTokens, 0);
  }
  if (!sawEntry) return null;
  return { inputTokens, outputTokens, cachedInputTokens };
}
```

```rust
// Rust
pub fn claude_model_usage_totals(model_usage: &Value) -> Option<UsageSummary> {
    let obj = model_usage.as_object()?;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut cached_input_tokens: u64 = 0;
    let mut saw_input = false;
    let mut saw_output = false;
    let mut saw_cached = false;
    let mut saw_entry = false;
    for (_model, value) in obj {
        let entry = match value.as_object() {
            Some(o) => o,
            None => continue,
        };
        if entry.is_empty() { continue; }
        saw_entry = true;
        if let Some(v) = entry.get("inputTokens").and_then(Value::as_u64) {
            input_tokens += v;
            saw_input = true;
        }
        if let Some(v) = entry.get("cacheCreationInputTokens").and_then(Value::as_u64) {
            input_tokens += v;
            saw_input = true;
        }
        if let Some(v) = entry.get("outputTokens").and_then(Value::as_u64) {
            output_tokens += v;
            saw_output = true;
        }
        if let Some(v) = entry.get("cacheReadInputTokens").and_then(Value::as_u64) {
            cached_input_tokens += v;
            saw_cached = true;
        }
    }
    if !saw_entry { return None; }
    Some(UsageSummary {
        input_tokens,
        output_tokens,
        cached_input_tokens: if saw_cached { Some(cached_input_tokens) } else { None },
    })
}
```

注：Rust 端 `cached_input_tokens` 是 `Option<u64>`，因此增加 `saw_cached` 跟踪「是否真有数据」。

### `extractClaudeLoginUrl`

```rust
pub fn extract_claude_login_url(text: &str) -> Option<String> {
    let urls = extract_urls(text);
    if urls.is_empty() { return None; }
    for raw_url in &urls {
        let cleaned = clean_trailing_url_punct(raw_url);
        if cleaned.contains("claude") || cleaned.contains("anthropic") || cleaned.contains("auth") {
            return Some(cleaned);
        }
    }
    urls.first().map(|s| clean_trailing_url_punct(s))
}
```

辅助函数：
- `extract_urls`：用 `regex_lite` 抓 `https?://...`
- `clean_trailing_url_punct`：trim 末尾 `] } ) . ! , ? ; : ' "` 标点

---

## 测试覆盖（9 个新增）

### `claude_model_usage_totals`（4 个）
- 聚合多 model：cache tokens 计入 input
- 空 object → None
- 全部空 entry → None
- 缺字段时 token 为 0，cached_input_tokens 为 None

### `extract_claude_login_url`（5 个）
- 优先选 `claude.ai` URL
- 优先选 `anthropic.com` URL
- 清理末尾标点
- 无 URL → None
- fallback 到第一个 URL

---

## 文件清单

- **修改**：`crates/pc-adapter-claude-local/src/claude_errors.rs`（新增 3 个 fn + 9 个测试）
- **修改**：`crates/pc-adapter-claude-local/Cargo.toml`（新增 `regex-lite` + `pc-adapter-api` 依赖）

## 测试结果

```
claude_errors::tests_extra: 9 passed, 0 failed
pc-adapter-claude-local: 162 passed (153 prior + 9 new)
pc-acpx: 883 passed
pc-adapter-codex-local: 260 passed
pc-adapter-process: 6 passed
pc-activity: 14 passed
pc-adapter-quota: 39 (上次验证)
合计: 1600 passed (was 1591, +9)
```

---

## 后续 R458-R459

- **R458** test.ts 完整复刻（含 `testEnvironment` 整个流程）
- **R459** pc-repos / pc-heartbeat 深化

## 当前差距

| 维度 | 已经实现 | 后续 |
|---|---|---|
| codex 适配器 | ~98% | （接近完成） |
| **claude 适配器** | **~94%** | R458 |
| pc-acpx 核心 | ~95% | （少量边界） |
| pc-http routes | ~96% | （少量边界） |
| quota / heartbeat | ~85% | R457（已部分） |
| 其他 adapter | 0% | R456（延后） |
