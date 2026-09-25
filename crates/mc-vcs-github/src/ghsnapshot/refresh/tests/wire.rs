//! **离线 GraphQL 替身**上的用例（真 wire）：分页到完、三道守卫、null rollup、
//! `Retry-After`，以及「端口请求 → 解析 → 真 HTTP → 归一化 → head-SHA 守卫写」的端到端。
//!
//! 替身道具（`HttpDouble` / `github_double` / `Wire`）见 `super::super::test_support`。
//! 全部用例对着本机的 `TcpListener` 替身跑真 `reqwest` + 真 JWT 签名 + 真分页循环
//! （替身三条纪律之二：出站请求的头与体逐字段可比对）。

use super::*;

/// 分页到完（上游验收判据 2）：contexts 跨两页全部收齐，**绝不**假设 <100；
/// `StatusContext` 与 `CheckRun` 折成同一形状。
#[tokio::test]
async fn graphql_double_paginates_contexts_to_completion() {
    let double = github_double(|request| {
        let cursor = variables_of(request)
            .get("cursor")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if cursor.is_empty() {
            Wire::Data(json!({"repository": {"pullRequest": {
                "headRefOid": "sha1", "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                    "state": "FAILURE",
                    "contexts": {
                        "pageInfo": {"hasNextPage": true, "endCursor": "CUR2"},
                        "nodes": [
                            {"__typename": "CheckRun", "name": "backend", "status": "COMPLETED",
                             "conclusion": "FAILURE", "detailsUrl": "u1"},
                            {"__typename": "CheckRun", "name": "frontend", "status": "COMPLETED",
                             "conclusion": "SUCCESS", "detailsUrl": "u2"}
                        ]
                    }
                }}}]}
            }}}))
        } else {
            Wire::Data(json!({"repository": {"pullRequest": {
                "headRefOid": "sha1", "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                    "state": "FAILURE",
                    "contexts": {
                        "pageInfo": {"hasNextPage": false, "endCursor": ""},
                        "nodes": [
                            {"__typename": "CheckRun", "name": "e2e", "status": "IN_PROGRESS",
                             "conclusion": null, "detailsUrl": "u3"},
                            {"__typename": "StatusContext", "context": "vercel", "state": "SUCCESS",
                             "targetUrl": "u4"}
                        ]
                    }
                }}}]}
            }}}))
        }
    })
    .await;
    let client = enabled_client(double.base_url());
    let snapshot = fetch_pr_snapshot(&client, 7, "acme", "api", 5, real_now())
        .await
        .expect("two pages must be collected");
    assert_eq!(snapshot.head_sha, "sha1");
    assert_eq!(snapshot.mergeable.as_deref(), Some("MERGEABLE"));
    assert_eq!(snapshot.rollup_state.as_deref(), Some("FAILURE"));
    assert!(snapshot.has_checks);
    assert_eq!(snapshot.checks.len(), 4, "跨页 4 条 context");
    let status = &snapshot.checks[3];
    assert_eq!(status.name, "vercel");
    assert_eq!(status.status, "completed");
    assert_eq!(status.conclusion.as_deref(), Some("success"));
    assert!(status.is_status_context);
    assert_eq!(snapshot.checks[2].status, "in_progress");
    assert_eq!(snapshot.checks[2].conclusion, None);
    assert!(!snapshot.decided(), "有 in_progress 的 run ⇒ 未决");
    assert_eq!(double.requests_to("/graphql").len(), 2, "两页两次请求");
}

