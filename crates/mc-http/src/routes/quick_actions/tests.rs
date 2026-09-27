//! M10-B3（`LUM-2114`）6 条 quick-action 键的证据面。
//!
//! 分两半，**依据不同**：
//!
//! - **本文件**（门 ⑤，零库）：键集的**逐字形状**（两形态 / 只一形态）、出库前的
//!   两层（401 / 400）、**不挂**机器凭据闸的反向判据、四个 validator 的纯逻辑；
//! - **`tests/db.rs`**（门 ⑥，`#[ignore]`，真库）：目录 CRUD、可见性折叠、
//!   render/run 的引用校验与那**唯一**一道 invoke 闸。
//!
//! 门 ⑦ 负责「本地 ↔ 上游路由表」的比对；这里负责**运行期**那一半：光看字面量，
//! 一个只注册了带尾斜杠形态的键会同时骗过本文件的一半与 ⑦ 的一半检查。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::actor_guard::HUMAN_ACTOR_REQUIRED_MESSAGE;

mod db;
mod support;

use support::*;

/// 6 条上游键的 `(method, 目录面/issue 侧, 带尾斜杠形态?)`。
///
/// 末列是**上游 `router.go` 的注册形态**逐字抄下来的：`2021` 的
/// `r.Route("/api/quick-actions")` + child `"/"` ⇒ fixture 路径**带**尾斜杠 ⇒
/// chi 的 Mount **两种**写法都服务；`1995/1996` 是 plain `r.Post` ⇒ **只有**一种。
pub(super) const SIX: [(&str, &str, bool); 6] = [
    ("GET", "/api/quick-actions/", true),
    ("POST", "/api/quick-actions/", true),
    ("PATCH", "/api/quick-actions/:id/", true),
    ("DELETE", "/api/quick-actions/:id/", true),
    (
        "POST",
        "/api/issues/:id/quick-actions/:quickActionId/render",
        false,
    ),
    (
        "POST",
        "/api/issues/:id/quick-actions/:quickActionId/run",
        false,
    ),
];

/// 把 `:param` 换成真 uuid，得到一条可发的路径。
fn concrete(template: &str) -> String {
    let id = Uuid::new_v4();
    template
        .replace(":id", &id.to_string())
        .replace(":quickActionId", &id.to_string())
}

// ---------------------------------------------------------------------------
// 1. 键集
// ---------------------------------------------------------------------------

/// 6 条逐条**已注册**（404 = 键不存在），且目录那 4 条的**两种写法都**注册。
///
/// 判据不是「返回 200」—— 懒库下到不了 200（会走到成员校验那一格）—— 而是
/// **既不是 404 也不是 405**：那两格分别代表「键没注册」与「键注册了但方法不对」。
#[tokio::test]
async fn the_six_upstream_keys_exist_and_the_four_catalogue_ones_serve_both_forms() {
    let app = lazy_app();
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    let id = Uuid::new_v4();

    for (method, template, both_forms) in SIX {
        // 上游字面量那一形态（带尾斜杠的 4 条 / 不带的 2 条）。
        let literal = concrete(template);
        let (status, _, _) = send(&app, &Call::new(method, &literal, user, ws)).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {literal} 必须已注册"
        );
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {literal}");

        if both_forms {
            // chi Mount 的第二种写法：axum 必须**也**注册，否则 404（不是 307）。
            let bare = literal
                .trim_end_matches('/')
                .replace("/:id", &id.to_string());
            let bare = if bare.is_empty() {
                "/api/quick-actions".to_string()
            } else {
                bare
            };
            let (status, _, _) = send(&app, &Call::new(method, &bare, user, ws)).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {bare}（另一种形态）"
            );
            assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {bare}");
        } else {
            // plain 注册 ⇒ 补一个尾斜杠就是**多**出来的形状。
            let slashed = format!("{literal}/");
            let (status, _, _) = send(&app, &Call::new(method, &slashed, user, ws)).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {slashed}：上游是 plain 注册，不得有别名形态"
            );
        }
    }
}

