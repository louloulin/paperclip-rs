#![forbid(unsafe_code)]

//! 自由文本敏感信息遮罩（free-text redaction），对齐 Node `feedback-redaction.ts` 的 redact_free_text 等 API。
//!
//! 物理位置：`pc-feedback/src/redaction/free_text_pure.rs`。
//! 历史位置：`pc-repos/src/feedback_redaction.rs`（R792A 抽离，纯函数 0 sqlx）。
//!
//! 与 `pc-feedback::redaction::pure` (`RedactionPattern` / `apply_pattern`)
//! 的差异（本模块自带 `redact_free_text` / `sanitize_free_text_value` 等高级入口）：
//! - `redact_free_text` —— 自由文本内的敏感模式 redact
//! - `sanitize_free_text_value` —— JSON Value 的 truncate + redact 组合入口
//! - `truncate_value` / `truncate_string_fields` —— 长字段截断
//! - `RedactionState` —— 累加式的 redact 状态汇总
//!
//! 与 `pc-repos::redact` (结构化 JSON 键值层 redact) 的差异：
//! - `pc-repos::redact` 处理结构化 JSON 的「键值」层 redact
//! - 本模块处理任意自由文本（用户反馈、日志、注释等）的「文本内」敏感模式 redact
//!
//! 模式覆盖（按优先级排序，低 index = 高优先级）：
//! 0. `pem_block`：`-----BEGIN ... -----` ... `-----END ... -----`
//! 1. `bearer_token`：`Bearer xxx`
//! 2. `jwt`：3 段 / 4 段 base64url
//! 3. `github_token`：`gh[pousr]_xxx`（20+ 字符）
//! 4. `provider_api_key`：`sk-xxx` / `sk-ant-xxx`（12+ 字符）
//! 5. `dsn`：postgres / mysql / mongodb / redis / amqp / kafka / nats / mssql 连接串
//! 6. `secret_assignment`：`api_key=xxx` / `token: "xxx"` 等键值对（兜底）
//!
//! 设计：
//! - `RedactionState` 跟踪 redactedFields / truncatedFields / notes / counts
//! - `redact_free_text(input, state?) -> (String, RedactionState)` 入口
//!   - 传入 `Some(&mut state)`：累加到调用方 state；返回克隆的 state（兼容旧调用风格）
//!   - 传入 `None`：使用本地 state，返回新建的 state
//! - `truncate_value(value, max_chars) -> (String, bool)` 长字段截断
//!   - 使用 `…` (U+2026, 3 UTF-8 bytes) 作为省略号，预算 3 字节
//! - 模式合并策略：所有 pattern 一次性扫描，按 (start, priority) 排序，
//!   重叠区间保留高优先级（priority 更小的）pattern
//! - 纯函数无副作用，方便单测

use std::collections::{BTreeMap, BTreeSet};

// ============================================================================
// State
// ============================================================================

/// 单次 redact 调用的状态汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionState {
    pub redacted_patterns: BTreeSet<String>,
    pub truncated_fields: BTreeSet<String>,
    pub notes: BTreeSet<String>,
    pub counts: BTreeMap<String, usize>,
}

impl RedactionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_redaction(&mut self, kind: &str) {
        self.redacted_patterns.insert(kind.to_string());
        *self.counts.entry(kind.to_string()).or_insert(0) += 1;
    }

    pub fn record_truncation(&mut self, key: &str) {
        self.truncated_fields.insert(key.to_string());
        *self.counts.entry(format!("truncated:{key}")).or_insert(0) += 1;
    }

    pub fn record_note(&mut self, note: &str) {
        self.notes.insert(note.to_string());
    }

    /// 序列化为可 JSON 化的视图。
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "redactedPatterns": self.redacted_patterns,
            "truncatedFields": self.truncated_fields,
            "notes": self.notes,
            "counts": self.counts,
        })
    }
}

// ============================================================================
// Patterns
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternId {
    PemBlock,
    BearerToken,
    Jwt,
    GithubToken,
    ProviderApiKey,
    Dsn,
    SecretAssignment,
}

impl PatternId {
    fn kind(self) -> &'static str {
        match self {
            Self::PemBlock => "pem_block",
            Self::BearerToken => "bearer_token",
            Self::Jwt => "jwt",
            Self::GithubToken => "github_token",
            Self::ProviderApiKey => "provider_api_key",
            Self::Dsn => "dsn",
            Self::SecretAssignment => "secret_assignment",
        }
    }
}

