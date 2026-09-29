//! `connect` 的离线替身端到端。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。
//! **纯移动**：断言与夹具调用逐字未改。

use super::*;

// ---------------------------------------------------------------------------
// connect：离线替身端到端
// ---------------------------------------------------------------------------

/// Forgejo 全链：路由 → `ValidateToken`（真 HTTP，走替身 `/api/v1/user`）→ 铸 secret →
/// `secretbox` 封装 → 真库 → 响应（含**一次性**明文）+ `webhook_url` 派生。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_forgejo_persists_ciphertext_only() {
    let fx = fixture!(ready);
    let instance = serve_stub(forgejo_stub("acme-bot", StatusCode::OK)).await;
    set_vcs_public_base("https://public.test");

    let (status, body, raw) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            &json!({
                "provider": "forgejo",
                "instance_url": format!("{instance}/"),   // 尾斜杠要被归一化掉
                "access_token": "fj-pat-DO-NOT-LOG",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "forgejo");
    assert_eq!(
        body["instance_url"], instance,
        "尾斜杠被 NormalizeInstanceURL 去掉"
    );
    assert_eq!(body["account_login"], "acme-bot");
    assert_eq!(body["workspace_id"], fx.ws.to_string());
    let connection_id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    assert_eq!(
        body["webhook_path"],
        format!("/api/webhooks/vcs/{connection_id}")
    );
    assert_eq!(
        body["webhook_url"],
        format!("https://public.test/api/webhooks/vcs/{connection_id}")
    );

    // 一次性明文 secret：64 位十六进制。
    let webhook_secret = body["webhook_secret"].as_str().expect("webhook_secret");
    assert_eq!(webhook_secret.len(), 64);
    assert!(webhook_secret.chars().all(|c| c.is_ascii_hexdigit()));

    // **响应里没有 PAT 的明文**（只有 webhook_secret 这一次性明文是允许的）。
    assert!(!raw.contains("fj-pat-DO-NOT-LOG"), "响应回显了 PAT：{raw}");
    // 响应里也**没有**密文（两个 `*_encrypted` 列不进任何 DTO）。
    assert!(!raw.contains("encrypted"));

    // 库里：两个凭据列都是**密文**（明文入库即失败），且能解回原值。
    let (row_id, provider, token_enc, secret_enc) = fx
        .raw_connection(fx.ws)
        .await
        .expect("connection row exists");
    assert_eq!(row_id, connection_id);
    assert_eq!(provider, "forgejo");
    assert_ne!(token_enc, "fj-pat-DO-NOT-LOG");
    assert_ne!(secret_enc, webhook_secret);
    assert!(!token_enc.contains("fj-pat-DO-NOT-LOG"));

    let opened_token = open(&token_enc);
    assert_eq!(opened_token, "fj-pat-DO-NOT-LOG");
    let opened_secret = open(&secret_enc);
    assert_eq!(opened_secret, webhook_secret);

    // 列表：**不**含任何凭据（连一次性明文都不再出现）。
    let (status, list, list_raw) =
        call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["connections"].as_array().expect("array").len(), 1);
    assert!(
        !list_raw.contains(webhook_secret),
        "列表回显了 webhook secret"
    );
    assert!(!list_raw.contains("fj-pat-DO-NOT-LOG"));

    reset_vcs_public_base();
    fx.teardown().await;
}

/// GitLab 全链：走 `/api/v4/user`（**不同**的 API 前缀与头），回复仍进同一张表。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_gitlab_uses_api_v4() {
    let fx = fixture!(ready);
    let instance = serve_stub(gitlab_stub("gl-bot", StatusCode::OK)).await;

    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            &json!({
                "provider": "gitlab",
                "instance_url": instance,
                "access_token": "glpat-DO-NOT-LOG",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "gitlab");
    assert_eq!(body["account_login"], "gl-bot");

    let (_, provider, _, _) = fx.raw_connection(fx.ws).await.expect("row");
    assert_eq!(provider, "gitlab");

    fx.teardown().await;
}

/// 同实例重连 = **原地轮换**（`UNIQUE (workspace_id, instance_url)`），不产生第二行。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn reconnect_same_instance_rotates_in_place() {
    let fx = fixture!(ready);
    let instance = serve_stub(forgejo_stub("acme-bot", StatusCode::OK)).await;
    let body = json!({
        "provider": "gitea",
        "instance_url": instance,
        "access_token": "fj-pat-2",
    })
    .to_string();

    let (status, first, _) = send(
        &fx.app,
        req_json("POST", &fx.connections_uri(), Some(fx.admin), &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second, _) = send(
        &fx.app,
        req_json("POST", &fx.connections_uri(), Some(fx.admin), &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");

    assert_eq!(first["id"], second["id"], "同一实例 ⇒ 同一连接行");
    assert_ne!(
        first["webhook_secret"], second["webhook_secret"],
        "每次 connect 都铸新 secret"
    );
    assert_eq!(second["provider"], "gitea", "重连可以换 provider 标签");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vcs_connection WHERE workspace_id = $1")
            .bind(fx.ws)
            .fetch_one(&fx.pool)
            .await
            .expect("count");
    assert_eq!(count, 1);

    fx.teardown().await;
}

/// provider / URL / 字段的四个 400 分支 + 两个出站失败的映射（400 / 502）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_rejects_bad_requests_and_maps_outbound_failures() {
    let fx = fixture!(ready);
    let uri = fx.connections_uri();

    // 未注册的 provider / 空 provider（Go 的零值）⇒ 400 `unsupported provider`。
    for payload in [
        json!({"provider": "github", "instance_url": "https://git.test", "access_token": "t"}),
        json!({"instance_url": "https://git.test", "access_token": "t"}),
        // `null` body：Go 的 Decode 是 no-op ⇒ 零值 ⇒ 同样 400 unsupported provider。
        Value::Null,
    ] {
        let (status, body, _) = send(
            &fx.app,
            req_json("POST", &uri, Some(fx.admin), &payload.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
        assert_eq!(error_code(&body), Some("validation_error"), "{payload}");
    }

    // 缺 instance_url / token ⇒ 400（上游文案 + 本仓统一的 `validation error: ` 前缀，
    // docs/40 §5 / `tests/autopilots/trigger_crud.rs` 同款）。
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"https://git.test"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: instance_url and access_token are required"
    );

    // URL 形态 ⇒ 400。
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"git.test","access_token":"t"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: instance_url must be an absolute http(s) URL"
    );

    // 非法 JSON ⇒ 400 `invalid request body`。
    let (status, body, _) = send(&fx.app, req_json("POST", &uri, Some(fx.admin), "{")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: invalid request body"
    );

    // 替身回 401（token 被拒）⇒ 400（**不是** 502）。
    let rejecting = serve_stub(forgejo_stub("acme-bot", StatusCode::UNAUTHORIZED)).await;
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            &json!({"provider": "forgejo", "instance_url": rejecting, "access_token": "bad"})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: the provider rejected the access token"
    );

    // 连不上的实例（端口 1 无人监听）⇒ 502。
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            &json!({"provider": "forgejo", "instance_url": "http://127.0.0.1:1", "access_token": "t"})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), Some("upstream_error"));

    // 出站失败一个行都没落。
    assert!(fx.raw_connection(fx.ws).await.is_none());

    fx.teardown().await;
}