/// 反向：上游 6 条里**不存在**的那些形状必须 404。
#[tokio::test]
async fn no_ghost_shapes_are_registered() {
    let app = lazy_app();
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    for ghost in [
        "/api/quick-actions/x/render",           // 上游没有这一条
        "/api/quick-actions/x/run",              // 上游没有这一条
        "/api/issues/x/quick-actions/y",         // 上游没有这一条
        "/api/issues/x/quick-actions/y/render/", // plain 注册 + 别名
    ] {
        let (status, _, _) = send(&app, &Call::new("POST", ghost, user, ws)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "POST {ghost} 不得存在");
    }
    // 目录的 `{id}` 键**确实**在（非 uuid 的 id 走到解析那一格 ⇒ 400，不是 404）。
    let (status, _, _) = send(
        &app,
        &Call::new("DELETE", "/api/quick-actions/not-a-uuid", user, ws),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "/api/quick-actions/:id 已注册（404 会说明键不存在）"
    );
    // 而 `GET` 在那个键上不存在（上游那 4 条里没有它）⇒ 405，不是 404。
    let (status, _, _) = send(&app, &Call::new("GET", "/api/quick-actions/abc", user, ws)).await;
    assert_eq!(
        status,
        StatusCode::METHOD_NOT_ALLOWED,
        "GET /api/quick-actions/:id"
    );
}

// ---------------------------------------------------------------------------
// 2. 出库前的两层 + 「不挂」机器凭据闸
// ---------------------------------------------------------------------------

