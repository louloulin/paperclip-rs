//! golden fixture 回放的门禁测试。
//!
//! 三个层次：
//!
//! 1. **纯离线**（`fixture_set_is_self_consistent`）：不需要数据库、不需要起服务，
//!    CI 的 `cargo test --workspace` 就能跑 —— 保证 fixture 全部可加载、每条都能被
//!    规划成请求或给出明确原因、报告行数与 fixture 数一一对应。
//! 2. **stateless 回放**（`stateless_tier_decides_every_anonymous_fixture`）：
//!    在本仓真实 router 上重放匿名 fixture 并断言结论的**分布**（判定闭环 + 可复现），
//!    不断言"某条必须 pass"——那是产品状态，会随实现演进而变，测试只锁协议。
//! 3. **database 回放**（`database_tier_replays_every_fixture`，`#[ignore]`）：
//!    需要 `MULTICA_TEST_DATABASE_URL`；未设置时打印 skip。

use std::path::PathBuf;

use mc_conformance::{
    harness, judge, load_dir, merge, plan, run_tier, to_row, Bindings, Fixture, Observed, Outcome,
    Report, Tier,
};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden")
}

#[test]
fn fixture_set_is_self_consistent() {
    let dir = golden_dir();
    let fixtures = load_dir(&dir).expect("golden fixtures must load");
    assert!(
        !fixtures.is_empty(),
        "no fixtures under {} — run scripts/extract_upstream_fixtures.py",
        dir.display()
    );

    // 每条 fixture 要么能规划成请求，要么必须给出非空原因（不许"静默不动"）。
    let bindings = Bindings::stateless();
    for fx in &fixtures {
        match plan(fx, &bindings) {
            Ok(p) => {
                assert!(
                    !p.uri.contains('{'),
                    "{}: 仍有未填充的路径占位符 {}",
                    fx.id,
                    p.uri
                );
                assert!(fx.path.starts_with('/'), "{}: path 必须是绝对路径", fx.id);
            }
            Err(reason) => assert!(
                !reason.trim().is_empty(),
                "{}: unevaluable 但没有原因",
                fx.id
            ),
        }
    }

    // 报告行数与 fixture 数一一对应 —— 这是"等价率不可能靠少算行来变好看"的机器保证。
    let rows: Vec<_> = fixtures
        .iter()
        .map(|fx| {
            to_row(
                fx,
                &mc_conformance::FixtureOutcome {
                    outcome: Outcome::Pass,
                    tier: "test".into(),
                    detail: String::new(),
                    status_observed: Some(fx.expect.status),
                    offline: None,
                    database: None,
                },
            )
        })
        .collect();
    let report = Report::from_rows(&dir, &bindings, rows);
    assert_eq!(report.totals.fixtures, fixtures.len());
    assert_eq!(report.totals.pass, fixtures.len());
}

#[tokio::test]
async fn stateless_tier_decides_every_anonymous_fixture() {
    let dir = golden_dir();
    let fixtures = load_dir(&dir).expect("golden fixtures must load");
    let bindings = Bindings::stateless();
    let router = harness::stateless_router().expect("stateless router");
    let observed = run_tier(&router, &fixtures, &bindings, Tier::Stateless).await;
    assert_eq!(observed.len(), fixtures.len());

    let anonymous: Vec<&Fixture> = fixtures
        .iter()
        .filter(|f| f.actor.kind == mc_conformance::ActorKind::Anonymous)
        .collect();
    assert!(
        anonymous.len() >= 10,
        "匿名 fixture 太少（{}），离线判定这一层就名存实亡",
        anonymous.len()
    );

    for (fx, got) in fixtures.iter().zip(&observed) {
        if fx.actor.kind == mc_conformance::ActorKind::Anonymous {
            assert_ne!(
                got.outcome,
                Outcome::Unevaluable,
                "{}: 匿名 fixture 在 stateless 层必须能判定：{}",
                fx.id,
                got.detail
            );
        } else {
            // 非匿名 fixture 在这一层必须**明确说**需要数据库层，而不是伪装成 mismatch。
            assert_eq!(got.outcome, Outcome::Unevaluable, "{}", fx.id);
            assert!(
                got.detail.contains("database tier"),
                "{}: 应说明需要 database 层，实际：{}",
                fx.id,
                got.detail
            );
        }
    }

    // 上游要求 `/api/me`、`/api/issues`、`/api/workspaces` 这些受保护路由无凭证 401。
    // 这是本层的地板值：低于 4 说明鉴权链断了。
    let report = Report::from_rows(
        &dir,
        &bindings,
        fixtures
            .iter()
            .zip(&observed)
            .map(|(fx, got)| to_row(fx, &merge(Some(got.clone()), None)))
            .collect(),
    );
    assert!(
        report.totals.pass >= 4,
        "受保护路由的 401 少于 4 条，鉴权链可能断了：{}",
        report.render_text()
    );
    assert_eq!(
        report.totals.unmounted
            + report.totals.placeholder
            + report.totals.pass
            + report.totals.mismatch,
        anonymous.len(),
        "匿名 fixture 必须全部落到某个明确结论上"
    );

    // 同一份 fixture 在同一层跑两次，报告必须字节一致（`--check` 的前提）。
    let again = run_tier(&router, &fixtures, &bindings, Tier::Stateless).await;
    let rows_a = fixtures
        .iter()
        .zip(&observed)
        .map(|(fx, got)| to_row(fx, &merge(Some(got.clone()), None)))
        .collect();
    let rows_b = fixtures
        .iter()
        .zip(&again)
        .map(|(fx, got)| to_row(fx, &merge(Some(got.clone()), None)))
        .collect();
    let a = Report::from_rows(&dir, &bindings, rows_a)
        .to_json()
        .unwrap();
    let b = Report::from_rows(&dir, &bindings, rows_b)
        .to_json()
        .unwrap();
    assert_eq!(a, b, "stateless 层报告必须可复现（否则 --check 无意义）");
}