/// 替身纪律之二：**出站请求逐字段** —— 头（Accept / API 版本 / Content-Type /
/// `Bearer <installation token>`）、体（那一条查询 + 四个变量 + 首页游标为 `null`）。
#[tokio::test]
async fn offline_double_asserts_outbound_headers_and_query_body() {
    let double = github_double(|_| {
        Wire::Data(json!({"repository": {"pullRequest": {
            "headRefOid": "sha1", "mergeable": "MERGEABLE",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}
        }}}))
    })
    .await;
    let client = enabled_client(double.base_url());
    let _ = fetch_pr_snapshot(&client, 7, "acme", "api", 12, real_now())
        .await
        .expect("single page");

    let token_calls = double.requests_to("/access_tokens");
    assert_eq!(token_calls.len(), 1, "token 只换一次（换完进缓存）");
    assert_eq!(token_calls[0].method, "POST");
    assert_eq!(
        token_calls[0].path, "/app/installations/7/access_tokens",
        "路径逐字"
    );
    assert!(
        token_calls[0]
            .header("authorization")
            .unwrap_or_default()
            .starts_with("Bearer "),
        "必须带 App JWT"
    );
    assert!(token_calls[0].body.contains("\"metadata\":\"read\""));

    let graph_ql = double.requests_to("/graphql");
    assert_eq!(graph_ql.len(), 1);
    assert_eq!(graph_ql[0].method, "POST");
    assert_eq!(
        graph_ql[0].header("accept"),
        Some("application/vnd.github+json")
    );
    assert_eq!(
        graph_ql[0].header("x-github-api-version"),
        Some("2022-11-28")
    );
    assert_eq!(graph_ql[0].header("content-type"), Some("application/json"));
    assert_eq!(
        graph_ql[0].header("authorization"),
        Some("Bearer ghs_installation_token"),
        "GraphQL 必须用换来的 installation token"
    );
    let body = graph_ql[0].json();
    let query = body
        .get("query")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(query.contains("statusCheckRollup"), "查的是那一条查询");
    assert!(
        query.contains("contexts(first:100,after:$cursor)"),
        "带游标分页"
    );
    let variables = variables_of(&graph_ql[0]);
    assert_eq!(
        variables.get("owner").and_then(serde_json::Value::as_str),
        Some("acme")
    );
    assert_eq!(
        variables.get("repo").and_then(serde_json::Value::as_str),
        Some("api")
    );
    assert_eq!(
        variables.get("number").and_then(serde_json::Value::as_i64),
        Some(12)
    );
    assert!(
        variables
            .get("cursor")
            .is_none_or(serde_json::Value::is_null),
        "首页游标必须是 null（上游逐字）"
    );
}

/// 守卫一：分页途中 head 变了 ⇒ **整条作废**（不是拼一份混页快照）。
#[tokio::test]
async fn graphql_double_rejects_head_change_during_pagination() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let double = github_double(move |_| {
        let page = counter.fetch_add(1, Ordering::SeqCst);
        let head = if page == 0 { "shaA" } else { "shaB" };
        Wire::Data(json!({"repository": {"pullRequest": {
            "headRefOid": head, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "PENDING",
                "contexts": {"pageInfo": {"hasNextPage": page == 0, "endCursor": "CUR2"}, "nodes": []}
            }}}]}
        }}}))
    })
    .await;
    let client = enabled_client(double.base_url());
    let error = fetch_pr_snapshot(&client, 7, "acme", "api", 5, real_now())
        .await
        .expect_err("head 变更必须被拒绝");
    assert!(
        error.to_string().contains("head changed during pagination"),
        "{error}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "第二次就发现不一致，不做第三次"
    );
}

/// 守卫三：游标**不前进**的病态替身被截断在页数上限（上游
/// `TestFetchPRSnapshotRejectsPaginationBeyondLimit`）。
#[tokio::test]
async fn graphql_double_rejects_pagination_beyond_the_page_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let double = github_double(move |_| {
        let page = counter.fetch_add(1, Ordering::SeqCst) + 1;
        Wire::Data(json!({"repository": {"pullRequest": {
            "headRefOid": "shaA", "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "PENDING",
                "contexts": {
                    "pageInfo": {"hasNextPage": true, "endCursor": format!("CUR{page}")},
                    "nodes": []
                }
            }}}]}
        }}}))
    })
    .await;
    let client = enabled_client(double.base_url());
    let error = fetch_pr_snapshot(&client, 7, "acme", "api", 5, real_now())
        .await
        .expect_err("永不到头的翻页必须被截断");
    assert!(
        error.to_string().contains("pagination exceeds page limit"),
        "{error}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        crate::ghsnapshot::snapshot::MAX_SNAPSHOT_CONTEXT_PAGES
    );
}

