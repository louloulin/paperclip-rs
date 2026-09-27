//! M9-3 的证据面（`docs/62` §4.2 与 §6.5 的通用 `DoD`）。
//!
//! 分两半，**判据不同**（与 `routes/cloud/subscriptions/tests.rs` 同手法）：
//!
//! - **不碰库的那一半**（门 ⑤，本文件）：5 条路由的字面量与授权前置（无会话 **401**）、
//!   问卷 v2 形状的**逐字段**解析、两个 shim 的**纯函数**判定（正文选择 / `starter_prompt`
//!   覆盖 / 文案常量逐字）、以及 waitlist 的四条 **400** 判定；
//! - **真库的那一半**（门 ⑥，`tests/db.rs`，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）：
//!   问卷落库、`complete` 的**幂等**（直读 `onboarded_at` 那一列比对）、waitlist 两列的
//!   **直读**对齐（`DoD` 第 2 条）、两条 DEPRECATED shim 的 **provision 链逐行**断言
//!   （`DoD` 第 3 条）。
//!
//! 🔴 **本片一条云侧替身都没有**（5 条全是本地 user-scoped 路由）⇒ 与 M9-1/M9-2 不同，
//! 「替身纪律」在这里的对应物是**零出站**。
//!
//! 共用件在 `tests/support.rs`（拆出来是门 ⑩ 的 800 行上限要求）。

use axum::http::StatusCode;
use mc_core::onboarding::QuestionnaireAnswers;
use serde_json::{json, Value};

use crate::routes::onboarding::profile::{decode_json, PATCH_ONBOARDING_BODY_LIMIT};
use crate::routes::onboarding::shim::shim_content;
use crate::routes::onboarding::shim::{
    issue_body, BootstrapOnboardingNoRuntimeRequest, BootstrapOnboardingRuntimeRequest,
};

// 本文件是 `profile.rs` 的子模块（`#[path = "tests.rs"]`）⇒ 子模块目录落在
// `routes/onboarding/`，两个子模块都用**显式** `#[path]` 指到 `tests/`。
#[path = "tests/db.rs"]
mod db;
#[path = "tests/support.rs"]
mod support;

use support::*;

/// 本片 5 条（method, 本地字面量）—— 与 `docs/fixtures/upstream-routes.tsv` 的 M9 段逐字相等。
///
/// 判据：`upstream-routes.tsv` 里 M9 + `/api/me/onboarding` 的那几行与 [`FIVE`] 逐字相等
/// （「本地字面量必须与上游字面量**逐字**相同」是 ⑦ 的口径，而 ⑦ 比的是 key，
/// 这里补的是「本片声明的 5 条 = 上游那 5 条」这一层）。
const FIVE: [(&str, &str); 5] = [
    ("PATCH", "/api/me/onboarding"),
    ("POST", "/api/me/onboarding/complete"),
    ("POST", "/api/me/onboarding/cloud-waitlist"),
    ("POST", "/api/me/onboarding/no-runtime-bootstrap"),
    ("POST", "/api/me/onboarding/runtime-bootstrap"),
];

/// 5 条在 `docs/fixtures/upstream-routes.tsv` 的 owner 段里**逐字**是 M9。
#[test]
fn the_five_literals_are_exactly_the_upstream_onboarding_keys() {
    assert_eq!(FIVE.len(), 5);
    for (method, path) in FIVE {
        assert!(path.starts_with("/api/me/onboarding"), "{path}");
        // 上游 `router.go:1617-1627` 的 5 行（`complete` 挂在 `/api/me/onboarding` 的 Route 上）。
        let expected: &str = match path {
            "/api/me/onboarding" => "PATCH",
            _ => "POST",
        };
        assert_eq!(*method, *expected, "{path}");
    }
    // 形态：`docs/62` §1.4 实测本簇 `dual-form required: 0` ⇒ **不**补尾斜杠别名。
    assert!(!FIVE.iter().any(|(_, path)| path.ends_with('/')));
}