/// 无会话 ⇒ 401；workspace 四个来源都缺 ⇒ 400（6 条 × 2 层）。
#[tokio::test]
async fn the_pre_database_layers_hold_for_all_six_routes() {
    let app = lazy_app();
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    for (method, template, _) in SIX {
        let literal = concrete(template);
        let (status, _, body) = send(
            &app,
            &Call::new(method, &literal, user, ws).without_session(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {literal}");
        assert_eq!(error_of(&body)["code"], "unauthorized", "{literal}");

        let (status, _, body) = send(
            &app,
            &Call::new(method, &literal, user, ws).without_workspace(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {literal}");
        assert_eq!(error_of(&body)["code"], "validation_error", "{literal}");
    }
}

/// 🔴 本片**不挂** `RequireHumanActor`（上游 `router.go:1948` 那一组只
/// `RequireWorkspaceMember`）—— **反向**判据：机器凭据不得被本片拦。
///
/// 「不挂」不能只写在注释里：照抄 M9-1 / M9-2 的 `RequireHumanActor` 会把这条
/// 簇**收窄**到上游允许的面之外。判据是「错误消息里不得出现那一句话」**且**
/// 「状态码不得是 403」。
#[tokio::test]
async fn machine_credentials_are_not_gated_by_this_slice() {
    let app = lazy_app();
    let user = Uuid::new_v4();
    let ws = Uuid::new_v4();
    for (method, template, _) in SIX {
        let literal = concrete(template);
        for actor in ["task_token", "cloud_pat"] {
            let (status, _, body) =
                send(&app, &Call::new(method, &literal, user, ws).machine(actor)).await;
            let message = body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !message.contains(HUMAN_ACTOR_REQUIRED_MESSAGE),
                "{actor}: {method} {literal} 不得被本片拦"
            );
            assert_ne!(status, StatusCode::FORBIDDEN, "{actor}: {method} {literal}");
        }
    }
}

// ---------------------------------------------------------------------------
// 3. 四个 validator（纯逻辑，不碰库）
// ---------------------------------------------------------------------------

/// 名字：trim 后非空 + ≤ 32 **字符**（按 rune，不是字节 —— 上游 `utf8.RuneCountInString`）。
#[test]
fn name_validation_is_trim_bound_and_rune_counted() {
    assert_eq!(validate_name("  ship it  ").unwrap(), "ship it");
    assert!(validate_name("   ").is_err());
    assert!(
        validate_name(&"é".repeat(32)).is_ok(),
        "32 个双字节字符 = 64 字节仍合法"
    );
    assert!(validate_name(&"é".repeat(33)).is_err());
    assert_eq!(
        validate_name("").unwrap_err().to_string(),
        "validation error: name is required"
    );
}

/// prompt：非空 + ≤ 4000 字符 + **拒绝 `{{...}}`** + **拒绝会点到人的 mention**。
///
/// 后两条是本片最容易被「照抄成普通字符串校验」丢掉的行为，判据逐条给出上游逐字消息。
#[test]
fn prompt_validation_refuses_template_tokens_and_live_mentions() {
    assert_eq!(validate_prompt("  do the thing ").unwrap(), "do the thing");
    assert!(validate_prompt(" ").is_err());

    // `{{...}}` 被拒，消息里**带上那个 token 本身**（上游逐字）。
    let err = validate_prompt("look at {{issue.title}} please")
        .unwrap_err()
        .to_string();
    assert!(err.contains("{{issue.title}}"), "{err}");
    assert!(
        err.contains("template variables are not supported yet"),
        "{err}"
    );

    // 单个 `{{`、只有一个收尾 `}`、以及 `}}` 在 `{{` **之前**的写法都**不**算模板
    // （上游正则是 `\{\{[^}]*\}\}`：`[^}]*` 不跨 `}`，且收尾必须是**两个** `}`）。
    assert!(validate_prompt("an {{ orphan brace").is_ok());
    assert!(validate_prompt("a {{ half closed } token").is_ok());
    assert!(validate_prompt("a }} b {{ c").is_ok());

    // mention：agent / squad / member / all 被拒；**issue 链接**是唯一例外。
    for kind in ["agent", "squad", "member", "all"] {
        let prompt = format!("ping [@x](mention://{kind}/y) now");
        let err = validate_prompt(&prompt).unwrap_err().to_string();
        assert!(err.contains("cannot @mention"), "{kind}: {err}");
    }
    assert!(validate_prompt("see [rel](mention://issue/123)").is_ok());
}

/// 可见性：空 ⇒ `public`（上游的默认值）；其余只允许两个词。
#[test]
fn visibility_defaults_to_public_and_rejects_the_rest() {
    assert_eq!(normalize_visibility("").unwrap(), "public");
    assert_eq!(normalize_visibility("  private  ").unwrap(), "private");
    assert!(normalize_visibility("team").is_err());
    assert_eq!(
        normalize_visibility("team").unwrap_err().to_string(),
        "validation error: visibility must be \"public\" or \"private\""
    );
}

/// 绑定：类型只允许 `agent` / `squad`，id 非空。
#[test]
fn assignee_validation_is_closed_over_two_types() {
    assert!(validate_assignee("agent", "u").is_ok());
    assert!(validate_assignee("squad", "u").is_ok());
    assert!(validate_assignee("member", "u").is_err());
    assert!(validate_assignee("agent", "   ").is_err());
}

/// `?include_archived` 是**字面量**比较（上游 `== "true"`）：`TRUE` / `1` 都算假。
#[test]
fn include_archived_is_a_literal_comparison() {
    assert!(include_archived(Some(&"true".to_string())));
    assert!(!include_archived(Some(&"TRUE".to_string())));
    assert!(!include_archived(Some(&"1".to_string())));
    assert!(!include_archived(None));
}

/// 渲染出来的正文 = mention 行 + 空行 + **逐字** prompt（没有插值）。
#[test]
fn the_body_is_a_mention_line_plus_the_verbatim_prompt() {
    let target = QuickActionTarget {
        agent_id: Uuid::new_v4(),
        name: "Ada".to_string(),
        mention_type: "squad".to_string(),
        mention_id: Uuid::new_v4(),
        invocable_by_everyone: true,
    };
    let row = QuickActionRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        name: "ship".to_string(),
        description: String::new(),
        assignee_type: "squad".to_string(),
        assignee_id: target.mention_id,
        prompt: "please ship  the  thing".to_string(),
        visibility: "public".to_string(),
        status: "active".to_string(),
        last_used_at: None,
        use_count: 0,
        created_by_type: "member".to_string(),
        created_by_id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let body = build_body(&row, &target);
    assert_eq!(
        body,
        format!(
            "[@Ada](mention://squad/{})\n\nplease ship  the  thing",
            target.mention_id
        )
    );
    // 响应体里 `target_missing` / `target_public` **恒**出现，`target_name` 带 omitempty。
    let with = serde_json::to_value(ActionResponse::new(&row, Some(&target))).unwrap();
    assert_eq!(with["target_name"], json!("Ada"));
    assert_eq!(with["target_public"], json!(true));
    assert_eq!(with["target_missing"], json!(false));
    let without = serde_json::to_value(ActionResponse::new(&row, None)).unwrap();
    assert!(without.get("target_name").is_none(), "{without}");
    assert_eq!(without["target_public"], json!(false));
    assert_eq!(without["target_missing"], json!(true));
}
