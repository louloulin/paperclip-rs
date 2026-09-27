//! M10-B1 的证据面（`docs/64` §6.5 的通用 `DoD` 第 5 条：每条路由至少一条用例）。
//!
//! 分两半，**判据不同**（与 `routes/onboarding/tests.rs` 同手法）：
//!
//! - **不碰库的那一半**（门 ⑤，本文件 + `download.rs` 子模块）：6 条键的**字面量**与
//!   上游 `upstream-routes.tsv` 逐字相等、router 真装得上、**无会话 401**、
//!   缺 workspace 头 400、`/signed-download` 无签名 **403**（fail-closed）、
//!   `cdn_signed` 键不出现（与「本片用本地 HMAC 签名」互为断言）、上游错误文案逐条；
//! - **真库的那一半**（门 ⑥，`tests/db.rs`，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）：
//!   6 条键的端到端、跨 workspace **404**、非成员 **404**、`DELETE` 的可见性 / 权限
//!   （上传者 ✓ / admin ✓ / 别人 **403** / 抓取上下文副本 **404**）、能力链接端到端、
//!   `/content` 与 `/download` 的响应头逐条。
//!
//! 🔴 **本片零出站**（6 条全是本地路由）⇒ 「替身纪律」的对应物是**零出站**。
//!
//! 共用件在 `tests/support.rs`（拆出来是门 ⑩ 的 800 行上限要求）；
//! `download.rs` 的**纯函数层**用例在 `tests/download.rs`（同一原因拆出）。

// 本文件是 `attachments/mod.rs` 的子模块（`#[path = "tests.rs"]`）⇒ 子模块目录落在
// `routes/attachments/`，三个子模块都用**显式** `#[path]` 指到 `tests/`。
#[path = "tests/db.rs"]
mod db;
#[path = "tests/db_bytes.rs"]
mod db_bytes;
#[path = "tests/download.rs"]
mod download;
#[path = "tests/fx.rs"]
mod fx;
#[path = "tests/support.rs"]
mod support;

use serde_json::Value;

use support::*;

/// 本片 6 条（method, 上游字面量）—— 与 `docs/fixtures/upstream-routes.tsv` 的 M3+ 段逐字相等。
///
/// 判据：`upstream-routes.tsv` 里 `/api/attachments` 与 `/api/issues/{id}/attachments`
/// 那 6 行与 [`SIX`] 逐字相同（「本地字面量必须与上游字面量**逐字**相同」是 ⑦ 的口径，
/// 而 ⑦ 比的是 key，这里补的是「本片声明的 6 条 = 上游那 6 条」这一层）。
pub(crate) const SIX: [(&str, &str); 6] = [
    ("GET", "/api/attachments/{id}"),
    ("DELETE", "/api/attachments/{id}"),
    ("GET", "/api/attachments/{id}/content"),
    ("GET", "/api/attachments/{id}/download"),
    ("GET", "/api/attachments/{id}/signed-download"),
    ("GET", "/api/issues/{id}/attachments"),
];

/// 本仓实际注册的 6 条（路径参数写 `:name` —— matchit 0.7 把 `{name}` 当**字面量**）。
pub(crate) const SIX_LOCAL: [(&str, &str); 6] = [
    ("GET", "/api/attachments/:id"),
    ("DELETE", "/api/attachments/:id"),
    ("GET", "/api/attachments/:id/content"),
    ("GET", "/api/attachments/:id/download"),
    ("GET", "/api/attachments/:id/signed-download"),
    ("GET", "/api/issues/:id/attachments"),
];

// --------------------------------------------------------------------------- //
// 形态门
// --------------------------------------------------------------------------- //

#[test]
fn six_local_literals_are_the_upstream_literals() {
    for ((um, up), (lm, local)) in SIX.iter().zip(SIX_LOCAL.iter()) {
        assert_eq!(um, lm, "method 分叉");
        assert_eq!(
            up.replace('{', ":").replace('}', ""),
            *local,
            "path 分叉（上游 {up} ⇒ 本地 {local}）"
        );
    }
}