/// `expect.json_subset` 必须真的对着**实现里的响应**判定，不只是单元测试里的玩具数据。
///
/// 上游 fixture 本轮一条 `json_subset` 都没抽到（见 docs/27 的解释），所以这里手工写
/// 一条指向本仓已实现路由 `/api/health` 的 fixture —— 响应体的字段名是上游同款
/// （`status` / `service`），subset 命中即证明"响应字段级等价"这条路是通的。
#[tokio::test]
#[ignore = "需要起 router；用 --ignored 跑"]
async fn json_subset_is_checked_against_a_live_route() {
    let raw = r#"{
      "schema_version": 1,
      "id": "health/manual-json-subset@handwritten:1#1",
      "method": "GET",
      "path": "/api/health",
      "actor": {"kind": "anonymous"},
      "expect": {"status": 200, "json_subset": {"status": "ok", "service": "multica-rs"}},
      "source": {"file": "handwritten", "line": 1, "test": "manual", "site": "manual", "via": "router", "commit": "n/a"}
    }"#;
    let fx: Fixture = serde_json::from_str(raw).expect("fixture parses");
    fx.verify().expect("fixture verifies");

    let router = harness::stateless_router().expect("stateless router");
    let got = mc_conformance::replay_one(&router, &fx, &Bindings::stateless()).await;
    assert_eq!(got.outcome, Outcome::Pass, "subset 未命中：{}", got.detail);

    // 反向：断言一个响应里不存在的字段必须被判成 mismatch（否则 subset 是摆设）。
    let mut broken = fx.clone();
    broken.expect.json_subset = serde_json::json!({"service": "not-multica-rs"});
    let got = mc_conformance::replay_one(&router, &broken, &Bindings::stateless()).await;
    assert_eq!(got.outcome, Outcome::Mismatch, "{}", got.detail);
    assert!(
        got.detail.contains("json_subset mismatch"),
        "{}",
        got.detail
    );

    // 单元层面的边界：数组按下标、对象递归。
    assert!(mc_conformance::json_subset(
        &serde_json::json!({"a": {"b": [1, 2, 3]}, "extra": true}),
        &serde_json::json!({"a": {"b": [1, 2]}})
    )
    .is_ok());
    assert!(mc_conformance::json_subset(
        &serde_json::json!({"a": {"b": [1]}}),
        &serde_json::json!({"a": {"b": [1, 2]}})
    )
    .is_err());
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL（真库 + 迁移 + 种子身份）"]
async fn database_tier_replays_every_fixture() {
    let Some(url) = harness::database_url_from_env() else {
        eprintln!("database_tier_replays_every_fixture: MULTICA_TEST_DATABASE_URL 未设置，跳过");
        return;
    };
    let dir = golden_dir();
    let fixtures = load_dir(&dir).expect("golden fixtures must load");
    let (router, bindings) = harness::database_router(&url)
        .await
        .expect("database tier bootstrap");
    let observed = run_tier(&router, &fixtures, &bindings, Tier::Database).await;
    assert_eq!(observed.len(), fixtures.len());
    for (fx, got) in fixtures.iter().zip(&observed) {
        assert!(!got.detail.trim().is_empty(), "{}: 结论必须带原因", fx.id);
        // 真库层不应该再出现"无法构造请求"。
        assert_ne!(
            got.outcome,
            Outcome::Unevaluable,
            "{}: {}",
            fx.id,
            got.detail
        );
    }
    let report = Report::from_rows(
        &dir,
        &bindings,
        fixtures
            .iter()
            .zip(&observed)
            .map(|(fx, got)| to_row(fx, &merge(None, Some(got.clone()))))
            .collect(),
    );
    eprintln!("database tier: {}", report.summary_line());
    for row in &report.fixtures {
        assert!(
            !row.detail.trim().is_empty(),
            "{}: 报告行必须带 detail",
            row.id
        );
    }
    // 判定工具本身的自检：给一个人为构造的响应，判词必须符合预期。
    let (outcome, detail) = judge(
        &fixtures[0],
        &Observed {
            status: 418,
            body: b"teapot".to_vec(),
            content_type: Some("text/plain".into()),
        },
    );
    assert_eq!(outcome, Outcome::Mismatch);
    assert!(detail.contains("418"), "{detail}");
}
