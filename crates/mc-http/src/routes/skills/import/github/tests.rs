//! `github.rs`（api.github.com 面 + 两条入口）的单元测试。

use super::*;

#[test]
fn escape_ref_path_keeps_slashes_and_escapes_each_segment() {
    // GitHub 的 commits / raw 端点**不接受** `release%2Fv2`：段内转义、`/` 保留。
    assert_eq!(escape_ref_path("main"), "main");
    assert_eq!(escape_ref_path("release/v2"), "release/v2");
    assert_eq!(escape_ref_path("feat/my branch"), "feat/my%20branch");
    assert_eq!(escape_ref_path("v1.0.0-rc.1"), "v1.0.0-rc.1");
}

#[test]
fn tree_entry_size_clamps_negative_sizes_to_zero() {
    // 畸形 tree 响应（size 缺失或负数）不能把预算算术算成负数，否则上限形同消失。
    let entry = GithubTreeEntry {
        path: "a.md".into(),
        kind: "blob".into(),
        size: -5,
    };
    assert_eq!(entry.size(), 0);
    let entry = GithubTreeEntry {
        path: "a.md".into(),
        kind: "blob".into(),
        size: 7,
    };
    assert_eq!(entry.size(), 7);
}

#[test]
fn tree_response_decodes_the_keyword_field() {
    // `type` 是 Rust 关键字 ⇒ 必须靠 `rename`；漏了它 kind 恒为 ""（全部条目被当成 tree）。
    let response: GithubTreeResponse = serde_json::from_str(
        r#"{"truncated":true,"tree":[{"path":"SKILL.md","type":"blob","size":12}]}"#,
    )
    .unwrap();
    assert!(response.truncated);
    assert_eq!(response.tree.len(), 1);
    assert_eq!(response.tree[0].kind, "blob");
    assert_eq!(response.tree[0].size(), 12);
}