#[test]
fn the_six_keys_are_the_ones_docs_64_counts_for_this_slice() {
    // `docs/64` §4.2 的账：B1 = 6 行（5 缺口 + 1 占位升级）。
    // 判据 = 上游 TSV 里 owner 为 `M3+` 且属附件面的行数（逐条 = 6）。
    let tsv = include_str!("../../../../../docs/fixtures/upstream-routes.tsv");
    let hits: Vec<&str> = tsv
        .lines()
        .filter(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            f.len() >= 3
                && f[2] == "M3+"
                && (f[1].starts_with("/api/attachments/") || f[1] == "/api/issues/{id}/attachments")
        })
        .collect();
    assert_eq!(hits.len(), SIX.len(), "TSV 里的附件面行数应为 6：{hits:?}");
    for (method, path) in SIX {
        let want = format!("{method}\t{path}\tM3+");
        assert!(hits.iter().any(|h| h.starts_with(&want)), "TSV 缺 {want}");
    }
}

#[tokio::test]
async fn router_builds_without_duplicate_registration() {
    // 同 path+method 重复注册 ⇒ axum **启动时 panic**（`docs/15` §9.6.6）。
    // 这条是「第 6 条不在 attachments 聚合里」这条设计决定的活判据。
    let _ = test_app(lazy_db());
}

#[tokio::test]
async fn no_trailing_slash_form_is_registered() {
    // 🔴 补尾斜杠 = `EXTRA_ALIAS`（本波 allowlist 0 数据行、没有豁免退路）。
    // 判据用「真实 router + 尾斜杠 URI」：真装了的话会走到 handler（401），
    // 没装的话落到 axum 的 **404 兜底**。匿名 + 尾斜杠 ⇒ 命中即 401、不命中即 404，
    // 两者**不可能**混淆。
    let app = test_app(lazy_db());
    let id = new_uuid();
    let (status, _, body) = call(&app, Call::anon("GET", format!("/api/attachments/{id}/"))).await;
    assert_eq!(
        status, 404,
        "尾斜杠形态被注册了 ⇒ EXTRA_ALIAS（实际 {body}）"
    );
}

// --------------------------------------------------------------------------- //
// 认证 / workspace 前置（门 ⑤，零库）
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn every_scoped_route_requires_a_session() {
    let app = test_app(lazy_db());
    let id = new_uuid();
    let cases: [(&str, String); 5] = [
        ("GET", format!("/api/attachments/{id}")),
        ("DELETE", format!("/api/attachments/{id}")),
        ("GET", format!("/api/attachments/{id}/content")),
        ("GET", format!("/api/attachments/{id}/download")),
        ("GET", format!("/api/issues/{id}/attachments")),
    ];
    for (method, uri) in cases {
        let (status, _, body) = call(&app, Call::anon(method, uri.clone())).await;
        assert_eq!(status, 401, "{method} {uri} 匿名应是 401，实际 {body}");
    }
}

#[tokio::test]
async fn workspace_scoped_routes_need_a_workspace_header() {
    // 两条 workspace 路由：会话有了但**没有** workspace 头 ⇒ 400（缺 `workspace_id`）。
    // `/download` 刻意**不**要它（那正是它能当原生 `<img src>` 用的原因）⇒ 它走的是
    // 另一条加载路径（自解析 workspace），这里的用例不覆盖它。
    let app = test_app(lazy_db());
    let user = new_uuid();
    let id = new_uuid();
    for uri in [
        format!("/api/attachments/{id}"),
        format!("/api/attachments/{id}/content"),
        format!("/api/issues/{id}/attachments"),
    ] {
        let (status, _, body) = call(&app, Call::authed("GET", uri.clone(), user)).await;
        assert_eq!(status, 400, "{uri} 缺 workspace 头应是 400，实际 {body}");
    }
}