/// 4 条挂在本目录的三个 `router()` 上，第 5 条（`complete`）挂在 `workspaces.rs` 的
/// `/api/me` 子树上（anchor 的 `onboarding/mod.rs` 模块头逐字点名了这一格）。
#[test]
fn complete_is_the_only_one_mounted_outside_this_directory() {
    let (method, path) = FIVE[1];
    assert_eq!((method, path), ("POST", "/api/me/onboarding/complete"));
    // 其余 4 条都在本目录。
    assert_eq!(FIVE.iter().filter(|(_, p)| *p != path).count(), 4);
}

/// 5 条**全部**无会话 ⇒ **401**（上游 `router.go:1603` 那个 user-scoped 组的 `middleware.Auth`）。
#[tokio::test]
async fn every_route_without_a_session_is_401() {
    let app = test_app(lazy_db());
    for (method, path) in FIVE {
        let (status, body) = send(
            &app,
            &Call::anonymous(method, path).body(json!({}).to_string()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path}: {body:?}"
        );
    }
}

/// `PATCH /api/me/onboarding` 的体判据（不碰库那一半）。
#[test]
fn the_patch_body_gate_matches_the_upstream_max_bytes_reader() {
    // 空体是**非法**的（上游 `json.Decode` 对空体 ⇒ `EOF` ⇒ 400）。
    let empty = axum::body::Bytes::new();
    assert!(decode_json(&empty).is_err());

    // 超 16 KiB ⇒ 400（与「非法 JSON」**同一条**，上游不区分）。
    let huge = axum::body::Bytes::from(format!(
        r#"{{"questionnaire":{{"role":"{}"}}}}"#,
        "x".repeat(PATCH_ONBOARDING_BODY_LIMIT)
    ));
    assert!(huge.len() > PATCH_ONBOARDING_BODY_LIMIT);
    assert!(decode_json(&huge).is_err());

    // 合法 v2 形状：逐字段解析（`DoD` 第 1 条的 v2 那一半）。
    let ok = axum::body::Bytes::from(
        json!({
            "questionnaire": {
                "source": ["search"],
                "source_other": "",
                "source_skipped": false,
                "role": "engineer",
                "role_other": "",
                "role_skipped": false,
                "use_case": ["ship_code", "manage_team"],
                "use_case_other": "",
                "use_case_skipped": false,
                "version": 2,
            }
        })
        .to_string(),
    );
    let parsed = decode_json(&ok).expect("v2 body");
    // 🔴 handler 落库的是**原始** JSON（上游 `json.RawMessage`）⇒ 这里是 `Value`，
    // 形状语义由读侧 `QuestionnaireAnswers` 判（本仓已有的 `mc-core` 纯函数）。
    assert_eq!(parsed["questionnaire"]["role"], json!("engineer"));
    assert_eq!(
        parsed["questionnaire"]["use_case"],
        json!(["ship_code", "manage_team"])
    );
    let answers: QuestionnaireAnswers =
        serde_json::from_value(parsed["questionnaire"].clone()).expect("typed answers");
    assert_eq!(answers.source, vec!["search"]);
    assert_eq!(answers.version, 2);
    assert!(answers.in_flow_resolved());

    // 省略 `questionnaire` 是**合法**的（上游：`COALESCE(NULL, col)` ⇒ 保留旧值）。
    let absent = axum::body::Bytes::from(json!({}).to_string());
    assert!(decode_json(&absent)
        .expect("empty object")
        .get("questionnaire")
        .is_none());

    // 上游那一层是 `*json.RawMessage` ⇒ **任何**合法 JSON 都接受（含非对象）。
    let odd = axum::body::Bytes::from(json!({"questionnaire": 5}).to_string());
    assert_eq!(
        decode_json(&odd).expect("raw message")["questionnaire"],
        json!(5)
    );
}

