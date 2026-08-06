//! OpenClaw 集成：invite-prompt 文本生成。
//!
//! 语义：根据 company + 用户上下文，生成一段邀请该用户接入 OpenClaw 集成的
//! 提示词（邮件主题 / 正文 / 系统 prompt）。本端点为确定性模板渲染，无 DB
//! 写入，仅记录一次实时事件供下游观测。

use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiResult, AppState};
use pc_realtime::LiveEvent;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/openclaw/invite-prompt",
        post(invite_prompt),
    )
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct InvitePromptBody {
    #[serde(default)]
    user_email: Option<String>,
    #[serde(default)]
    user_name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    locale: Option<String>,
}

fn render_invite_prompt(company_id: Uuid, body: &InvitePromptBody) -> Value {
    let email = body
        .user_email
        .clone()
        .unwrap_or_else(|| "<user-email>".into());
    let name = body.user_name.clone().unwrap_or_else(|| "there".into());
    let role = body.role.clone().unwrap_or_else(|| "collaborator".into());
    let locale = body.locale.clone().unwrap_or_else(|| "en-US".into());
    let subject = format!(
        "You're invited to OpenClaw for company {}",
        company_id.simple()
    );
    let body_text = format!(
        "Hi {name},\n\n\
         You ({email}, role: {role}) have been invited to connect OpenClaw with this workspace.\n\
         OpenClaw keeps your issue graph, secrets, and routines in sync across environments.\n\n\
         Locale: {locale}\n\
         Company: {}\n\n\
         Reply to this thread to confirm, or ignore this message to opt out.",
        company_id.simple()
    );
    let system_prompt = format!(
        "You are the OpenClaw onboarding assistant for company {} (locale: {locale}).\n\
         Help the invited {role} ({email}) set up their workspace, link their repositories,\n\
         and verify issue-graph liveness. Be concise and action-oriented.",
        company_id.simple()
    );
    json!({
        "companyId": company_id,
        "locale": locale,
        "channel": "email",
        "subject": subject,
        "body": body_text,
        "systemPrompt": system_prompt,
        "variables": {
            "email": email,
            "name": name,
            "role": role,
        }
    })
}

async fn invite_prompt(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<InvitePromptBody>,
) -> ApiResult<Json<Value>> {
    let payload = render_invite_prompt(company_id, &body);
    state.realtime.publish(
        LiveEvent::new("openclaw.invite_prompt_generated", "openclaw", company_id)
            .with_company(company_id)
            .with_data(json!({
                "locale": payload.get("locale"),
            })),
    );
    Ok(Json(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_subject_body_and_system_prompt() {
        let cid = Uuid::nil();
        let body = InvitePromptBody {
            user_email: Some("alice@example.com".into()),
            user_name: Some("Alice".into()),
            role: Some("admin".into()),
            locale: Some("zh-CN".into()),
        };
        let v = render_invite_prompt(cid, &body);
        let subject = v.get("subject").and_then(|x| x.as_str()).expect("subj");
        let body_str = v.get("body").and_then(|x| x.as_str()).expect("body");
        let sys = v.get("systemPrompt").and_then(|x| x.as_str()).expect("sys");
        assert!(subject.contains("OpenClaw"));
        assert!(body_str.contains("Alice"));
        assert!(body_str.contains("alice@example.com"));
        assert!(body_str.contains("admin"));
        assert!(body_str.contains("zh-CN"));
        assert!(sys.contains("zh-CN"));
        let locale = v.get("locale").and_then(|x| x.as_str()).expect("loc");
        assert_eq!(locale, "zh-CN");
    }

    #[test]
    fn fills_defaults_when_fields_missing() {
        let cid = Uuid::nil();
        let body = InvitePromptBody::default();
        let v = render_invite_prompt(cid, &body);
        let body_str = v.get("body").and_then(|x| x.as_str()).expect("body");
        // Defaults: <user-email> / there / collaborator / en-US
        assert!(body_str.contains("there"));
        assert!(body_str.contains("collaborator"));
        assert!(body_str.contains("<user-email>"));
        assert!(body_str.contains("en-US"));
    }

    #[test]
    fn variable_block_mirrors_inputs() {
        let cid = Uuid::nil();
        let body = InvitePromptBody {
            user_email: Some("bob@x".into()),
            user_name: Some("Bob".into()),
            role: Some("viewer".into()),
            locale: Some("en-US".into()),
        };
        let v = render_invite_prompt(cid, &body);
        let vars = v.get("variables").expect("vars");
        assert_eq!(vars["email"], "bob@x");
        assert_eq!(vars["name"], "Bob");
        assert_eq!(vars["role"], "viewer");
    }
}
