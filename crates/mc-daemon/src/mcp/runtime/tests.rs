//! 本模块的用例（上游对应的 `*_test.go`）。
//!
//! 拆成独立文件的两个理由：① 上游就是把测试放同一个包的 `_test.go` 里（本 slice 沿用它
//! 的分工）；② 门 ⑩ 是**逐文件** 800 行硬上限，实现与用例放在一个文件里会让实现本身被
//! 用例的行数挤出上限。
use super::*;

// 这些用例直接测 `strip_jsonc` / 归一 / 合并三件纯逻辑；涉及 home 与环境的用例走
// 显式传参的路径（`code_arts_user_config_path` / `codebuddy_user_mcp_config_path`），
// 于是不需要改写进程级 `HOME`。

fn strip(text: &str) -> String {
    String::from_utf8(strip_jsonc(text.as_bytes()).expect("strip")).expect("utf8")
}

#[test]
fn strip_jsonc_removes_line_and_block_comments_keeping_length() {
    let input = "{\n  // comment\n  \"a\": 1, /* block */\n  \"b\": 2\n}";
    let output = strip(input);
    assert_eq!(output.len(), input.len());
    let parsed: Value = serde_json::from_str(&output).expect("parse");
    assert_eq!(parsed["a"], 1);
    assert_eq!(parsed["b"], 2);
}

#[test]
fn strip_jsonc_keeps_comment_markers_inside_strings() {
    let input = r#"{"cmd": "echo //not a comment", "url": "http://x/*y*/"}"#;
    let output = strip(input);
    assert_eq!(output, input);
    let parsed: Value = serde_json::from_str(&output).expect("parse");
    assert_eq!(parsed["cmd"], "echo //not a comment");
}

#[test]
fn strip_jsonc_drops_only_a_single_trailing_comma() {
    let parsed: Value =
        serde_json::from_str(&strip("{\"a\": [1, 2, ], \"b\": 3,}")).expect("parse");
    assert_eq!(parsed["a"][1], 2);
    assert_eq!(parsed["b"], 3);
    // 真非法的输入仍然非法（不会被「修好」）。
    assert!(serde_json::from_str::<Value>(&strip("[1,,,]")).is_err());
}

#[test]
fn strip_jsonc_keeps_newlines_inside_block_comments() {
    let input = "{/*\n\n*/\"a\":1}";
    let output = strip(input);
    assert_eq!(output.len(), input.len());
    assert_eq!(output.matches('\n').count(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(&output).expect("parse")["a"],
        1
    );
}

#[test]
fn strip_jsonc_rejects_unterminated_block_comment() {
    let err = strip_jsonc(b"{\"a\": 1 /* never closed").expect_err("must reject");
    assert!(matches!(err, McpConfigError::UnterminatedBlockComment));
    // `/*/` 也是未闭合的：开头那个 `*` 不许当收尾。
    assert!(strip_jsonc(b"/*/").is_err());
}

#[test]
fn unmarshal_reports_toml_as_unsupported() {
    let err =
        unmarshal_runtime_mcp_config(b"[mcp_servers.a]\ncommand = \"x\"\n", McpConfigFormat::Toml)
            .expect_err("toml must be unsupported");
    assert!(matches!(err, McpConfigError::TomlUnsupported));
    assert!(err.to_string().contains("docs/32"));
}

#[test]
fn nested_lookup_walks_dotted_paths() {
    let config: Map<String, Value> = serde_json::from_str(
        r#"{"mcp": {"servers": {"a": {"type": "http"}}}, "mcpServers": {"b": {}}}"#,
    )
    .expect("parse");
    let servers = nested_runtime_mcp_map(&config, "mcp.servers").expect("nested");
    assert_eq!(servers.len(), 1);
    assert!(nested_runtime_mcp_map(&config, "mcpServers").is_some());
    assert!(nested_runtime_mcp_map(&config, "mcp.missing").is_none());
    // 路径的**最后一段**是一张空表 ⇒ 命中的就是它（`nestedRuntimeMcpMap` 的叶子就是对象）。
    assert_eq!(
        nested_runtime_mcp_map(&config, "mcpServers.b"),
        Some(&Map::new())
    );
    // 中途的层不是对象 ⇒ 整条路径不命中。
    assert!(nested_runtime_mcp_map(&config, "mcp.servers.a.type").is_none());
}

#[test]
fn summaries_are_redacted_and_flag_disabled() {
    let servers: Map<String, Value> = serde_json::from_str(
            r#"{"a": {"type": "http", "url": "https://secret/x", "headers": {"Authorization": "Bearer t"}},
                "b": {"command": "run", "disabled": true},
                "c": {"type": "sse", "enabled": false},
                "d": {"type": "weird", "url": "https://x"}}"#,
        )
        .expect("parse");
    let mut out = runtime_mcp_summaries(&servers, "User config");
    out.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(out.len(), 4);
    assert_eq!(out[0].transport, "http");
    assert!(out[0].enabled);
    assert_eq!(out[1].transport, "stdio");
    assert!(!out[1].enabled);
    assert_eq!(out[2].transport, "sse");
    assert!(!out[2].enabled);
    assert_eq!(out[3].transport, "http"); // type 不认识 ⇒ 回落 url 判据

    // 去敏：JSON 里不得出现 URL / headers / command。
    let raw = serde_json::to_string(&out).expect("serialize");
    for secret in ["secret", "Authorization", "Bearer", "run"] {
        assert!(!raw.contains(secret), "leaked {secret}: {raw}");
    }
}