struct CompiledPattern {
    id: PatternId,
    regex: regex::Regex,
    /// `true` 表示 replacement 含 `$1` 等反向引用（secret_assignment），需用 `replace`；
    /// `false` 表示纯字面量替换（其他 pattern），可用 `replace` 也可。
    uses_capture: bool,
    /// 字面量 replacement（uses_capture=false 时使用）。
    literal_replacement: &'static str,
}

fn compiled_patterns() -> Vec<CompiledPattern> {
    // 通用 secret 键值对：api_key=xxx / token: "xxx" / auth "xxx" / bearer 'xxx'
    // 注意：`token` 单独作为关键字纳入（虽然 Node 没明确列，但 `token: xxx` 是常见 secret）
    let secret_assignment_re = concat!(
        r#"(?i)\b(api[-_]?key|access[-_]?token|token|auth(?:_?token)?|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)"#,
        r#"\s*[:=]\s*(?:["']([^"'\s,;]+)["']|([^\s,;]+))"#,
    );
    vec![
        CompiledPattern {
            id: PatternId::PemBlock,
            regex: regex::Regex::new(r"-----BEGIN [^-]+-----[\s\S]+?-----END [^-]+-----")
                .expect("PEM_BLOCK_RE"),
            uses_capture: false,
            literal_replacement: "[REDACTED_PEM_BLOCK]",
        },
        CompiledPattern {
            id: PatternId::BearerToken,
            regex: regex::Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._~+/-]+=*")
                .expect("BEARER_TOKEN_RE"),
            uses_capture: false,
            literal_replacement: "Bearer [REDACTED_TOKEN]",
        },
        CompiledPattern {
            id: PatternId::Jwt,
            regex: regex::Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)?\b")
                .expect("JWT_RE"),
            uses_capture: false,
            literal_replacement: "[REDACTED_JWT]",
        },
        CompiledPattern {
            id: PatternId::GithubToken,
            regex: regex::Regex::new(r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b").expect("GITHUB_TOKEN_RE"),
            uses_capture: false,
            literal_replacement: "[REDACTED_GITHUB_TOKEN]",
        },
        CompiledPattern {
            id: PatternId::ProviderApiKey,
            regex: regex::Regex::new(r"\bsk-(?:ant-)?[A-Za-z0-9_-]{12,}\b")
                .expect("PROVIDER_API_KEY_RE"),
            uses_capture: false,
            literal_replacement: "[REDACTED_API_KEY]",
        },
        CompiledPattern {
            id: PatternId::Dsn,
            regex: regex::Regex::new(
                r#"(?i)\b(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp|kafka|nats|mssql):\/\/[^\s<>'")]+"#,
            )
            .expect("DSN_RE"),
            uses_capture: false,
            literal_replacement: "[REDACTED_CONNECTION_STRING]",
        },
        CompiledPattern {
            id: PatternId::SecretAssignment,
            regex: regex::Regex::new(secret_assignment_re).expect("SECRET_ASSIGNMENT_RE"),
            uses_capture: true,
            literal_replacement: "$1=[REDACTED]",
        },
    ]
}

// ============================================================================
// Multi-pattern merge
// ============================================================================

/// 在 input 中找出所有 pattern 的所有匹配，按 (start, priority) 排序，
/// 重叠区间保留 priority 更小（更高优先级）的 pattern。
/// 返回最终保留的 (start, end, pattern_idx) 列表。
fn collect_non_overlapping_matches(input: &str) -> Vec<(usize, usize, usize)> {
    let patterns = compiled_patterns();
    // 收集所有匹配：(start, end, pattern_idx)
    let mut all: Vec<(usize, usize, usize)> = Vec::new();
    for (idx, pattern) in patterns.iter().enumerate() {
        for m in pattern.regex.find_iter(input) {
            all.push((m.start(), m.end(), idx));
        }
    }
    // 排序：先按 start 升序，再按 priority（pattern 索引越小 = 越高优先级）升序
    all.sort_by_key(|&(s, _, p)| (s, p));

    // 合并重叠：循环检查末尾元素，必要时弹出
    let mut result: Vec<(usize, usize, usize)> = Vec::new();
    for (start, end, prio) in all {
        loop {
            match result.last() {
                Some(&(_, last_end, last_prio)) if start < last_end => {
                    // 重叠区间
                    if prio < last_prio {
                        // 新匹配优先级更高 → 弹出旧的，继续检查再前一个
                        result.pop();
                    } else {
                        // 旧匹配优先级更高或相等 → 跳过新匹配
                        break;
                    }
                }
                _ => {
                    // 无重叠，直接 push
                    result.push((start, end, prio));
                    break;
                }
            }
        }
    }
    result
}

