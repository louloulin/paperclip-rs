//! `/api/chat/**`（M4-3 的 15 条路线 = 20 个「方法 × 路径」键）端到端测试。
//!
//! - [`route_paths_are_mounted`]：**不需要数据库**，用 `Db::connect_lazy` 装配完整 router，
//!   逐个打 20 个键。命中路由时「没带用户头」必然 401、「带了用户头没带 workspace」必然
//!   400（workspace 解析在成员校验之前，不碰 DB）；一旦某条路径写错（漏了尾斜杠别名、
//!   或用了 axum 0.8 的 `{id}` 字面量写法），就会掉到 404 兜底而失败。CI 无库也能挡回归。
//! - 其余 5 条需要真实 PG（`MULTICA_TEST_DATABASE_URL`），全部 `#[ignore]`。
//!
//! 夹具在 `tests/chat/support.rs`（门 ⑩ 的 800 行上限把两边分开；那个目录没有 `main.rs`
//! ⇒ cargo 不把它当独立 target，只是本文件的子模块）。
//!
//! 运行示例：
//! ```
//! cargo test -p mc-http --test chat --features test-util                 # 仅路由守卫
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test chat --features test-util -- --ignored  # 全量
//! ```
//!
//! 断言取向：**逐字对齐上游 Go**（错误文案、空体语义、秒精度时间戳、游标里的纳秒、幂等
//! 204、`ON CONFLICT` 位置复用），而不是「本仓实现现在返回什么」—— 实现漂了测试会红。

#![cfg(feature = "test-util")]

#[path = "chat/support.rs"]
mod support;

use mc_core::Id;
use serde_json::json;
use support::{assert_err, connect, ids, msg, new_message, raw_session, Ctx, AT, SC, SESSIONS};
use uuid::Uuid;

/// 建库夹具 + router；没有 `MULTICA_TEST_DATABASE_URL` 就跳过（`#[ignore]` 下的双保险）。
macro_rules! open_ctx {
    () => {{
        let Some((pool, db)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
            return;
        };
        Ctx::open(pool, db).await
    }};
}

// ---------------------------------------------------------------------------
// 1. 路由存在性守卫（无需数据库）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_paths_are_mounted() {
    let app = support::lazy_app();
    let sid = Uuid::new_v4().to_string();
    let rid = Uuid::new_v4().to_string();
    let aid = Uuid::new_v4().to_string();
    // 20 个键：`sessions` 根与 `:sessionId` 是 chi `Mount` 语义 ⇒ **两个形态都要在**；
    // 其余是 plain 子路由 ⇒ **只有无尾斜杠形态**（多一个会被门 ⑦ 判 `EXTRA_ALIAS`）。
    let paths: Vec<(&str, String)> = vec![
        ("POST", SESSIONS.to_string()),
        ("POST", format!("{SESSIONS}/")),
        ("GET", SESSIONS.to_string()),
        ("GET", format!("{SESSIONS}/")),
        ("GET", format!("{SESSIONS}/{sid}")),
        ("GET", format!("{SESSIONS}/{sid}/")),
        ("PATCH", format!("{SESSIONS}/{sid}")),
        ("PATCH", format!("{SESSIONS}/{sid}/")),
        ("DELETE", format!("{SESSIONS}/{sid}")),
        ("DELETE", format!("{SESSIONS}/{sid}/")),
        ("PATCH", format!("{SESSIONS}/{sid}/pin")),
        ("PATCH", format!("{SESSIONS}/{sid}/archive")),
        ("POST", format!("{SESSIONS}/{sid}/read")),
        ("GET", format!("{SESSIONS}/{sid}/messages")),
        ("GET", format!("{SESSIONS}/{sid}/messages/page")),
        ("GET", format!("{SESSIONS}/{sid}/draft-restores")),
        ("DELETE", format!("{SESSIONS}/{sid}/draft-restores/{rid}")),
        ("GET", "/api/chat/pinned-agents".to_string()),
        ("POST", "/api/chat/pinned-agents".to_string()),
        ("DELETE", format!("/api/chat/pinned-agents/{aid}")),
    ];
    for (method, path) in paths {
        // (a) 缺 `X-Multica-User-Id` → 401（extractor 阶段），绝不是 404。
        let (s, b) = support::call(&app, method, &path, None, None, None).await;
        assert_eq!(s, SC::UNAUTHORIZED, "{method} {path} → {b}");
        assert_eq!(b["error"]["code"], "unauthorized", "{method} {path}");
        // (b) 有用户、缺 workspace 上下文 → 400（workspace 解析早于成员校验，不碰 DB）。
        let (s, b) = support::call(&app, method, &path, Some(Uuid::new_v4()), None, None).await;
        assert_eq!(s, SC::BAD_REQUEST, "{method} {path} → {b}");
        assert_eq!(msg(&b), "validation error: invalid workspace id");
        // (c) workspace 不是 UUID → 同一个 400。
        let (s, b) = support::call(
            &app,
            method,
            &path,
            Some(Uuid::new_v4()),
            Some("not-a-uuid"),
            None,
        )
        .await;
        assert_eq!(s, SC::BAD_REQUEST, "{method} {path} → {b}");
        assert_eq!(msg(&b), "validation error: invalid workspace id");
    }
}

