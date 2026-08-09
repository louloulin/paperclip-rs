//! `pc-acpx` model id 解析 — 通用 helper。
//!
//! 复刻 Node `packages/adapters/*/src/server/execute.ts` 中多个 adapter
//! 共享的 "provider/model" 拆分逻辑。
//!
//! - pi-local `parseModelProvider`（L69-74）
//! - opencode-local `parseModelProvider`（L70-75）
//!
//! 不依赖 cursor-local 的 `resolveProviderFromModel`（后者带 sonnet/claude/gpt 启发式，
//! 属于 cursor 特有逻辑）。

/// 解析 "provider/model" 形式的 provider 前缀。
///
/// Node 等价：`parseModelProvider`（pi-local / opencode-local）。
///
/// - `None` / 全空白 → `None`。
/// - 不含 `/` → `None`。
/// - `/` 前缀（即 `prefix` 为空）→ `None`。
///
/// 否则返回 trim 后的小写化前缀（与 Node 等价：`trim()` + 大小写由 caller 决定）。
pub fn parse_model_provider(model: Option<&str>) -> Option<String> {
    let trimmed = model?.trim();
    if !trimmed.contains('/') {
        return None;
    }
    let idx = trimmed.find('/').unwrap();
    let prefix = trimmed[..idx].trim();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix.to_owned())
    }
}

/// 解析 "provider/model" 形式的 model id 后缀。
///
/// Node 等价：`parseModelId`（pi-local）。
///
/// - `None` / 全空白 → `None`。
/// - 不含 `/` → 返回整串作为 model id。
/// - `/` 后缀为空 → `None`。
pub fn parse_model_id(model: Option<&str>) -> Option<String> {
    let trimmed = model?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('/') {
        return Some(trimmed.to_owned());
    }
    let idx = trimmed.find('/').unwrap();
    let suffix = trimmed[idx + 1..].trim();
    if suffix.is_empty() {
        None
    } else {
        Some(suffix.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_标准拆分() {
        assert_eq!(
            parse_model_provider(Some("anthropic/claude-sonnet-4")),
            Some("anthropic".to_owned())
        );
    }

    #[test]
    fn provider_大小写保留() {
        // Node 实现：`trim()` 但不强制 lowercase。调用方需要 lowercase 时自己处理。
        assert_eq!(
            parse_model_provider(Some("ANTHROPIC/CLAUDE")),
            Some("ANTHROPIC".to_owned())
        );
    }

    #[test]
    fn provider_无斜杠_None() {
        assert_eq!(parse_model_provider(Some("claude-sonnet-4")), None);
    }

    #[test]
    fn provider_空前缀_None() {
        assert_eq!(parse_model_provider(Some("/model")), None);
    }

    #[test]
    fn provider_空输入_None() {
        assert_eq!(parse_model_provider(None), None);
        assert_eq!(parse_model_provider(Some("")), None);
        assert_eq!(parse_model_provider(Some("   ")), None);
    }

    #[test]
    fn id_标准拆分() {
        assert_eq!(
            parse_model_id(Some("anthropic/claude-sonnet-4")),
            Some("claude-sonnet-4".to_owned())
        );
    }

    #[test]
    fn id_无斜杠_整串() {
        assert_eq!(
            parse_model_id(Some("claude-sonnet-4")),
            Some("claude-sonnet-4".to_owned())
        );
    }

    #[test]
    fn id_空后缀_None() {
        assert_eq!(parse_model_id(Some("anthropic/")), None);
    }

    #[test]
    fn id_空输入_None() {
        assert_eq!(parse_model_id(None), None);
        assert_eq!(parse_model_id(Some("")), None);
        assert_eq!(parse_model_id(Some("   ")), None);
    }

    #[test]
    fn 多斜杠只切第一个() {
        assert_eq!(parse_model_provider(Some("a/b/c")), Some("a".to_owned()));
        assert_eq!(parse_model_id(Some("a/b/c")), Some("b/c".to_owned()));
    }
}