/// 验收判据 5：`statusCheckRollup == null` ⇒ 没有 check（**绝不**当作通过）。
#[tokio::test]
async fn graphql_double_returns_null_rollup_as_no_checks() {
    let double = github_double(|_| {
        Wire::Data(json!({"repository": {"pullRequest": {
            "headRefOid": "sha9", "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}
        }}}))
    })
    .await;
    let client = enabled_client(double.base_url());
    let snapshot = fetch_pr_snapshot(&client, 7, "acme", "api", 5, real_now())
        .await
        .expect("null rollup 不是错误");
    assert!(!snapshot.has_checks);
    assert!(snapshot.checks.is_empty());
    assert!(!snapshot.decided());
}

/// 限流的 wire 形状：`403` + `retry-after: 90` ⇒ [`GithubError::RateLimited`] 带正确的秒数
/// （管道据此记 installation 级暂停）。
#[tokio::test]
async fn graphql_double_surfaces_secondary_rate_limit_with_retry_after() {
    let double = github_double(|_| Wire::RateLimited {
        retry_after_secs: 90,
    })
    .await;
    let client = enabled_client(double.base_url());
    let error = fetch_pr_snapshot(&client, 7, "acme", "api", 5, real_now())
        .await
        .expect_err("403 必须被翻译成限流");
    assert!(
        matches!(
            error,
            GithubError::RateLimited {
                retry_after_secs: 90
            }
        ),
        "{error:?}"
    );
}

/// **端到端（无真库）**：端口请求 → 解析地址 → 真 `reqwest` 打替身 → 归一化 →
/// head-SHA 守卫写。生产抓取器 `HttpSnapshotFetcher` 就在这条链上。
#[tokio::test]
async fn port_request_reaches_the_graphql_double_and_writes_the_row() {
    let double = github_double(|_| {
        Wire::Data(json!({"repository": {"pullRequest": {
            "headRefOid": "sha-wire", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "SUCCESS",
                "contexts": {"pageInfo": {"hasNextPage": false, "endCursor": ""},
                    "nodes": [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED",
                               "conclusion": "SUCCESS", "detailsUrl": "u1"}]}
            }}}]}
        }}}))
    })
    .await;
    let store = FixedStore::new().with_rows(vec![PrRowRef {
        id: mc_core::id::Id::new(),
        state: "open".into(),
    }]);
    let manager = Manager::with_options(
        Arc::new(enabled_client(double.base_url())),
        store.clone(),
        options(
            quiet_tuning(),
            &FakeClock::at(real_now()),
            Arc::new(RecordingTimer::default()),
            Arc::new(HttpSnapshotFetcher) as Arc<dyn SnapshotFetcher>,
        ),
    );
    manager.enqueue_request(&request(RefreshReason::Webhook, 9));
    manager.start().expect("start");
    assert!(
        wait_until(|| !store.applied().is_empty(), Duration::from_secs(5)).await,
        "写库没发生"
    );
    let applied = store.applied();
    let (_, snapshot, fetched_at) = &applied[0];
    assert_eq!(snapshot.head_sha, "sha-wire");
    assert_eq!(snapshot.mergeable.as_deref(), Some("CONFLICTING"));
    assert_eq!(snapshot.merge_state_status.as_deref(), Some("DIRTY"));
    assert_eq!(snapshot.checks.len(), 1);
    assert!(snapshot.decided());
    assert!(
        *fetched_at >= 1_600_000_000,
        "写库用的是注入的时钟：{fetched_at}"
    );
    assert_eq!(
        store.resolve_calls.load(Ordering::SeqCst),
        1,
        "解析只做一次"
    );
    assert_eq!(double.requests_to("/graphql").len(), 1);
    manager.shutdown().await;
}
