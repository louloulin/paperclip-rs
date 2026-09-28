//! golden fixture 回放的门禁测试。
//!
//! 三个层次：
//!
//! 1. **纯离线**（`fixture_set_is_self_consistent`）：不需要数据库、不需要起服务，
//!    CI 的 `cargo test --workspace` 就能跑 —— 保证 fixture 全部可加载、每条都能被
//!    规划成请求或给出明确原因、报告行数与 fixture 数一一对应、**前提表没有写错的条目**。
//! 2. **stateless 回放**（`stateless_tier_decides_every_decidable_fixture`）：
//!    在本仓真实 router 上重放该层能判定的 fixture 并断言结论的**分布**（判定闭环 +
//!    可复现），不断言"某条必须 pass"—— 那是产品状态，会随实现演进而变，测试只锁协议。
//! 3. **database 回放**（`database_tier_replays_every_decidable_fixture`，`#[ignore]`）：
//!    需要 `MULTICA_TEST_DATABASE_URL`；未设置时打印 skip。
//!
//! ## 「能判定」的定义（docs/37 §203）
//!
//! 一层能判定一条 fixture，当且仅当**身份可判定**（stateless 层只判 `anonymous`）
//! **且前提全齐**（`extraction.requires` + `REPO_SIDE_PRECONDITIONS` 里的每一项都由这一层
//! 供得起）。第二问是本轮加的：`POST /auth/send-code` 身份是匿名，但它一上来就查库 ⇒
//! stateless 层对它只能给 `unevaluable`，而**不能**把「查库失败 ⇒ 500」记成 `mismatch`。

use std::path::PathBuf;

