//! 第 4–6 节：消息读面 + 游标分页、快捷栏（pinned agents）、draft-restore + project 锁。
//!
//! 真库那一半（门 ⑥，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。**纯移动**：
//! 断言与夹具调用逐字未改。

use super::support::{assert_err, connect, ids, new_message, raw_session, Ctx, AT, SC, SESSIONS};
use mc_core::Id;
use serde_json::json;
use uuid::Uuid;

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
    assert_err(&b, s, SC::NOT_FOUND, "chat session");

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
    // 游标 = **窗口里最旧一条**（上游在 `messages = messages[:limit]` 之后取 `len-1`，即多取
    // 2 行的那批里被截断的那条），且是纳秒形态（RFC3339Nano 去尾零）⇒ 这条是 `.5`。
    let cursor = page1["next_cursor"].clone();
    assert_eq!(cursor["created_at"], "2026-01-01T00:00:02.5Z", "{page1}");
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
    // 纳秒形态（上游 `oldest.CreatedAt.Time.Format(time.RFC3339Nano)`）：游标取**窗口里最旧一条**
    // ⇒ `limit=1` 时就是最新的那条；尾零去掉 ⇒ `.000000` 渲染成整秒、`.123456` 原样保留。
    let solo = raw_session(&ctx.pool, ctx.fx.ws, owner, ctx.fx.agents[0], true).await;
    let _ = new_message(&ctx.pool, solo, "user", "a", AT[0], "message").await;
    let _ = new_message(&ctx.pool, solo, "user", "b", AT[3], "message").await;
    let (_, page) = ctx
        .get(&format!("{SESSIONS}/{solo}/messages/page?limit=1"))
        .await;
    assert_eq!(
        page["next_cursor"]["created_at"], "2026-01-01T00:00:03Z",
        "{page}（`.000000` ⇒ 无小数点）"
    );
    let _ = new_message(&ctx.pool, solo, "user", "c", AT[4], "message").await;
    let (_, page) = ctx
        .get(&format!("{SESSIONS}/{solo}/messages/page?limit=1"))
        .await;
    assert_eq!(
        page["next_cursor"]["created_at"], "2026-01-01T00:00:04.123456Z",
        "{page}（非零小数位原样保留）"
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
    assert_err(&b, s, SC::NOT_FOUND, "chat session");

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
    assert_err(&b, s, SC::NOT_FOUND, "agent");
    let (s, b) = ctx
        .send(
            "POST",
            uri,
            peer,
            Some(&json!({"agent_id": ctx.fx.agents[0]}).to_string()),
        )
        .await;
    assert_err(&b, s, SC::NOT_FOUND, "agent");

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
    assert_err(&b, s, SC::NOT_FOUND, "agent");

    // 可见性：上游 `accessibleAgentIDs` 只在 `actorType == "member"` 时按
    // `memberAllowedToViewAgent` 过滤，而它读的 `ListAllAgents` 是
    // `WHERE workspace_id = $1 AND kind = 'user'`（**没有** `archived_at IS NULL`）
    // ⇒ **归档不等于不可见**，pin 保留（`chat_pinned_agent.go` 的 doc 注释写“archived →
    // dropped”，与自己的实现不符；以查询为准，见 `chat/session/support.rs::accessible_agent_ids`）。
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(ctx.fx.agents[1])
        .execute(&ctx.pool)
        .await
        .expect("archive agent");
    let (_, b) = ctx.get(uri).await;
    assert_eq!(b.as_array().unwrap().len(), 5, "{b}（归档 agent 仍算可见）");
    sqlx::query("UPDATE agent SET archived_at = NULL WHERE id = $1")
        .bind(ctx.fx.agents[1])
        .execute(&ctx.pool)
        .await
        .expect("unarchive agent");

    // 权限变更**会**丢 pin：peer 置顶的是 `shared`（peer **自己拥有**的 `public_to` + 命中 workspace
    // 白名单）。把它改成「不属于 peer 的 private agent」（`member_allowed_to_view`：`can_manage`
    // 已不成立、`permission_mode != public_to` 直接 false）⇒ 静默丢弃（行还在库里），改回又出现。
    sqlx::query("UPDATE agent SET permission_mode = 'private', owner_id = $2 WHERE id = $1")
        .bind(ctx.fx.shared)
        .bind(ctx.fx.owner)
        .execute(&ctx.pool)
        .await
        .expect("move agent away from peer");
    let (_, b) = ctx.send("GET", uri, peer, None).await;
    assert_eq!(
        b.as_array().unwrap().len(),
        0,
        "{b}（不可见 agent 的 pin 被丢弃）"
    );
    sqlx::query("UPDATE agent SET permission_mode = 'public_to', owner_id = $2 WHERE id = $1")
        .bind(ctx.fx.shared)
        .bind(ctx.fx.peer)
        .execute(&ctx.pool)
        .await
        .expect("restore agent owner/mode");
    let (_, b) = ctx.send("GET", uri, peer, None).await;
    assert_eq!(b.as_array().unwrap().len(), 1, "{b}");

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
        // `chat_draft_restore.id` **没有默认值**（上游 `CreateChatDraftRestore` 由调用方传入，
        // 约定是被删掉的那条 user 消息的 id）⇒ 直插的夹具自己造一个。
        restores.push(
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO chat_draft_restore(id, chat_session_id, task_id, content, created_at) \
                 VALUES ($1, $2, $3, $4, $5::timestamptz) RETURNING id",
            )
            .bind(Uuid::new_v4())
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
    assert_err(&b, s, SC::NOT_FOUND, "chat session");
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
    assert_err(&b, s, SC::NOT_FOUND, "chat session");
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
    assert_err(&b, s, SC::NOT_FOUND, "project");
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
    assert_err(&b, s, SC::NOT_FOUND, "project");
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
