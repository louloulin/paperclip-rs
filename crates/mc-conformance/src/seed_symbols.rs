//! **分组判据**：哪些上游测试需要「自己那套行」，以及怎么在一��� fixture 里扫出
//! `$test…` 符号。
//!
//! 从 `seed.rs` 拆出来的原因只有门 ⑩：`§297` 给 seed 加了任务令牌行之后越过 800 行。
//! 拆分的边界按「**判据 vs 执行**」划：这一面只回答「哪些分组要种」，
//! [`crate::seed`] 只负责把那些行真的种出来。对外路径逐字不变（`seed` re-export）。

use std::collections::BTreeSet;

use crate::Fixture;

/// 会让一个分组需要**自己那套行**的符号：四类实体，加上 workspace。
///
/// `$testWorkspaceID` 与那四类的解析面不同（它由 `Bindings` 直接持有，不是本文件种
/// 出来的行），但**它同样是「被 `DELETE` 摧毁的共享行」** —— C 桶里 5 条
/// `workspaces/…` 就是它，所以它也必须按组分。
pub const GROUP_SYMBOLS: [&str; 5] = [
    "$testAgentID",
    "$testIssueID",
    "$testChatSessionID",
    "$testTaskID",
    "$testWorkspaceID",
];

/// 需要**自己一套行**的分组键，按字典序。
///
/// 判据是「这条 fixture 在 [`crate::plan`] 会解析到的位置里引用了 [`GROUP_SYMBOLS`]
/// 之一」。刻意扫得比 `plan()` 宽（连 `body` 与 `path` 一起扫）：多算一个分组只会
/// 多建一套行，少算一个分组则会让那批 fixture 静默变成 `unbound symbol` →
/// `unevaluable`（§205.5 纪律：不可判定与判定为过在总数里长得一样）。
#[must_use]
pub fn groups_for(fixtures: &[Fixture]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for fx in fixtures {
        let used = referenced_symbols(fx);
        if GROUP_SYMBOLS.iter().any(|sym| used.contains(*sym)) {
            out.insert(fx.source.test.clone());
        }
    }
    out.into_iter().collect()
}

/// 引用了某个符号的分组键集合（[`groups_for`] 的单符号版本）。
///
/// 只给 [`seed`] 的**前置守卫**用：解绑形态会连带取消在飞任务，而「声明了该形态又
/// 引用 `$testTaskID`」的分组会因此静默少掉一行任务 —— 那比明确报错糟得多。
#[must_use]
pub fn groups_referencing(fixtures: &[Fixture], symbol: &str) -> BTreeSet<String> {
    fixtures
        .iter()
        .filter(|fx| referenced_symbols(fx).contains(symbol))
        .map(|fx| fx.source.test.clone())
        .collect()
}

/// `plan()` 会送去 [`crate::Bindings::resolve`] 的全部取值位置。
fn referenced_symbols(fx: &Fixture) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_symbols(&fx.path, &mut out);
    for raw in fx.path_params.values() {
        collect_symbols(raw, &mut out);
    }
    for raw in fx.query.values() {
        collect_symbols(raw, &mut out);
    }
    for raw in fx.headers.values() {
        collect_symbols(raw, &mut out);
    }
    for raw in fx.actor.upstream_identity.values() {
        collect_symbols(raw, &mut out);
    }
    collect_json_symbols(fx.body.as_ref(), &mut out);
    out
}

/// 扫出一个字符串里所有 `$name` 形态的符号（`$` + `[A-Za-z0-9_]*`）。
fn collect_symbols(raw: &str, out: &mut BTreeSet<String>) {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        out.insert(raw[start..i].to_string());
    }
}

/// `body` 是任意 JSON：逐层走到字符串上再扫符号。
fn collect_json_symbols(value: Option<&serde_json::Value>, out: &mut BTreeSet<String>) {
    match value {
        Some(serde_json::Value::String(s)) => collect_symbols(s, out),
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                collect_json_symbols(Some(item), out);
            }
        }
        Some(serde_json::Value::Object(map)) => {
            for item in map.values() {
                collect_json_symbols(Some(item), out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::group_of;
    use serde_json::json;

    /// 造一条最小 fixture：只填 `groups_for` 会看的那些字段，以及 `Fixture::verify`
    /// 会看的那几个。用它来验分组键，不必依赖 `contracts/golden/**` 的具体内容。
    fn fx(test: &str, path_params: &[(&str, &str)]) -> Fixture {
        let mut value = json!({
            "schema_version": crate::SCHEMA_VERSION,
            "id": format!("dom/{test}@a.go:1#1"),
            "method": "GET",
            "path": "/api/things/{id}",
            "actor": { "kind": "anonymous" },
            "expect": { "status": 200 },
            "source": { "file": "a.go", "line": 1, "test": test, "site": "handler" },
        });
        if !path_params.is_empty() {
            let mut map = serde_json::Map::new();
            for (k, v) in path_params {
                map.insert((*k).to_string(), json!(v));
            }
            value["path_params"] = serde_json::Value::Object(map);
        }
        serde_json::from_value(value).expect("minimal fixture")
    }

    #[test]
    fn groups_for_picks_exactly_the_tests_that_reference_a_group_symbol() {
        // 承重：分组多算一个只是多建一套行，少算一个会让那批 fixture 静默变 unevaluable。
        // 所以这条钉住**两个方向**：引用了的必须进，没引用的必须不进。
        let fixtures = vec![
            fx("TestUsesIssue", &[("id", "$testIssueID")]),
            fx("TestUsesIssueAgain", &[("id", "$testIssueID")]),
            fx("TestUsesWorkspace", &[("id", "$testWorkspaceID")]),
            fx("TestUsesTaskEmbedded", &[("id", "api/$testTaskID/tail")]),
            fx("TestUsesNothing", &[("id", "not-a-uuid")]),
        ];
        assert_eq!(
            groups_for(&fixtures),
            vec![
                "TestUsesIssue".to_string(),
                "TestUsesIssueAgain".to_string(),
                "TestUsesTaskEmbedded".to_string(),
                "TestUsesWorkspace".to_string(),
            ]
        );
        // 分组键是 `source.test` 而不是 `id`：同一条上游测试的多次请求（CRUD 链）必须
        // 落到**同一组**，否则 delete-then-get 会变成 get-200。
        assert_eq!(group_of(&fixtures[0]), "TestUsesIssue");
    }
}