#[tokio::test]
async fn signed_download_is_public_but_fails_closed_without_a_signature() {
    // 🔴 这条是本片最要紧的一条：**无签名 ⇒ 403**（不是 401、也不是 404）。
    // 「无签名」是**所有**无效情况的代表：缺 exp、缺 sig、过期、换 id、换 intent。
    let app = test_app(lazy_db());
    let id = new_uuid();
    for uri in [
        format!("/api/attachments/{id}/signed-download"),
        format!("/api/attachments/{id}/signed-download?exp=99999999999&sig=deadbeef"),
        format!("/api/attachments/{id}/signed-download?dl=1"),
    ] {
        let (status, _, body) = call(&app, Call::anon("GET", uri.clone())).await;
        assert_ne!(status, 401, "{uri} 公开路由不该要会话");
        assert_eq!(status, 403, "{uri} 无签名应是 403，实际 {body}");
        assert!(
            body.contains("invalid or expired download link"),
            "{uri} 403 文案要与上游逐字相同，实际 {body}"
        );
    }
}

#[test]
fn load_capability_cannot_escalate_itself_to_download_intent() {
    // 用**可注入**的纯函数层钉（改进程 env 会与并行测试竞态，判据 =
    // `routes/config/tests.rs:15-19`）：load 签名配 `dl=1` 验不过，反之亦然。
    use crate::routes::attachments::download::{
        capability_path, derive_capability_key, download_capability_path, verify_capability,
        CAPABILITY_DOWNLOAD_INTENT,
    };
    let key = derive_capability_key("unit-test-secret");
    let id = "11111111-1111-1111-1111-111111111111";
    let load = capability_path(Some(&key), id, 1_000);
    assert!(load.contains("exp=1060"), "TTL 必须是 60s：{load}");
    let load_sig = load.split("sig=").nth(1).expect("sig");
    assert!(verify_capability(
        Some(&key),
        id,
        "1060",
        load_sig,
        "",
        1_000
    ));
    // 🔴 load 签名配 download 意图验不过。
    assert!(!verify_capability(
        Some(&key),
        id,
        "1060",
        load_sig,
        CAPABILITY_DOWNLOAD_INTENT,
        1_000
    ));
    // 🔴 反向：download 签名配 load 意图也验不过（两域真分隔，不是同一签名）。
    let dl = download_capability_path(Some(&key), id, 1_000);
    let dl_sig = dl
        .split("sig=")
        .nth(1)
        .expect("sig")
        .split('&')
        .next()
        .expect("no &");
    assert!(verify_capability(
        Some(&key),
        id,
        "1060",
        dl_sig,
        CAPABILITY_DOWNLOAD_INTENT,
        1_000
    ));
    assert!(!verify_capability(
        Some(&key),
        id,
        "1060",
        dl_sig,
        "",
        1_000
    ));
}

#[tokio::test]
async fn malformed_attachment_id_is_400_not_404() {
    // 上游 `parseUUIDOrBadRequest` ⇒ 400（不是 404）。
    //
    // ⚠️ 要**带会话**才轮得到 400：提取器在参数表里先跑 ⇒ 匿名请求先吃 **401**。
    // 这个顺序本身就是本片该钉的东西（不能为了早点报 400 而跳过认证）。
    let app = test_app(lazy_db());
    let user = new_uuid();
    let ws = new_uuid();
    for uri in [
        "/api/attachments/not-a-uuid",
        "/api/attachments/not-a-uuid/content",
    ] {
        let (status, _, body) = call(&app, Call::new("GET", uri, user, ws)).await;
        assert_eq!(status, 400, "{uri} 实际 {status} / {body}");
        assert!(
            body.contains("attachment id must be a uuid"),
            "{uri} 实际 {body}"
        );
    }
    // `/download` **要会话**（它只是**不要** workspace 头）⇒ 同样带会话。
    let (status, _, body) = call(
        &app,
        Call::authed("GET", "/api/attachments/not-a-uuid/download", user),
    )
    .await;
    assert_eq!(status, 400, "实际 {status} / {body}");
    assert!(body.contains("attachment id must be a uuid"), "实际 {body}");
    // `/signed-download` 也是公开路由 ⇒ 同款。
    let (status, _, body) = call(
        &app,
        Call::anon("GET", "/api/attachments/not-a-uuid/signed-download"),
    )
    .await;
    assert_eq!(status, 400, "实际 {status} / {body}");
    assert!(body.contains("attachment id must be a uuid"), "实际 {body}");
}

