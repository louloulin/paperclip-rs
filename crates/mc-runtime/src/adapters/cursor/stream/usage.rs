//! 用量归集：顶层 camelCase 四元组、嵌套对象的 legacy 兼容读法、按模型展开成
//! [`ModelUsage`] 行。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 = `docs/32`
//! §30 的 **D10**。**纯移动**：函数体与签名逐字未改。

use super::{tokens, CursorEvent, ModelUsage, TokenUsage, Value};
use std::collections::BTreeMap;

/// `result` 事件的顶层 camelCase 用量（四个字段全零 / 全缺 ⇒ `None`）。
pub(super) fn top_level_usage(event: &CursorEvent) -> Option<TokenUsage> {
    let usage = tokens(
        event.input_tokens.unwrap_or(0),
        event.output_tokens.unwrap_or(0),
        event.cache_read_tokens.unwrap_or(0),
        event.cache_write_tokens.unwrap_or(0),
    );
    if usage.total_tokens == 0 {
        None
    } else {
        Some(usage)
    }
}

/// 嵌套用量对象的兼容读法（上游 `cursorUsage.UnmarshalJSON` 的"第一个非零值"顺序）。
pub(super) fn nested_usage(value: &Value) -> TokenUsage {
    let first = |paths: &[&[&str]]| -> u64 {
        for path in paths {
            let mut cursor = value;
            let mut ok = true;
            for key in *path {
                if let Some(next) = cursor.get(*key) {
                    cursor = next;
                } else {
                    ok = false;
                    break;
                }
            }
            if ok {
                if let Some(number) = cursor.as_u64().filter(|number| *number != 0) {
                    return number;
                }
            }
        }
        0
    };
    tokens(
        first(&[&["input_tokens"], &["inputTokens"]]),
        first(&[&["output_tokens"], &["outputTokens"]]),
        first(&[
            &["cached_input_tokens"],
            &["cachedInputTokens"],
            &["cacheReadTokens"],
            &["cache_read_input_tokens"],
            &["cacheReadInputTokens"],
        ]),
        first(&[
            &["cacheWriteTokens"],
            &["cache_creation_input_tokens"],
            &["cacheCreationInputTokens"],
        ]),
    )
}

/// 按路径取一个 `u64`（取不到算 0）。
pub(super) fn field_u64_any(value: &Value, path: &[&str]) -> u64 {
    let mut cursor = value;
    for key in path {
        match cursor.get(*key) {
            Some(next) => cursor = next,
            None => return 0,
        }
    }
    cursor.as_u64().unwrap_or(0)
}

pub(super) fn usage_map(usage: &BTreeMap<String, TokenUsage>) -> Vec<ModelUsage> {
    usage
        .iter()
        .map(|(model, usage)| ModelUsage {
            model: model.clone(),
            usage: *usage,
        })
        .collect()
}