// ---------------------------------------------------------------------------
// 2. 创建 / 读取 / 列表可见性
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：创建校验 + 列表可见性连着读更清楚。
async fn create_get_and_list_visibility() {
    let ctx = open_ctx!();
    let (owner, peer, outsider) = (ctx.fx.owner, ctx.fx.peer, ctx.fx.outsider);
    let agent = ctx.fx.agents[0];

    // 空体 / 非对象体 → 400 `invalid request body`（Go `Decode` 的 EOF / 顶层类型错）。
    for raw in ["", "[]"] {
        let (s, b) = ctx.raw("POST", SESSIONS, raw).await;
        assert_err(&b, s, SC::BAD_REQUEST, "invalid request body");
    }
    // `{}` → `agent_id is required`；坏 UUID → `invalid agent_id`；不存在 → 404。
    let (s, b) = ctx.raw("POST", SESSIONS, "{}").await;
    assert_err(&b, s, SC::BAD_REQUEST, "agent_id is required");
    let (s, b) = ctx
        .raw("POST", SESSIONS, &json!({"agent_id": "nope"}).to_string())
        .await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid agent_id");
    let (s, b) = ctx
        .raw(
            "POST",
            SESSIONS,
            &json!({"agent_id": Uuid::new_v4()}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: agent");
    // peer 的 private agent：存在但 invoke 门不过 ⇒ 403（不是 404 —— 得先看得见才谈能不能用）。
    let (s, b) = ctx
        .raw(
            "POST",
            SESSIONS,
            &json!({"agent_id": ctx.fx.peer_private}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::FORBIDDEN, "you do not have access to this agent");
    // 归档 agent → 400 `agent is archived`。
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(ctx.fx.agents[1])
        .execute(&ctx.pool)
        .await
        .expect("archive agent");
    let (s, b) = ctx
        .raw(
            "POST",
            SESSIONS,
            &json!({"agent_id": ctx.fx.agents[1]}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::BAD_REQUEST, "agent is archived");

    // 正常创建：201 + 单会话形态（create **不** trim 标题）。
    let (s, b) = ctx
        .raw(
            "POST",
            SESSIONS,
            &json!({"agent_id": agent, "title": "  hi  "}).to_string(),
        )
        .await;
    assert_eq!(s, SC::CREATED, "{b}");
    let id = b["id"].as_str().expect("id").to_string();
    assert_eq!(b["workspace_id"], Id(ctx.fx.ws).as_string());
    assert_eq!(b["creator_id"], Id(owner).as_string());
    assert_eq!(b["agent_id"], Id(agent).as_string());
    assert_eq!(b["title"], "  hi  ");
    assert_eq!(b["status"], "active");
    assert_eq!(b["pinned"], false);
    assert_eq!(b["has_unread"], false);
    assert_eq!(b["unread_count"], 0);
    assert!(b["last_message"].is_null() && b["project_id"].is_null());
    let created = b["created_at"].as_str().expect("created_at");
    assert!(
        created.ends_with('Z') && !created.contains('.'),
        "秒精度：{created}"
    );
    assert!(!b.as_object().unwrap().contains_key("channel_source"));

    // 列表：200 裸数组；带尾斜杠的别名形态与无斜杠等价。
    let (s, b) = ctx.get(SESSIONS).await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(ids(&b), vec![id.clone()]);
    let (s, b) = ctx.get(&format!("{SESSIONS}/{id}/")).await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["id"], id.as_str());

    // 归属门：别人的会话 403，非成员 404，不存在 404，坏 id 400。
    let uri = format!("{SESSIONS}/{id}");
    let (s, b) = ctx.send("GET", &uri, peer, None).await;
    assert_err(&b, s, SC::FORBIDDEN, "not your chat session");
    let (s, b) = ctx.send("GET", &uri, outsider, None).await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: workspace");
    let (s, b) = ctx.get(&format!("{SESSIONS}/{}", Uuid::new_v4())).await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: chat session");
    let (s, b) = ctx.get(&format!("{SESSIONS}/not-a-uuid")).await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid chat session id");

    // 列表可见性：隐藏渠道会话（无 `explicitly_created_at` + 只有 `channel_command`）与
    // 「agent 对调用者不可见」的会话都不进列表；单个隐藏会话走公开门也是 404。
    let hidden = raw_session(&ctx.pool, ctx.fx.ws, owner, agent, false).await;
    let _ = new_message(
        &ctx.pool,
        hidden,
        "assistant",
        "channel",
        AT[0],
        "channel_command",
    )
    .await;
    let foreign = raw_session(&ctx.pool, ctx.fx.ws, owner, ctx.fx.peer_private, true).await;
    let (_, b) = ctx.get(SESSIONS).await;
    assert_eq!(
        ids(&b),
        vec![id],
        "{b}（隐藏会话 / 不可见 agent 的会话都不该出现）"
    );
    assert!(!ids(&b).contains(&foreign.to_string()));
    let (s, b) = ctx.get(&format!("{SESSIONS}/{hidden}")).await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: chat session");

    ctx.cleanup().await;
}

// ---------------------------------------------------------------------------
// 3. pin / archive / read / 更新 / 删除
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：四个开关 + 删除幂等连着读更清楚。
async fn flags_update_and_delete() {
    let ctx = open_ctx!();
    let (ws, owner, peer) = (ctx.fx.ws, ctx.fx.owner, ctx.fx.peer);
    let id = ctx.create(ctx.fx.agents[0], "flags").await;
    let uri = format!("{SESSIONS}/{id}");

    // pin：`true` → pinned；缺字段 / 显式 null → false（Go 里 null 进非指针是 no-op）。
    let (s, b) = ctx
        .raw("PATCH", &format!("{uri}/pin"), r#"{"pinned":true}"#)
        .await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["pinned"], true);
    for raw in ["{}", r#"{"pinned":null}"#] {
        let (s, b) = ctx.raw("PATCH", &format!("{uri}/pin"), raw).await;
        assert_eq!(s, SC::OK, "{b}");
        assert_eq!(b["pinned"], false, "{raw} → {b}");
    }
    for raw in ["", r#"{"pinned":"yes"}"#] {
        let (s, b) = ctx.raw("PATCH", &format!("{uri}/pin"), raw).await;
        assert_err(&b, s, SC::BAD_REQUEST, "invalid request body");
    }

    // archive：默认列表不再出现，`?status=all` 仍在；取消归档回到 active。
    let (s, b) = ctx
        .raw("PATCH", &format!("{uri}/archive"), r#"{"archived":true}"#)
        .await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["status"], "archived");
    let (_, b) = ctx.get(SESSIONS).await;
    assert_eq!(b.as_array().unwrap().len(), 0, "{b}");
    let (_, b) = ctx.get(&format!("{SESSIONS}?status=all")).await;
    assert_eq!(ids(&b), vec![id.clone()], "{b}");
    let (s, b) = ctx
        .raw("PATCH", &format!("{uri}/archive"), r#"{"archived":false}"#)
        .await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["status"], "active");

    // read → 204（上游不回体）。
    let (s, b) = ctx.send("POST", &format!("{uri}/read"), owner, None).await;
    assert_eq!(s, SC::NO_CONTENT, "{b}");

    // update：title 去空白；「都缺」/「都给」/ 显式 null 都是 400；超 200 字符 400。
    for raw in [
        "{}",
        r#"{"title":null}"#,
        r#"{"title":"t","project_id":null}"#,
    ] {
        let (s, b) = ctx.raw("PATCH", &uri, raw).await;
        assert_err(
            &b,
            s,
            SC::BAD_REQUEST,
            "exactly one of title or project_id is required",
        );
    }
    let (s, b) = ctx
        .raw("PATCH", &uri, &json!({"title": "  renamed  "}).to_string())
        .await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["title"], "renamed");
    let (s, b) = ctx.raw("PATCH", &uri, r#"{"title":"   "}"#).await;
    assert_err(&b, s, SC::BAD_REQUEST, "title is required");
    let (s, b) = ctx
        .raw(
            "PATCH",
            &uri,
            &json!({"title": "字".repeat(201)}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::BAD_REQUEST, "title is too long");
    let (s, b) = ctx.raw("PATCH", &uri, r#"{"title":5}"#).await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid request body");

    // 删除：204 幂等；别人的会话 403（弱归属门）；隐藏渠道会话也能被删（清理面不看公开门）。
    let (s, b) = ctx.send("DELETE", &uri, peer, None).await;
    assert_err(&b, s, SC::FORBIDDEN, "not your chat session");
    for _ in 0..2 {
        let (s, b) = ctx.delete(&uri).await;
        assert_eq!(s, SC::NO_CONTENT, "{b}");
    }
    let (s, _) = ctx.get(&uri).await;
    assert_eq!(s, SC::NOT_FOUND);
    let hidden = raw_session(&ctx.pool, ws, owner, ctx.fx.agents[0], false).await;
    let (s, b) = ctx.delete(&format!("{SESSIONS}/{hidden}")).await;
    assert_eq!(s, SC::NO_CONTENT, "{b}");

    ctx.cleanup().await;
}

// ---------------------------------------------------------------------------
// 4. 消息读面 + 游标分页
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：全量读 + 两页翻页连着读更清楚。
async fn message_read_and_paging() {
    let ctx = open_ctx!();
    let (owner, peer) = (ctx.fx.owner, ctx.fx.peer);
    let id = ctx.create(ctx.fx.agents[0], "paging").await;
    let sid = Uuid::parse_str(&id).expect("session id");
    let base = format!("{SESSIONS}/{id}");

    // 4 条可见消息 + 1 条 `onboarding_kickoff`（隐藏）+ 1 条 `channel_command`（SQL 层排除）。
    let mut m = Vec::new();
    for (n, at) in AT.iter().enumerate().take(4) {
        let content = format!("c{n}");
        m.push(new_message(&ctx.pool, sid, "user", &content, at, "message").await);
    }
    let _ = new_message(
        &ctx.pool,
        sid,
        "assistant",
        "kickoff",
        AT[2],
        "onboarding_kickoff",
    )
    .await;
    let _ = new_message(
        &ctx.pool,
        sid,
        "assistant",
        "ping",
        AT[5],
        "channel_command",
    )
    .await;

    // 全量读：时间升序、隐藏种类与渠道控制记录都不出现、时间戳是秒精度。
    let (s, b) = ctx.get(&format!("{base}/messages")).await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(
        ids(&b),
        m.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        "{b}"
    );
    assert_eq!(b[0]["content"], "c0");
    assert_eq!(b[0]["created_at"], "2026-01-01T00:00:01Z");
    assert_eq!(b[0]["quick_actions"], json!([]));
    assert!(b[0]["task_id"].is_null() && b[0]["failure_reason"].is_null());
    assert!(!b.to_string().contains("kickoff") && !b.to_string().contains("ping"));
    let (s, b) = ctx
        .send("GET", &format!("{base}/messages"), peer, None)
        .await;
    assert_err(&b, s, SC::FORBIDDEN, "not your chat session");
    let missing = Uuid::new_v4();
    let (s, b) = ctx.get(&format!("{SESSIONS}/{missing}/messages")).await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: chat session");

    // 分页：`limit=2`。SQL 多取 2 行 ⇒ 隐藏行被滤掉后 `has_more` 仍然为真，窗口是最近两条。
    let (s, page1) = ctx.get(&format!("{base}/messages/page?limit=2")).await;
    assert_eq!(s, SC::OK, "{page1}");
    assert_eq!(page1["limit"], 2);
    assert_eq!(page1["has_more"], true, "{page1}");
    assert_eq!(
        ids(&page1["messages"]),
        vec![m[2].to_string(), m[3].to_string()],
        "{page1}"
    );
    // 游标 = 窗口里最旧一条，且是**纳秒形态**（RFC3339Nano 去尾零：`.000000` ⇒ 无小数点）。
    let cursor = page1["next_cursor"].clone();
    assert_eq!(cursor["created_at"], "2026-01-01T00:00:03Z", "{page1}");
    assert_eq!(cursor["id"], Id(m[2]).as_string());
    let next = format!(
        "{base}/messages/page?limit=2&before_created_at={}&before_id={}",
        cursor["created_at"].as_str().unwrap(),
        cursor["id"].as_str().unwrap()
    );
    let (s, page2) = ctx.get(&next).await;
    assert_eq!(s, SC::OK, "{page2}");
    assert_eq!(page2["has_more"], false, "{page2}");
    assert_eq!(
        ids(&page2["messages"]),
        vec![m[0].to_string(), m[1].to_string()]
    );
    assert!(
        !page2.as_object().unwrap().contains_key("next_cursor"),
        "`omitempty`：无下一页时字段整个不出现：{page2}"
    );
    // 纳秒形态的尾零：`.1` 保留、`.000000` 掉成整秒（上面那条）。
    let solo = raw_session(&ctx.pool, ctx.fx.ws, owner, ctx.fx.agents[0], true).await;
    let _ = new_message(&ctx.pool, solo, "user", "a", AT[0], "message").await;
    let _ = new_message(&ctx.pool, solo, "user", "b", AT[4], "message").await;
    let (_, page) = ctx
        .get(&format!("{SESSIONS}/{solo}/messages/page?limit=1"))
        .await;
    assert_eq!(
        page["next_cursor"]["created_at"], "2026-01-01T00:00:01.1Z",
        "{page}"
    );

    // 参数校验：`limit` 越界 / 非数字 → 400 `invalid limit`；游标只给一半 → 400 `invalid cursor`；
    // 且「不存在的会话 + 坏 limit」必须是 404（门在取参之前 —— 上游就是这个顺序）。
    for query in ["limit=0", "limit=101", "limit=x"] {
        let (s, b) = ctx.get(&format!("{base}/messages/page?{query}")).await;
        assert_err(&b, s, SC::BAD_REQUEST, "invalid limit");
    }
    let (s, b) = ctx
        .get(&format!(
            "{base}/messages/page?before_created_at=2026-01-01T00:00:03Z"
        ))
        .await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid cursor");
    let (s, b) = ctx
        .get(&format!("{SESSIONS}/{missing}/messages/page?limit=0"))
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: chat session");

    ctx.cleanup().await;
}

// ---------------------------------------------------------------------------
// 5. 快捷栏（pinned agents）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：幂等 + 上限 + 可见性连着读更清楚。
async fn pinned_agents_bar() {
    let ctx = open_ctx!();
    let peer = ctx.fx.peer;
    let uri = "/api/chat/pinned-agents";

    // 空栏 → 200 裸数组。
    let (s, b) = ctx.get(uri).await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b, json!([]));

    // 坏请求：缺字段 / 显式 null / 坏 UUID 都是 400 `invalid agent_id`（**没有**单独的
    // `is required` 文案：上游拿空串直接进 `parseUUIDOrBadRequest`）；非对象体 400。
    for raw in [
        "{}",
        "null",
        r#"{"agent_id":null}"#,
        r#"{"agent_id":"nope"}"#,
    ] {
        let (s, b) = ctx.raw("POST", uri, raw).await;
        assert_err(&b, s, SC::BAD_REQUEST, "invalid agent_id");
    }
    let (s, b) = ctx.raw("POST", uri, "[]").await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid request body");
    // 不存在的 agent / 对调用者不可见的 agent → 404 `not found: agent`。
    let (s, b) = ctx
        .raw(
            "POST",
            uri,
            &json!({"agent_id": Uuid::new_v4()}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: agent");
    let (s, b) = ctx
        .raw(
            "POST",
            uri,
            &json!({"agent_id": ctx.fx.peer_private}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: agent");

    // 逐个置顶：位置从 1 开始递增；`position` 虽是 float64 也渲染成整数 `1`。
    for (n, agent) in ctx.fx.agents.iter().take(5).enumerate() {
        let (s, b) = ctx
            .raw("POST", uri, &json!({"agent_id": agent}).to_string())
            .await;
        assert_eq!(s, SC::OK, "{b}");
        assert_eq!(b["position"], n + 1, "{b}");
        assert_eq!(b["agent_id"], Id(*agent).as_string());
    }
    // 已置顶的重放：幂等且**在上限判定之前** ⇒ 栏满时仍 200 且位置不变。
    let (s, b) = ctx
        .raw(
            "POST",
            uri,
            &json!({"agent_id": ctx.fx.agents[0]}).to_string(),
        )
        .await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["position"], 1);
    // 第 6 个不同 agent → 400 `pinned agent limit reached`。
    let (s, b) = ctx
        .raw(
            "POST",
            uri,
            &json!({"agent_id": ctx.fx.agents[5]}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::BAD_REQUEST, "pinned agent limit reached");
    // 别人（member）有自己的栏，互不影响。
    let peer_body = json!({"agent_id": ctx.fx.shared}).to_string();
    let (s, b) = ctx.send("POST", uri, peer, Some(&peer_body)).await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["position"], 1);
    let (_, b) = ctx.send("GET", uri, peer, None).await;
    assert_eq!(b.as_array().unwrap().len(), 1, "{b}");

    // 列表：按 position 升序；「格式合法但大写」的 UUID 复现上游的原始字符串比较 ⇒ 404。
    let (s, b) = ctx.get(uri).await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b.as_array().unwrap().len(), 5, "{b}");
    let upper = ctx.fx.agents[0].to_string().to_uppercase();
    let (s, b) = ctx
        .raw("POST", uri, &json!({"agent_id": upper}).to_string())
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: agent");

    // 看不见的 agent 从响应里静默丢弃（归档后消失、恢复后回来；行还在库里）。
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(ctx.fx.agents[1])
        .execute(&ctx.pool)
        .await
        .expect("archive agent");
    let (_, b) = ctx.get(uri).await;
    assert_eq!(b.as_array().unwrap().len(), 4, "{b}");
    sqlx::query("UPDATE agent SET archived_at = NULL WHERE id = $1")
        .bind(ctx.fx.agents[1])
        .execute(&ctx.pool)
        .await
        .expect("unarchive agent");
    let (_, b) = ctx.get(uri).await;
    assert_eq!(b.as_array().unwrap().len(), 5, "{b}");

    // 取消置顶：204 幂等，未知 agent 也 204；坏 UUID → 400 `invalid agentId`（路径参数名不同）。
    for agent in [ctx.fx.agents[0], ctx.fx.agents[0], Uuid::new_v4()] {
        let (s, b) = ctx.delete(&format!("{uri}/{agent}")).await;
        assert_eq!(s, SC::NO_CONTENT, "{b}");
    }
    let (s, b) = ctx.delete(&format!("{uri}/not-a-uuid")).await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid agentId");
    let (_, b) = ctx.get(uri).await;
    assert_eq!(b.as_array().unwrap().len(), 4, "{b}");

    ctx.cleanup().await;
}

// ---------------------------------------------------------------------------
// 6. draft-restore + project 锁
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：草稿 + 项目锁连着读更清楚。
async fn draft_restores_and_project_lock() {
    let ctx = open_ctx!();
    let (ws, peer) = (ctx.fx.ws, ctx.fx.peer);
    let id = ctx.create(ctx.fx.agents[0], "drafts").await;
    let sid = Uuid::parse_str(&id).expect("session id");
    let uri = format!("{SESSIONS}/{id}/draft-restores");

    // 两条草稿（弱归属门：没有 agent 可见性要求）。
    let task = Uuid::new_v4();
    let mut restores = Vec::new();
    for (n, at) in AT.iter().enumerate().take(2) {
        let content = format!("draft {n}");
        restores.push(
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO chat_draft_restore(chat_session_id, task_id, content, created_at) \
                 VALUES ($1, $2, $3, $4::timestamptz) RETURNING id",
            )
            .bind(sid)
            .bind(task)
            .bind(content)
            .bind(at)
            .fetch_one(&ctx.pool)
            .await
            .expect("insert draft"),
        );
    }

    // 200 包装对象（不是裸数组）；`attachments` 属 M4-4 ⇒ 整个省略。
    let (s, b) = ctx.get(&uri).await;
    assert_eq!(s, SC::OK, "{b}");
    let list = b["restores"].as_array().expect("restores");
    assert_eq!(list.len(), 2, "{b}");
    assert_eq!(list[0]["id"], Id(restores[0]).as_string());
    assert_eq!(list[0]["chat_session_id"], id.as_str());
    assert_eq!(list[0]["task_id"], Id(task).as_string());
    assert_eq!(list[0]["content"], "draft 0");
    assert_eq!(list[0]["created_at"], "2026-01-01T00:00:01Z");
    assert!(!list[0].as_object().unwrap().contains_key("attachments"));
    // 归属 / 不存在 / 坏 id。
    let (s, b) = ctx.send("GET", &uri, peer, None).await;
    assert_err(&b, s, SC::FORBIDDEN, "not your chat session");
    let missing = Uuid::new_v4();
    let (s, b) = ctx
        .get(&format!("{SESSIONS}/{missing}/draft-restores"))
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: chat session");
    let (s, b) = ctx
        .get("/api/chat/sessions/not-a-uuid/draft-restores")
        .await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid chat session id");

    // 消费：204 幂等（重试安全）；坏 restore id 400，但**会话门在前** ⇒ 不存在的会话 + 坏 id 是 404。
    for restore in [restores[0], restores[0]] {
        let (s, b) = ctx.delete(&format!("{uri}/{restore}")).await;
        assert_eq!(s, SC::NO_CONTENT, "{b}");
    }
    let (s, b) = ctx.delete(&format!("{uri}/not-a-uuid")).await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid restore id");
    let bogus = format!("{SESSIONS}/{missing}/draft-restores/not-a-uuid");
    let (s, b) = ctx.delete(&bogus).await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: chat session");
    let (_, b) = ctx.get(&uri).await;
    assert_eq!(b["restores"].as_array().unwrap().len(), 1, "{b}");

    // project：创建时锁项目（不存在 → 404 且不落行）；更新时 `null` 清空、坏值 400、不存在 404。
    let body = json!({"agent_id": ctx.fx.agents[1], "project_id": ctx.fx.project}).to_string();
    let (s, b) = ctx.raw("POST", SESSIONS, &body).await;
    assert_eq!(s, SC::CREATED, "{b}");
    let with_project = b["id"].as_str().unwrap().to_string();
    assert_eq!(b["project_id"], Id(ctx.fx.project).as_string());
    let count_sql = "SELECT count(*) FROM chat_session WHERE workspace_id = $1";
    let before: i64 = sqlx::query_scalar(count_sql)
        .bind(ws)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
    let body = json!({"agent_id": ctx.fx.agents[1], "project_id": Uuid::new_v4()}).to_string();
    let (s, b) = ctx.raw("POST", SESSIONS, &body).await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: project");
    let after: i64 = sqlx::query_scalar(count_sql)
        .bind(ws)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
    assert_eq!(before, after, "项目锁失败时不留半截会话");

    let puri = format!("{SESSIONS}/{with_project}");
    let (s, b) = ctx.raw("PATCH", &puri, r#"{"project_id":null}"#).await;
    assert_eq!(s, SC::OK, "{b}");
    assert!(b["project_id"].is_null(), "{b}");
    for raw in [r#"{"project_id":5}"#, r#"{"project_id":"  "}"#] {
        let (s, b) = ctx.raw("PATCH", &puri, raw).await;
        assert_err(&b, s, SC::BAD_REQUEST, "project_id must be a UUID or null");
    }
    let (s, b) = ctx
        .raw(
            "PATCH",
            &puri,
            &json!({"project_id": Uuid::new_v4()}).to_string(),
        )
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "not found: project");
    let (s, b) = ctx
        .raw(
            "PATCH",
            &puri,
            &json!({"project_id": ctx.fx.project}).to_string(),
        )
        .await;
    assert_eq!(s, SC::OK, "{b}");
    assert_eq!(b["project_id"], Id(ctx.fx.project).as_string());

    ctx.cleanup().await;
}