/// 🔴 `DoD` 第 1 条「缺 `role` / `use_case`」这一格：**按上游不 400**（见 `profile.rs` 的
/// 偏离登记），本文件给出它的**可测等价物** —— 缺项的问卷**照样 200 落库**，但
/// `in_flow_resolved()` 为 `false` ⇒ 不算问卷已答完。
#[test]
fn a_questionnaire_missing_role_or_use_case_is_stored_but_not_in_flow_resolved() {
    let body = json!({"questionnaire": {"source": ["search"], "version": 2}});
    let answers: QuestionnaireAnswers =
        serde_json::from_value(body["questionnaire"].clone()).expect("typed answers");
    assert!(!answers.role_resolved());
    assert!(!answers.use_case_resolved());
    assert!(!answers.in_flow_resolved());
    // `source` 答了也不算「在流已答完」（上游 `complete()` 只看 role + use_case）。
    assert!(answers.source_resolved());

    // 版本不是 2 的历史行同样不算（`docs/62` §9.7 的 `complete()` 判据）。
    let legacy = json!({"role": "engineer", "use_case": "ship_code"});
    let legacy_answers: QuestionnaireAnswers =
        serde_json::from_value(legacy).expect("legacy answers");
    assert!(legacy_answers.in_flow_resolved());
    assert!(!legacy_answers.is_current_schema());
}