// ============================================================================
// Public API
// ============================================================================

/// 对自由文本执行所有已知模式的 redact。
///
/// - `state == Some(&mut s)`：累加到 `s`，同时返回一份克隆（兼容旧 `(out, state)` 风格）
/// - `state == None`：使用本地 state，返回新建 state
///
/// 返回 `(redacted_text, state)`：
/// - `redacted_text`：替换后的文本
/// - `state`：本次调用累计的 redacted 模式 / counts
pub fn redact_free_text(
    input: &str,
    state: Option<&mut RedactionState>,
) -> (String, RedactionState) {
    // 累积语义：传入 Some(&mut s) 时，从 s 的克隆开始，最后写回 s；
    // 传入 None 时使用全新的本地 state。
    let mut owned = match state {
        Some(ref s) => (*s).clone(),
        None => RedactionState::new(),
    };

    let matches = collect_non_overlapping_matches(input);
    let patterns = compiled_patterns();

    let mut output = String::with_capacity(input.len());
    let mut last_end = 0usize;
    for (start, end, idx) in matches {
        // 先复制中间未被匹配的片段
        output.push_str(&input[last_end..start]);
        let matched = &input[start..end];
        let pat = &patterns[idx];
        if pat.uses_capture {
            // 需要展开 $1 / $2 等反向引用
            let replaced = pat.regex.replace(matched, pat.literal_replacement);
            output.push_str(&replaced);
        } else {
            output.push_str(pat.literal_replacement);
        }
        owned.record_redaction(pat.id.kind());
        last_end = end;
    }
    output.push_str(&input[last_end..]);

    // 同步给调用方
    if let Some(s) = state {
        *s = owned.clone();
    }
    (output, owned)
}

/// 长字段截断：如果 `value.len() > max_chars` 截断到 `max_chars - 3` 字节并追加 `…`。
/// 返回 `(truncated_value, was_truncated)`。
///
/// 省略号 `…` 是 U+2026，占 3 UTF-8 bytes，故预算 3 bytes。
/// 最终输出字节数 ≤ `max_chars`（除非 `max_chars < 3`，此时输出恰好 3 bytes）。
pub fn truncate_value(value: &str, max_chars: usize) -> (String, bool) {
    if value.len() <= max_chars {
        return (value.to_string(), false);
    }
    let suffix_len = "…".len(); // 3 bytes for U+2026
    let mut end = max_chars.saturating_sub(suffix_len);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = &value[..end];
    (format!("{truncated}…"), true)
}