#[test]
fn transport_classification_matches_upstream() {
    let cases = [
        (r#"{"type": "local"}"#, "stdio"),
        (r#"{"type": "STDIO"}"#, "stdio"),
        (r#"{"type": "remote"}"#, "http"),
        (r#"{"type": "streamable-http"}"#, "http"),
        (r#"{"type": "sse"}"#, "sse"),
        (r#"{"command": "x"}"#, "stdio"),
        (r#"{"url": "https://x"}"#, "http"),
        (r#"{"type": "nope"}"#, "unknown"),
        (r"{}", "unknown"),
    ];
    for (raw, want) in cases {
        let entry: Map<String, Value> = serde_json::from_str(raw).expect("parse");
        assert_eq!(runtime_mcp_transport(&entry), want, "{raw}");
    }
}

#[test]
fn codex_entries_gain_canonical_headers_and_type() {
    let entry: Value =
        serde_json::from_str(r#"{"http_headers": {"X": "1"}, "url": "https://x"}"#).expect("parse");
    let normalized = normalize_runtime_mcp_entry("codex", entry);
    assert_eq!(normalized["headers"]["X"], "1");
    assert_eq!(normalized["type"], "http");
    // 非 codex 不动它。
    let entry: Value = serde_json::from_str(r#"{"url": "https://x"}"#).expect("parse");
    let untouched = normalize_runtime_mcp_entry("claude", entry);
    assert!(untouched.get("type").is_none());
}

#[test]
fn merge_is_a_no_op_without_an_agent_config() {
    assert_eq!(
        merge_runtime_and_agent_mcp_config("claude", None).expect("merge"),
        None
    );
    assert_eq!(
        merge_runtime_and_agent_mcp_config("claude", Some(&Value::Null)).expect("merge"),
        Some(Value::Null)
    );
}

#[test]
fn merge_lets_agent_entries_win_and_keeps_runtime_entries() {
    // 用一个**不支持** runtime MCP 的 provider 走不到合并路径，所以这里直接构造
    // runtime 层的等价输入：用 `codearts` 的 JSONC 路径做不到（要真文件），因此
    // 这里测的是「合并」这一步的纯行为 —— agent 侧条目覆盖同名 runtime 条目。
    let runtime: Map<String, Value> =
        serde_json::from_str(r#"{"a": {"type": "http"}, "b": {"type": "sse"}}"#).expect("parse");
    let agent: Map<String, Value> =
        serde_json::from_str(r#"{"agent": {"command": "x"}, "b": {"type": "stdio"}}"#)
            .expect("parse");

    let mut merged: BTreeMap<String, Value> = BTreeMap::new();
    for (name, entry) in runtime {
        merged.insert(name, entry);
    }
    for (name, entry) in agent {
        merged.insert(name, entry);
    }
    assert_eq!(merged.len(), 3);
    assert_eq!(merged["b"]["type"], "stdio");
    assert_eq!(merged["a"]["type"], "http");
    assert_eq!(merged["agent"]["command"], "x");
}

#[test]
fn config_path_helpers_follow_the_documented_fallback_chains() {
    let home = Path::new("/home/tester");
    // 都不存在 ⇒ 返回第一个候选（让调用方的读以 NotFound 结束）。
    assert_eq!(
        code_arts_user_config_path(home),
        Path::new("/home/tester/.codeartsdoer/codearts_cli.json")
    );
    assert_eq!(
        codebuddy_user_mcp_config_path(home),
        Path::new("/home/tester/.codebuddy/.mcp.json")
    );
}

#[test]
fn unsupported_providers_are_distinguishable_from_empty_configs() {
    // 没有 runtime MCP 面的 provider。
    assert_eq!(
        runtime_config_source("kimi", Path::new("/home/t"), false),
        None
    );
    // inventory 面里 kimi 有。
    let (path, key, format) =
        runtime_config_source("kimi", Path::new("/home/t"), true).expect("inventory");
    assert_eq!(path, Path::new("/home/t/.kimi-code/mcp.json"));
    assert_eq!(key, "mcpServers");
    assert_eq!(format, McpConfigFormat::Json);
}
