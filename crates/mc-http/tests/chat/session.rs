//! 第 2–3 节：会话的创建 / 读取 / 列表可见性，以及 pin / archive / read / 更新 / 删除。
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
    assert_err(&b, s, SC::NOT_FOUND, "agent");
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
    assert_err(&b, s, SC::NOT_FOUND, "workspace");
    let (s, b) = ctx.get(&format!("{SESSIONS}/{}", Uuid::new_v4())).await;
    assert_err(&b, s, SC::NOT_FOUND, "chat session");
    let (s, b) = ctx.get(&format!("{SESSIONS}/not-a-uuid")).await;
    assert_err(&b, s, SC::BAD_REQUEST, "invalid chat session id");

    // 列表可见性 1：隐藏渠道会话（无 `explicitly_created_at` + 只有 `channel_command`）不进列表；
    // 单个隐藏会话走公开门也是 404。
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
    let (_, b) = ctx.get(SESSIONS).await;
    assert_eq!(ids(&b), vec![id.clone()], "{b}（隐藏渠道会话不该出现）");
    let (s, b) = ctx.get(&format!("{SESSIONS}/{hidden}")).await;
    assert_err(&b, s, SC::NOT_FOUND, "chat session");

    // 列表可见性 2：agent 可见性白名单（上游 `accessibleAgentIDs` → `memberAllowedToViewAgent`
    // L192 / `canAccessPrivateAgent` L150）：**workspace owner/admin 与 agent owner 不受限**
    // （"workspace owner/admin pass (governance / inventory visibility retained)"），
    // 普通 member 只看得到「自己拥有的 agent」或「`public_to` 且命中 workspace/member 白名单」的 agent。
    let foreign = raw_session(&ctx.pool, ctx.fx.ws, owner, ctx.fx.peer_private, true).await;
    let allowed = raw_session(&ctx.pool, ctx.fx.ws, peer, ctx.fx.shared, true).await;
    let denied = raw_session(&ctx.pool, ctx.fx.ws, peer, ctx.fx.agents[0], true).await;
    let (_, b) = ctx.get(SESSIONS).await;
    assert!(
        ids(&b).contains(&foreign.to_string()),
        "{b}（owner 角色不受 agent 可见性限制）"
    );
    let (s, b) = ctx.send("GET", SESSIONS, peer, None).await;
    assert_eq!(s, SC::OK, "{b}");
    let peer_list = ids(&b);
    assert!(
        peer_list.contains(&allowed.to_string()),
        "{b}（`public_to` + workspace 白名单 ⇒ member 可见）"
    );
    assert!(
        !peer_list.contains(&denied.to_string()),
        "{b}（别人的 private agent 对 member 不可见）"
    );
    assert!(
        !peer_list.contains(&foreign.to_string()),
        "{b}（creator 过滤：列表只含自己建的会话）"
    );

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

    // 删除：别人的会话 403（弱归属门）；自己的会话 204；**已删的会话再删 404**
    // （上游 `DeleteChatSession` L677 先走 `loadChatSessionForUser` L252 ⇒ 行没了就是 404
    // `chat session not found`，`LockChatSessionForDelete` 的幂等 204 只覆盖「读到又被别人删掉」
    // 的竞态窗口）；隐藏渠道会话也能被删（清理面不看公开门）。
    let (s, b) = ctx.send("DELETE", &uri, peer, None).await;
    assert_err(&b, s, SC::FORBIDDEN, "not your chat session");
    let (s, b) = ctx.delete(&uri).await;
    assert_eq!(s, SC::NO_CONTENT, "{b}");
    let (s, b) = ctx.delete(&uri).await;
    assert_err(&b, s, SC::NOT_FOUND, "chat session");
    let (s, _) = ctx.get(&uri).await;
    assert_eq!(s, SC::NOT_FOUND);
    let hidden = raw_session(&ctx.pool, ws, owner, ctx.fx.agents[0], false).await;
    let (s, b) = ctx.delete(&format!("{SESSIONS}/{hidden}")).await;
    assert_eq!(s, SC::NO_CONTENT, "{b}");

    ctx.cleanup().await;
}