/// 对 serde_json::Value 的字符串字段做截断，状态记录到 `state`。
///
/// - 顶层字段名匹配 `key` → 如果 value 是 string 且被截断，state 记录 `key`
/// - 返回修改后的 Value
pub fn truncate_string_fields(
    value: &serde_json::Value,
    max_chars: usize,
    state: &mut RedactionState,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            let (new_s, truncated) = truncate_value(s, max_chars);
            if truncated {
                state.record_truncation("$");
            }
            serde_json::Value::String(new_s)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .map(|v| truncate_string_fields(v, max_chars, state))
                .collect(),
        ),
        serde_json::Value::Object(obj) => {
            let mut out = serde_json::Map::new();
            for (k, v) in obj {
                if let serde_json::Value::String(s) = v {
                    let (new_s, truncated) = truncate_value(s, max_chars);
                    if truncated {
                        state.record_truncation(k);
                        out.insert(k.clone(), serde_json::Value::String(new_s));
                    } else {
                        out.insert(k.clone(), serde_json::Value::String(new_s));
                    }
                } else {
                    out.insert(k.clone(), truncate_string_fields(v, max_chars, state));
                }
            }
            serde_json::Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `truncate_string_fields` + `redact_value_strings` 的组合入口。
///
/// 用于「先把所有 string 字段截短，再对所有 string 做文本模式 redact」。
pub fn sanitize_free_text_value(
    value: &serde_json::Value,
    max_chars: usize,
) -> (serde_json::Value, RedactionState) {
    let mut state = RedactionState::new();
    let truncated = truncate_string_fields(value, max_chars, &mut state);
    let redacted = redact_value_strings(&truncated, &mut state);
    (redacted, state)
}

fn redact_value_strings(
    value: &serde_json::Value,
    state: &mut RedactionState,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            let (new_s, _) = redact_free_text(s, Some(state));
            serde_json::Value::String(new_s)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(|v| redact_value_strings(v, state)).collect())
        }
        serde_json::Value::Object(obj) => {
            let mut out = serde_json::Map::new();
            for (k, v) in obj {
                out.insert(k.clone(), redact_value_strings(v, state));
            }
            serde_json::Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_pem_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAK...\n-----END RSA PRIVATE KEY-----";
        let (out, state) = redact_free_text(pem, None);
        assert!(out.contains("[REDACTED_PEM_BLOCK]"));
        assert!(state.redacted_patterns.contains("pem_block"));
    }

    #[test]
    fn redact_secret_assignment_with_quotes() {
        let s = "api_key=\"sk_test_abcdef123456\" end";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("sk_test_abcdef123456"));
        assert!(state.redacted_patterns.contains("secret_assignment"));
    }

    #[test]
    fn redact_secret_assignment_without_quotes() {
        let s = "token: abcdef123456 trailing";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("token=[REDACTED]"));
        assert!(state.redacted_patterns.contains("secret_assignment"));
    }

    #[test]
    fn redact_bearer_token() {
        let s = "Authorization: Bearer eyJhbGc.eyJzdWI.SflKxw end";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("Bearer [REDACTED_TOKEN]"));
        assert!(state.redacted_patterns.contains("bearer_token"));
    }

    #[test]
    fn redact_github_token() {
        let s = "using ghp_abc123def456ghi789jkl012 calling";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED_GITHUB_TOKEN]"));
        assert!(state.redacted_patterns.contains("github_token"));
    }

    #[test]
    fn redact_provider_api_key() {
        let s = "key=sk-abcdefghij1234567890 end";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED_API_KEY]"));
        assert!(state.redacted_patterns.contains("provider_api_key"));
    }

    #[test]
    fn redact_anthropic_api_key() {
        let s = "sk-ant-abc123def456ghi789jkl012mno";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED_API_KEY]"));
        assert!(state.redacted_patterns.contains("provider_api_key"));
    }

    #[test]
    fn redact_jwt() {
        let s = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED_JWT]"));
        assert!(state.redacted_patterns.contains("jwt"));
    }

    #[test]
    fn redact_dsn_postgres() {
        let s = "postgres://user:pass@host:5432/db";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED_CONNECTION_STRING]"));
        assert!(state.redacted_patterns.contains("dsn"));
    }

    #[test]
    fn redact_dsn_mongodb_srv() {
        let s = "mongodb+srv://user:pass@cluster.example.com/db";
        let (out, _) = redact_free_text(s, None);
        assert!(out.contains("[REDACTED_CONNECTION_STRING]"));
    }

    #[test]
    fn redact_state_tracks_counts() {
        let s = "ghp_abc123def456ghi789jkl012 ghp_def456abc123ghi789jkl012";
        let (_, state) = redact_free_text(s, None);
        assert_eq!(state.counts.get("github_token"), Some(&2));
    }

    #[test]
    fn truncate_value_short_unchanged() {
        let (s, was) = truncate_value("hi", 100);
        assert_eq!(s, "hi");
        assert!(!was);
    }

    #[test]
    fn truncate_value_long_truncated() {
        let long = "x".repeat(500);
        let (s, was) = truncate_value(&long, 100);
        assert!(s.len() <= 100, "len was {}", s.len());
        assert!(was);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn truncate_string_fields_tracks_keys() {
        let v = serde_json::json!({
            "short": "ok",
            "long": "x".repeat(500),
            "nested": { "deep": "y".repeat(500) },
        });
        let mut state = RedactionState::new();
        let _ = truncate_string_fields(&v, 100, &mut state);
        assert!(state.truncated_fields.contains("long"));
        assert!(state.truncated_fields.contains("deep"));
    }

    #[test]
    fn sanitize_free_text_value_runs_truncate_then_redact() {
        let v = serde_json::json!({
            "feedback": "ran into ghp_abc123def456ghi789jkl012mno issue",
            "excerpt": "x".repeat(500),
        });
        let (out, state) = sanitize_free_text_value(&v, 100);
        let fb = out.get("feedback").and_then(|v| v.as_str()).unwrap();
        assert!(fb.contains("[REDACTED_GITHUB_TOKEN]"));
        let ex = out.get("excerpt").and_then(|v| v.as_str()).unwrap();
        assert!(ex.len() <= 100, "excerpt len was {}", ex.len());
        assert!(state.redacted_patterns.contains("github_token"));
        assert!(state.truncated_fields.contains("excerpt"));
    }

    #[test]
    fn state_to_json_serializable() {
        let mut state = RedactionState::new();
        state.record_redaction("github_token");
        state.record_truncation("feedback");
        let json = state.to_json();
        assert!(json.get("redactedPatterns").is_some());
        assert!(json.get("truncatedFields").is_some());
        assert!(json.get("counts").is_some());
    }

    #[test]
    fn redact_state_reusable_across_calls() {
        let mut state = RedactionState::new();
        let (_, _) = redact_free_text("ghp_abc123def456ghi789jkl012", Some(&mut state));
        let (_, _) = redact_free_text("sk-abcdefghij1234567890", Some(&mut state));
        assert!(state.redacted_patterns.contains("github_token"));
        assert!(state.redacted_patterns.contains("provider_api_key"));
        assert_eq!(state.counts.get("github_token"), Some(&1));
        assert_eq!(state.counts.get("provider_api_key"), Some(&1));
    }

    #[test]
    fn redact_handles_empty_input() {
        let (out, state) = redact_free_text("", None);
        assert_eq!(out, "");
        assert!(state.redacted_patterns.is_empty());
    }

    #[test]
    fn redact_handles_input_with_no_matches() {
        let s = "this is a totally innocent string with no secrets";
        let (out, state) = redact_free_text(s, None);
        assert_eq!(out, s);
        assert!(state.redacted_patterns.is_empty());
    }

    // --- Round 74 增量测试：覆盖重叠优先级 + state 累加 ---

    #[test]
    fn redact_bearer_wins_over_secret_assignment() {
        // Authorization: Bearer xxx 应被 bearer 匹配，而不是被 secret_assignment 抢占为 authorization=Bearer
        let s = "Authorization: Bearer abc123def456ghi789jkl012 end";
        let (out, state) = redact_free_text(s, None);
        assert!(out.contains("Bearer [REDACTED_TOKEN]"));
        assert!(!state.redacted_patterns.contains("secret_assignment"));
        assert!(state.redacted_patterns.contains("bearer_token"));
    }

    #[test]
    fn redact_multiple_github_tokens_all_counted() {
        let s = "first ghp_abc123def456ghi789jkl012 then ghp_def456abc123ghi789jkl012 finally";
        let (_, state) = redact_free_text(s, None);
        assert_eq!(state.counts.get("github_token"), Some(&2));
        assert!(state.redacted_patterns.contains("github_token"));
    }

    #[test]
    fn redact_state_accumulates_across_calls() {
        let mut state = RedactionState::new();
        let _ = redact_free_text("ghp_abc123def456ghi789jkl012", Some(&mut state));
        let _ = redact_free_text("postgres://user:pass@host:5432/db", Some(&mut state));
        let _ = redact_free_text("ghp_def456abc123ghi789jkl012", Some(&mut state));
        assert_eq!(state.counts.get("github_token"), Some(&2));
        assert_eq!(state.counts.get("dsn"), Some(&1));
        assert!(state.redacted_patterns.contains("github_token"));
        assert!(state.redacted_patterns.contains("dsn"));
    }

    #[test]
    fn truncate_value_with_multibyte_boundary() {
        // 中文每个字符 3 UTF-8 bytes，截断边界不应切碎字符
        let s = "中文字符串测试用例"; // 9 chars, 27 bytes
        let (out, was) = truncate_value(s, 12); // 期望 12 - 3 = 9 bytes (3 chars) + … = 12 bytes
        assert_eq!(out.len(), 12);
        assert!(was);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_value_exact_max_chars() {
        let s = "x".repeat(100);
        let (out, was) = truncate_value(&s, 100);
        assert_eq!(out, s);
        assert!(!was);
    }
}