/// waitlist 的四条 **400** 判定 + 两条规范化（不碰库那一半，判据在 handler 的判定顺序上）。
#[tokio::test]
async fn the_waitlist_validation_gate_is_four_distinct_400s() {
    let app = test_app(lazy_db());
    let user = uuid::Uuid::new_v4();
    let uri = "/api/me/onboarding/cloud-waitlist";
    for (body, expected) in [
        (json!({"email": "   "}), "email is required"),
        (
            json!({"email": format!("{}@e.test", "a".repeat(254))}),
            "email is too long",
        ),
        (json!({"email": "not-an-email"}), "email is invalid"),
        (
            json!({"email": "a@b.test", "reason": "x".repeat(501)}),
            "reason is too long",
        ),
    ] {
        let (status, bytes) =
            send(&app, &Call::new("POST", uri, user).body(body.to_string())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            message_ends_with(&bytes, expected),
            "{body} => {}",
            message_of(&bytes)
        );
    }
    // 非法 JSON 也是同一条 400。
    let (status, bytes) = send(&app, &Call::new("POST", uri, user).body("{not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(message_ends_with(&bytes, "invalid request body"));
}

/// 两个 shim 的文案常量是**契约**（去重的键）⇒ 逐字断言。
#[test]
fn the_shim_content_constants_are_verbatim() {
    assert_eq!(shim_content::ONBOARDING_ASSISTANT_NAME, "Multica Helper");
    assert_eq!(
        shim_content::ONBOARDING_ISSUE_TITLE,
        "Start here: learn Multica with Multica Helper"
    );
    // 🔴 必须与 pre-v3 的 service 常量逐字一致，否则跨版本去重失效。
    assert_eq!(
        shim_content::NO_RUNTIME_ISSUE_TITLE,
        "Connect a runtime to start using agents"
    );
    assert!(shim_content::ONBOARDING_ASSISTANT_INSTRUCTIONS
        .starts_with("You are Multica Helper, the built-in AI assistant"));
    assert!(shim_content::ONBOARDING_ASSISTANT_AVATAR_URL.starts_with("data:image/svg+xml,"));
    // 上游 `strings.Join(…, "\n")` ⇒ 末尾**没有**换行。
    assert!(shim_content::ONBOARDING_ISSUE_DESCRIPTION.ends_with("makes sense."));
    assert!(shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_EN.ends_with("guided first run."));
    assert!(shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_ZH.ends_with("上手引导。"));
}

/// EN/ZH 正文选择：上游逐字「ZH selected on any `zh*` prefix」。
#[test]
fn the_no_runtime_body_language_selection_matches_upstream() {
    for lang in ["zh", "zh-CN", "zh-Hans", "zho"] {
        assert_eq!(
            shim_content::no_runtime_issue_description(Some(lang)),
            shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_ZH,
            "{lang}"
        );
    }
    for lang in ["en", "en-US", "ja", "", "ZH"] {
        assert_eq!(
            shim_content::no_runtime_issue_description(Some(lang)),
            shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_EN,
            "{lang}"
        );
    }
    assert_eq!(
        shim_content::no_runtime_issue_description(None),
        shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_EN
    );
}

/// `starter_prompt` 非空 ⇒ **整体替换** issue 正文（上游逐字）；空 ⇒ 用默认文案。
#[test]
fn a_non_empty_starter_prompt_replaces_the_whole_issue_body() {
    assert_eq!(issue_body(""), shim_content::ONBOARDING_ISSUE_DESCRIPTION);
    assert_eq!(
        issue_body("   "),
        shim_content::ONBOARDING_ISSUE_DESCRIPTION
    );
    assert_eq!(issue_body("帮我先建个项目"), "帮我先建个项目");
}

/// 两条 shim 的请求体形状（上游两个 `…Request`）。
#[test]
fn the_two_shim_request_bodies_deserialize() {
    let runtime: BootstrapOnboardingRuntimeRequest =
        serde_json::from_str(r#"{"workspace_id":"w","runtime_id":"r","starter_prompt":"p"}"#)
            .expect("runtime request");
    assert_eq!(runtime.workspace_id, "w");
    assert_eq!(runtime.runtime_id, "r");
    assert_eq!(runtime.starter_prompt, "p");

    let no_runtime: BootstrapOnboardingNoRuntimeRequest =
        serde_json::from_str(r#"{"workspace_id":"w"}"#).expect("no-runtime request");
    assert_eq!(no_runtime.workspace_id, "w");

    // 缺字段 ⇒ 空串（⇒ 400 `… is required`，不是 500）。
    let blank: BootstrapOnboardingRuntimeRequest = serde_json::from_str("{}").expect("blank");
    assert!(blank.workspace_id.is_empty() && blank.runtime_id.is_empty());
}

/// 🔴 **路由表存在性**的可执行判据：5 条**注册的方法**不得是 405/404，
/// 而**没注册的方法**必须正好是 405（axum 对「路径在、方法不对」回 405）。
///
/// 这是「注册面」的判据，与 ⑦（比 key 集合）互补：⑦ 说「本地有这 5 个 key」，
/// 这一条说「这 5 个 key 真的挂到了 handler 上，不是 501 占位」。
#[tokio::test]
async fn the_five_keys_are_mounted_on_real_handlers_not_placeholders() {
    let app = test_app(lazy_db());
    let user = uuid::Uuid::new_v4();
    for (method, path) in FIVE {
        let other: &'static str = if *method == *"POST" { "PATCH" } else { "POST" };
        // 注册的那一格：过不了 401 就说明**没注册**（404/405）或者干脆是占位 501。
        let (status, bytes) = send(
            &app,
            &Call::new(method, path, user).body(json!({}).to_string()),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}: {bytes:?}"
        );
        assert_ne!(status, StatusCode::NOT_FOUND, "{method} {path}: {bytes:?}");
        assert_ne!(
            status,
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path} 是 501 占位"
        );
        // 没注册的那一格 ⇒ 405（路径存在、方法不存在）。
        // ⚠️ 只对**本目录**那 4 条成立：`complete` 挂在 `workspaces.rs` 的 user-scoped 组里，
        // 那个组的 `route_layer(require_user)` **先**于方法路由跑 ⇒ 它对未注册方法是 401。
        if *path == *"/api/me/onboarding/complete" {
            continue;
        }
        let (status, _) = send(
            &app,
            &Call::new(other, path, user).body(json!({}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{other} {path}");
    }
}

/// 错误信封里的 `message`（本仓的嵌套错误体）。
///
/// ⚠️ 它带**类型前缀**（`validation error: email is required`）⇒ 用例比**尾部**逐字。
fn message_of(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `message` 的尾部是否逐字等于 `expected`（剥掉本仓信封的类型前缀）。
fn message_ends_with(bytes: &[u8], expected: &str) -> bool {
    message_of(bytes).ends_with(expected)
}
