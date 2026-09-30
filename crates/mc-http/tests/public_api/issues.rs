//! `/v1/issues*`（4 条）的读写面与第三步授权。

use super::support::*;
use axum::http::StatusCode;
use mc_plugin_host::token::ActorKind;
use serde_json::json;

const SCOPES: &[&str] = &[
    "issues:read",
    "issues:write",
    "comments:read",
    "comments:write",
];

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn get_issue_by_uuid_and_by_identifier_agree() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "readable").await;
    let app = app(db);

    let (status, headers, by_id) = call_raw(
        &app,
        token_req(
            "GET",
            &format!("/v1/issues/{}", issue.id),
            &fixture.token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 乐观并发的 ETag 是弱验证器，值就是 revision（上游 `setPublicIssueETag`）。
    assert_eq!(
        headers.get("etag").unwrap(),
        &format!("W/\"{}\"", issue.revision)
    );
    assert_eq!(by_id["id"], issue.id.to_string());
    assert_eq!(by_id["identifier"], issue.identifier);
    assert_eq!(by_id["description"], "itest description");
    assert!(by_id["metadata"].is_object());
    assert_eq!(by_id["revision"], 1);

    // 同一个 issue 换一种写法（identifier）必须解析到同一行、同一份字节。
    let (_, _, by_identifier) = call_raw(
        &app,
        token_req(
            "GET",
            &format!("/v1/issues/{}", issue.identifier),
            &fixture.token,
            None,
        ),
    )
    .await;
    assert_eq!(by_id, by_identifier);

    // 不存在 / 乱写的引用 ⇒ 404（不是 400：`issue_ref` 是不透明引用）。
    for reference in [
        "PLUG-9999",
        "not-a-uuid",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let (status, _, body) = call_raw(
            &app,
            token_req(
                "GET",
                &format!("/v1/issues/{reference}"),
                &fixture.token,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{reference}");
        assert_eq!(body["detail"], "issue not found", "{reference}");
    }

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn patch_issue_touches_only_title_and_description() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "patchable").await;
    let app = app(db);
    let uri = format!("/v1/issues/{}", issue.id);

    // 只给 title ⇒ 200 + revision 前进 + ETag 跟着走。
    let (status, headers, body) = call_raw(
        &app,
        token_req(
            "PATCH",
            &uri,
            &fixture.token,
            Some(&json!({"title": "renamed"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "renamed");
    assert_eq!(body["description"], "itest description", "未给的字段不动");
    assert_eq!(body["revision"], issue.revision + 1);
    assert_eq!(
        headers.get("etag").unwrap(),
        &format!("W/\"{}\"", issue.revision + 1)
    );
    // title/description 之外的一切都不能经由本端点改动（上游注释：每个都带派发/目录/层级语义）。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PATCH",
            &uri,
            &fixture.token,
            Some(&json!({"status": "done"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "title or description is required");

    // 空 title ⇒ 400（上游只判「sanitize 之后为空」，**不** trim）。
    let (status, _, body) = call_raw(
        &app,
        token_req("PATCH", &uri, &fixture.token, Some(&json!({"title": ""}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "title must not be empty");
    // 只有空白字符**不是**空串（上游逐字如此：`sanitizeNullBytes` 只去 NUL）—— 这里锁住这个口径，
    // 免得后人自作聪明加一个 trim 而把上游放行的输入拒掉。
    let (status, _, body) = call_raw(
        &app,
        token_req("PATCH", &uri, &fixture.token, Some(&json!({"title": "  "}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "  ");
    // 空 body ⇒ 400。
    let (status, _, body) = call_raw(&app, token_req("PATCH", &uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "invalid request body");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn patch_issue_revision_conflicts_are_visible() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "conflict").await;
    let app = app(db);
    let uri = format!("/v1/issues/{}", issue.id);

    // `expected_revision` 是别人先改过 ⇒ 409 `revision_conflict`（客户端该重读再试，而不是覆盖）。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PATCH",
            &uri,
            &fixture.token,
            Some(&json!({"title": "stale", "expected_revision": 99})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "revision_conflict");
    assert_eq!(body["detail"], "resource changed since it was loaded");

    // `If-Match` 走同一条判定。
    let request = axum::http::Request::builder()
        .method("PATCH")
        .uri(&uri)
        .header("authorization", format!("Bearer {}", fixture.token))
        .header("content-type", "application/json")
        .header("if-match", "W/\"99\"")
        .body(axum::body::Body::from(
            json!({"title": "stale"}).to_string(),
        ))
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "revision_conflict");

    // 匹配的 revision ⇒ 通过（`expected_revision` 单独出现时就是乐观锁本体），且 revision 前进。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PATCH",
            &uri,
            &fixture.token,
            Some(&json!({"title": "fresh", "expected_revision": 1})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "fresh");
    assert_eq!(body["revision"], 2);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn patch_issue_if_match_and_body_revision_must_agree() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "ifmatch").await;
    let app = app(db);

    let request = axum::http::Request::builder()
        .method("PATCH")
        .uri(format!("/v1/issues/{}", issue.id))
        .header("authorization", format!("Bearer {}", fixture.token))
        .header("content-type", "application/json")
        .header("if-match", "W/\"1\"")
        .body(axum::body::Body::from(
            json!({"title": "x", "expected_revision": 2}).to_string(),
        ))
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "revision_mismatch");

    // 非法 `If-Match` ⇒ 400 `invalid_if_match`。
    let request = axum::http::Request::builder()
        .method("PATCH")
        .uri(format!("/v1/issues/{}", issue.id))
        .header("authorization", format!("Bearer {}", fixture.token))
        .header("content-type", "application/json")
        .header("if-match", "\"abc\"")
        .body(axum::body::Body::from(json!({"title": "x"}).to_string()))
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_if_match");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn member_comments_are_attributed_to_the_person_and_to_the_plugin() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "commented").await;
    let app = app(db);
    let uri = format!("/v1/issues/{}/comments", issue.id);

    // 会话（真人）发评论 ⇒ 署名是那个人，`via_plugin_id` 记下是哪个插件产生的。
    let (status, _, body) = call_raw(
        &app,
        session_req(
            "POST",
            &format!("/api/plugin-bridge/v1/issues/{}/comments", issue.id),
            fixture.user_id,
            fixture.installation_id,
            Some(&json!({"content": "hello from the panel"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["author_type"], "member");
    assert_eq!(body["author_id"], fixture.user_id.to_string());
    assert_eq!(body["content"], "hello from the panel");
    assert_eq!(body["type"], "comment");
    // 上游 `parent_id` 带 `omitempty` ⇒ 无父评论时字段**不出现**（不是空串）。
    assert!(body.get("parent_id").is_none(), "{body}");
    let comment_id: uuid::Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let via: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT via_plugin_id FROM comment WHERE id = $1")
            .bind(comment_id)
            .fetch_one(&pool)
            .await
            .expect("comment row");
    assert_eq!(
        via,
        Some(fixture.installation_id),
        "上游把这次写入记到插件头上（comment.via_plugin_id）"
    );

    // 安装令牌（没有真人）发评论 ⇒ 署名落到安装本身（`plugin`），而不是随便借一个成员。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "POST",
            &uri,
            &fixture.token,
            Some(&json!({"content": "from the plugin server"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["author_type"], "plugin");
    assert_eq!(body["author_id"], fixture.installation_id.to_string());

    // 回调令牌代表某个人 ⇒ 那次写入署那个人的名（写归属由**认证方式**决定）。
    let token = issue_callback_token(
        fixture.installation_id,
        fixture.workspace_id,
        ActorKind::Member,
        fixture.user_id,
        None,
    );
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "POST",
            &uri,
            &token,
            Some(&json!({"content": "via callback"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["author_type"], "member");

    // 列表：三条都在，按时间升序，类型是 `comment`。
    let listed_uri = format!("/api/plugin-bridge/v1/issues/{}/comments", issue.id);
    let (status, _, body) = call_raw(
        &app,
        session_req(
            "GET",
            &listed_uri,
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let comments = body["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 3, "{body}");
    assert_eq!(comments[0]["content"], "hello from the panel");
    assert_eq!(comments[2]["content"], "via callback");
    assert!(comments.iter().all(|c| c["type"] == "comment"));

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn comment_validation_and_thread_replies() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "threads").await;
    let other = seed_issue(&db, fixture.workspace_id, fixture.user_id, "other-thread").await;
    let app = app(db);
    let uri = format!("/v1/issues/{}/comments", issue.id);

    // 空内容 / 空 body ⇒ 400。
    let (status, _, body) = call_raw(
        &app,
        token_req("POST", &uri, &fixture.token, Some(&json!({"content": ""}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "content is required");
    let (status, _, body) = call_raw(&app, token_req("POST", &uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "invalid request body");

    // 父评论属于**另一个** issue ⇒ 400 `invalid_parent_comment`。
    let (_, _, root) = call_raw(
        &app,
        token_req(
            "POST",
            &uri,
            &fixture.token,
            Some(&json!({"content": "root"})),
        ),
    )
    .await;
    let (_, _, foreign) = call_raw(
        &app,
        token_req(
            "POST",
            &format!("/v1/issues/{}/comments", other.id),
            &fixture.token,
            Some(&json!({"content": "elsewhere"})),
        ),
    )
    .await;
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "POST",
            &uri,
            &fixture.token,
            Some(&json!({"content": "reply", "parent_id": foreign["id"]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "invalid parent comment");

    // 合法的回复 ⇒ 201，`parent_id` 回填。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "POST",
            &uri,
            &fixture.token,
            Some(&json!({"content": "reply", "parent_id": root["id"]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["parent_id"], root["id"]);

    // 非 uuid 的 parent_id ⇒ 400（上游 `util.ParseUUID` 的结论）。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "POST",
            &uri,
            &fixture.token,
            Some(&json!({"content": "reply", "parent_id": "nope"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "parent_id must be a valid UUID");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn comments_read_needs_its_own_scope() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let issue = seed_issue(
        &db,
        fixture.workspace_id,
        fixture.user_id,
        "scoped-comments",
    )
    .await;
    let app = app(db);

    let (status, _, body) = call_raw(
        &app,
        token_req(
            "GET",
            &format!("/v1/issues/{}/comments", issue.id),
            &fixture.token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        "this Plugin was not granted the comments:read scope"
    );

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

// ---------------------------------------------------------------------------
// 凭据门按台账声明分流（LUM-2610）
//
// 这一族在 conformance 回放里全回 401，根因是 `/v1` 合并点上的 `require_plugin_bearer`
// **只**认 `mpi_`/`mpc_`，于是所有用会话身份打过来的 `/v1/issues*` 在到达 handler 之前
// 就被毙了 —— 而 `mc_openapi::v1` 台账里这 4 条的 `policy.credentials` 明确含
// `CredentialKind::UserOAuth`（共享资源面），`/v1/context` 与 `/v1/storage/**`
// （插件扩展面）不含。这几条把「声明」和「实现」钉在一起。
// ---------------------------------------------------------------------------

/// 会话身份打 `/v1/issues/{ref}`：**200**（改前是 401 —— 门在 handler 之前就毙了）。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn session_credential_reaches_the_shared_resource_face() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "by-session").await;
    let app = app(db);

    let (status, _, body) = call_raw(
        &app,
        session_req(
            "GET",
            &format!("/v1/issues/{}", issue.id),
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "台账声明 user_oauth 是这 4 条的合法凭据，会话必须能走到 handler：{body}"
    );
    assert_eq!(body["id"], issue.id.to_string());

    // 令牌路径仍照旧（回归：这道门不是被拆掉，是被分流）。
    let (status, _, _) = call_raw(
        &app,
        token_req(
            "GET",
            &format!("/v1/issues/{}", issue.id),
            &fixture.token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// 插件扩展面**不**因为这次分流而放宽：`/v1/context` 拿会话仍然是 401。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn session_credential_is_still_refused_on_the_plugin_extension_face() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let app = app(db);

    let (status, _, body) = call_raw(
        &app,
        session_req(
            "GET",
            "/v1/context",
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "context 的 policy.credentials 只有两族插件令牌：{body}"
    );
    assert_eq!(body["code"], "plugin_bearer_required");
}

/// 匿名打 `/v1/issues/{ref}` 仍然是 401 —— 「声明允许会话」不等于「匿名也算过」。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn anonymous_is_still_refused_on_the_shared_resource_face() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "anonymous").await;
    let app = app(db);

    let (status, _, body) = call_raw(
        &app,
        axum::http::Request::builder()
            .method("GET")
            .uri(format!("/v1/issues/{}", issue.id))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "plugin_bearer_required");
}

/// **这一条是 conformance 那一族剩下的唯一站点的实测钉子**。
///
/// 回放发过来的会话请求带的是**空的** `X-Multica-Plugin-Installation`（上游那份 fixture 里
/// 这个头是 `""`：真正的凭据是安装令牌 `mpi_…`，而走查解不出明文、整头丢失，见 `docs/37`
/// §233.4 那一族）。所以请求过了凭据门之后停在 `session_caller` 的安装解析上，回
/// **400 `plugin installation is required`**（上游 `pluginSessionCaller` 逐字要求安装头：
/// workspace 只能来自安装行，否则调用方能把安装指向它从未被安装过的 workspace）。
///
/// 把它写成用例，是为了让「这 4 条为什么修不到 200」有一处**可复跑的证据**，
/// 而不是靠回忆上游测试做了什么。
#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn session_without_an_installation_stops_at_the_installation_check() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, SCOPES, "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "no-install").await;
    let app = app(db);

    let request = axum::http::Request::builder()
        .method("GET")
        .uri(format!("/v1/issues/{}", issue.id))
        .header(USER_ID_HEADER, fixture.user_id.to_string())
        .header(INSTALLATION_HEADER, "")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "过了凭据门之后，停在安装解析：{body}"
    );
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["detail"], "plugin installation is required");
}