// --------------------------------------------------------------------------- //
// 与 `/api/config` 的互为断言（`docs/64` §2.2 第 2 行）
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn cdn_signed_stays_absent_because_we_sign_locally() {
    // 本片用 `mc-storage` 自己的 HMAC、**不引 CloudFront** ⇒ `/api/config` 的
    // `cdn_signed` 必须**恒 false** 且因 `omitempty` **键不出现**。
    // 这条把「本片的签名口径」与「M10-4 的 config 字段」钉成一对断言。
    let app = test_app(lazy_db());
    let (status, _, body) = call(&app, Call::anon("GET", "/api/config")).await;
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).expect("config is json");
    assert!(
        !v.as_object().expect("object").contains_key("cdn_signed"),
        "cdn_signed 必须不出现（omitempty + 恒 false），实际 {body}"
    );
    // 对照格：`cdn_domain` 无 `omitempty` ⇒ **键总出现**（缺省空串）。
    assert_eq!(v["cdn_domain"], "", "实际 {body}");
}

// --------------------------------------------------------------------------- //
// 「确实走到了查库那一步」+ 上游文案
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn unreachable_database_yields_404_not_a_silent_pass() {
    // 库**不可达**（`UNREACHABLE_DB`）+ 合法 workspace 头 ⇒ 成员校验查库失败
    // ⇒ 按「非成员」那一格归到 **404**（本片把两种 deny 归一到一个码，见 `download.rs`）。
    // 这条钉住「请求确实走到了查库那一步」—— 否则一个恒 404 的假实现也能绿。
    let app = test_app(lazy_db());
    let user = new_uuid();
    let ws = new_uuid();
    let id = new_uuid();
    let (status, _, body) = call(
        &app,
        Call::new("GET", format!("/api/attachments/{id}"), user, ws),
    )
    .await;
    assert_eq!(status, 404, "实际 {status} / {body}");
    assert!(body.contains("not_found"), "实际 {body}");
}

#[test]
fn status_code_constants_used_by_tests_are_the_upstream_ones() {
    use crate::routes::attachments::delete::ERR_NOT_AUTHORIZED;
    use crate::routes::attachments::download::{
        ERR_BAD_LINK, ERR_NOT_PREVIEWABLE, ERR_OBJECT_MISSING, ERR_PREVIEW_TOO_LARGE,
        ERR_STORAGE_OFF, PREVIEW_CSP, PREVIEW_MAX_BYTES,
    };
    assert_eq!(PREVIEW_MAX_BYTES, 2 << 20);
    assert_eq!(ERR_BAD_LINK, "invalid or expired download link");
    assert_eq!(
        ERR_NOT_PREVIEWABLE,
        "preview not supported for this file type"
    );
    assert_eq!(ERR_PREVIEW_TOO_LARGE, "file too large for inline preview");
    assert_eq!(ERR_OBJECT_MISSING, "attachment object not found");
    assert_eq!(ERR_STORAGE_OFF, "storage not configured");
    assert_eq!(
        ERR_NOT_AUTHORIZED,
        "not authorized to delete this attachment"
    );
    assert_eq!(PREVIEW_CSP, "default-src 'none'; frame-ancestors 'self'");
}

#[tokio::test]
async fn the_placeholder_route_is_really_upgraded() {
    // 第 6 条**刻意**留在 `routes/issues/mod.rs`（占位升级，不是新增键）。
    // 带 workspace 头 + 会话 + 一个**不存在的** issue ⇒ 404（不是 **501**）。
    // 501 = 占位没被换掉；这是本片对「占位升级」这条纪律的活判据。
    let app = test_app(lazy_db());
    let user = new_uuid();
    let ws = new_uuid();
    let (status, _, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/issues/{}/attachments", new_uuid()),
            user,
            ws,
        ),
    )
    .await;
    assert_ne!(status, 501, "501 说明 501 占位没被升级掉：{body}");
    assert_eq!(status, 404, "实际 {status} / {body}");
}