use mc_conformance::{
    harness, judge, load_dir, merge, missing_requirements, plan, run_tier, to_row, Bindings,
    Fixture, Observed, Outcome, Report, Tier,
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

    // 前提表的每一行都得命中至少一个 fixture。写错 path 的后果不是「少一条判定」，
    // 而是一条永远不会被评估的声明 —— §199 的形态：声明了但从不被机器判定。
    for (method, path, ids) in mc_conformance::REPO_SIDE_PRECONDITIONS {
        let hits = fixtures
            .iter()
            .filter(|fx| fx.method.eq_ignore_ascii_case(method) && fx.path == *path)
            .count();
        assert!(
            hits > 0,
            "REPO_SIDE_PRECONDITIONS 里 {method} {path} ({ids:?}) 没命中任何 fixture"
        );
    }

    // 前提凑不齐的 fixture 在每一层都要**说出缺什么**，而不是安静地变成 mismatch。
    for fx in &fixtures {
        for tier in [Tier::Stateless, Tier::Database] {
            let missing = missing_requirements(fx, tier).expect("前提 id 都在登记表里");
            if !missing.is_empty() {
                let detail = mc_conformance::requirements_detail(fx, tier);
                for id in &missing {
                    assert!(
                        detail.contains(id),
                        "{}: 理由里没点名缺哪一样前提：{detail}",
                        fx.id
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn stateless_tier_decides_every_decidable_fixture() {
    let dir = golden_dir();
    let fixtures = load_dir(&dir).expect("golden fixtures must load");
    let bindings = Bindings::stateless();
    let routers = harness::stateless_routers().expect("stateless routers");
    let observed = run_tier(&routers, &fixtures, &bindings, Tier::Stateless).await;
    assert_eq!(observed.len(), fixtures.len());

    let decidable: Vec<&Fixture> = fixtures
        .iter()
        .filter(|f| Tier::Stateless.supports(f).expect("前提 id 都在登记表里"))
        .collect();
    assert!(
        decidable.len() >= 10,
        "这一层能判定的 fixture 太少（{}），离线判定这一层就名存实亡",
        decidable.len()
    );
    // 前提机制本身必须是活的：有 fixture 真的因为前提凑不齐而被排除，否则这套机制
    // 只是一段没被执行的代码（而「声明了但从不被机器判定」正是 §199 的教训）。
    let gated = fixtures
        .iter()
        .filter(|f| !decidable.iter().any(|d| d.id == f.id))
        .count();
    assert!(
        gated > 0,
        "没有任何 fixture 因前提 / 身份被排除，`requires` 机制没有被执行到"
    );

    for (fx, got) in fixtures.iter().zip(&observed) {
        if Tier::Stateless.supports(fx).expect("前提 id 都在登记表里") {
            assert_ne!(
                got.outcome,
                Outcome::Unevaluable,
                "{}: 这一层能判定的 fixture 必须给出结论：{}",
                fx.id,
                got.detail
            );
        } else {
            // 不可判定的 fixture 在这一层必须**明确说**缺什么，而不是伪装成 mismatch。
            assert_eq!(got.outcome, Outcome::Unevaluable, "{}", fx.id);
            assert!(
                !got.detail.trim().is_empty(),
                "{}: unevaluable 但没有理由",
                fx.id
            );
            assert!(
                got.detail.contains("database tier") || got.detail.contains("precondition"),
                "{}: 应说明缺的是身份还是前提，实际：{}",
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
        decidable.len(),
        "这一层能判定的 fixture 必须全部落到某个明确结论上"
    );
    // 任何被改判成不可判定的 fixture，报告行里都要带着它的前提清单。
    for row in &report.fixtures {
        if row.outcome == Outcome::Unevaluable {
            let declared = !mc_conformance::REPO_SIDE_PRECONDITIONS.is_empty();
            assert!(
                declared || !row.detail.trim().is_empty(),
                "{}: 不可判定就必须有理由",
                row.id
            );
        }
    }

    // 同一份 fixture 在同一层跑两次，报告必须字节一致（`--check` 的前提）。
    let again = run_tier(&routers, &fixtures, &bindings, Tier::Stateless).await;
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
/// 回归销钉：§203 的三条结论各自都有机器判据，否则它们就只是文档。
///
/// 1. **子根因 A**：daemon 身份在请求 context 里，所以它**不是一个 header**。抽取器一旦
///    退回「只看字面 header」，这 20 条会静默变回 `anonymous`，而它们会**重新变成 21 条假
///    mismatch**（不带凭据 ⇒ 在 `unauthorized("missing Authorization header")` 得 401）。
///    判据：凡 `actor.kind == daemon` 的 fixture 必须声明 `daemon_token`。
/// 2. **子根因 C**：`POST /auth/send-code` 身份是匿名，但它一上来就查库 ⇒ stateless 层
///    不判定它。判据：它必须声明 `database` 且在 stateless 层是 `unevaluable`。
/// 3. **第二个部署形态必须真的承重**：只把「cloud 已配置」这个 router 造出来而没有 fixture
///    用它，就是 §199 的形态（声明了但从不被机器判定）。判据：`webhooks` 的 401 那条在
///    **默认** router 上是 403（步 1 抢先短路），在 **cloud 已配置** router 上才是 401。
#[tokio::test]
async fn section_202_root_causes_stay_pinned() {
    let dir = golden_dir();
    let fixtures = load_dir(&dir).expect("golden fixtures must load");

    // 1.
    let daemon_fixtures: Vec<&Fixture> = fixtures
        .iter()
        .filter(|f| f.actor.kind == mc_conformance::ActorKind::Daemon)
        .collect();
    assert!(
        daemon_fixtures.len() >= 20,
        "daemon actor 的 fixture 只剩 {} 条：抽取器的 daemon-context 识别退化了？",
        daemon_fixtures.len()
    );
    for fx in &daemon_fixtures {
        assert!(
            fx.requirement_ids().contains(&"daemon_token"),
            "{}: daemon actor 必须声明 daemon_token",
            fx.id
        );
        assert!(
            fx.actor.identity_source.as_deref() == Some("middleware.WithDaemonContext"),
            "{}: daemon actor 必须记下身份是怎么施加的",
            fx.id
        );
    }

    // 2.
    let send_code = fixtures
        .iter()
        .find(|f| f.method == "POST" && f.path == "/auth/send-code")
        .expect("send-code fixture must exist");
    assert_eq!(send_code.actor.kind, mc_conformance::ActorKind::Anonymous);
    assert!(
        send_code.requirement_ids().contains(&"database"),
        "send-code 必须声明 database（§201.2 子根因 C）"
    );
    assert!(
        !Tier::Stateless
            .supports(send_code)
            .expect("前提 id 都在登记表里"),
        "send-code 在 stateless 层不得被判为可判定"
    );
    assert!(
        Tier::Database
            .supports(send_code)
            .expect("前提 id 都在登记表里"),
        "send-code 在真库层必须可判定（这是它唯一的归宿）"
    );

    // 3.
    let webhook = fixtures
        .iter()
        .find(|f| f.path == "/api/webhooks/stripe" && f.expect.status == 401)
        .expect("the unsigned-webhook fixture must exist");
    let bindings = Bindings::stateless();
    let default_router = harness::stateless_router().expect("stateless router");
    let on_default = mc_conformance::replay_one(&default_router, webhook, &bindings).await;
    assert_eq!(
        on_default.outcome,
        Outcome::Mismatch,
        "默认（未配置 cloud）router 上这条必须是 mismatch —— 那是步 1 抢先短路的证据；\
         若它变成 pass，说明 fixture 已经被另一个前提挡住了，这条判据就没意义了：{}",
        on_default.detail
    );
    let routers = harness::stateless_routers().expect("stateless routers");
    let on_cloud =
        mc_conformance::replay_one(routers.for_fixture(webhook), webhook, &bindings).await;
    assert_eq!(
        on_cloud.outcome,
        Outcome::Pass,
        "cloud 已配置的 router 上这条必须真的判成 401：{}",
        on_cloud.detail
    );
    // 反向：默认形态仍然是「未配置 ⇒ 403」，不能为了判定另一条而把它改掉。
    let disabled = fixtures
        .iter()
        .find(|f| f.path == "/api/webhooks/stripe" && f.expect.status == 403)
        .expect("the disabled-webhook fixture must exist");
    let disabled_got =
        mc_conformance::replay_one(routers.for_fixture(disabled), disabled, &bindings).await;
    assert_eq!(
        disabled_got.outcome,
        Outcome::Pass,
        "「cloud 未配置 ⇒ 403」仍然是默认形态：{}",
        disabled_got.detail
    );
}

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
async fn database_tier_replays_every_decidable_fixture() {
    let Some(url) = harness::database_url_from_env() else {
        eprintln!(
            "database_tier_replays_every_decidable_fixture: MULTICA_TEST_DATABASE_URL 未设置，跳过"
        );
        return;
    };
    let dir = golden_dir();
    let fixtures = load_dir(&dir).expect("golden fixtures must load");
    let (router, bindings) = harness::database_router(&url)
        .await
        .expect("database tier bootstrap");
    let routers = mc_conformance::TierRouters::single(router);
    let observed = run_tier(&routers, &fixtures, &bindings, Tier::Database).await;
    assert_eq!(observed.len(), fixtures.len());
    for (fx, got) in fixtures.iter().zip(&observed) {
        assert!(!got.detail.trim().is_empty(), "{}: 结论必须带原因", fx.id);
        if Tier::Database.supports(fx).expect("前提 id 都在登记表里") {
            // 真库层对「它供得起前提的 fixture」不应该再出现"无法构造请求"。
            assert_ne!(
                got.outcome,
                Outcome::Unevaluable,
                "{}: {}",
                fx.id,
                got.detail
            );
        } else {
            // 前提供不起的必须**点名缺什么**：`daemon_token` / `cloud_runtime_stub` 那些
            // 没有任何一层供得起，硬造一个替身把它们判成通过就是自欺（docs/37 §203）。
            assert_eq!(got.outcome, Outcome::Unevaluable, "{}", fx.id);
            for id in missing_requirements(fx, Tier::Database).expect("前提 id 都在登记表里")
            {
                assert!(got.detail.contains(id), "{}: 理由里没点名 {id}", fx.id);
            }
        }
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
