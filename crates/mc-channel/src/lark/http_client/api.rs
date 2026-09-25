//! lark **端点面**：`ApiClient` 的 12 个非资源端点 + 安装期的 Bot 身份查询
//! （上游 `internal/integrations/lark/http_client.go` 的端点实现那一半）。
//!
//! - **写者**：M7-10（`docs/60-M7-PLAN.md` §3.3；本片是 stage 5 的第一片，**0 路由**）。
//! - **为什么是独立文件**：`http_client.rs` 单文件首版 **978 行** > 门 ⑩ 的 800 硬限 ⇒ 按
//!   「传输核心（父模块）/ 端点面（本文件）/ 资源响应面（[`super::resource`]）」切开。
//!   切分是**回归上游结构**（上游 `http_client.go` 的端点族与它的下载辅助函数本来就是两簇），
//!   不是为凑门而拆。
//! - **本文件不含**：令牌缓存、HTTP 编解码、错误分类 —— 那些在父模块
//!   （[`super::HttpApiClient::tenant_access_token`] / [`super::HttpApiClient::do_json`]）与
//!   [`crate::lark::client`]。本文件只负责"每个端点长什么样"。

use async_trait::async_trait;
use serde_json::{json, Value};

use super::resource::{DownloadedResource, DownloadedResourceStream, MAX_MESSAGE_RESOURCE_BYTES};
use super::HttpApiClient;
use crate::lark::client::{ApiClient, ApiError, ErrorClass};
use crate::lark::params::{
    batch_get_users_query, binding_prompt_card, contact_user_path, escape_path_segment,
    list_messages_request, message_path, outbound_message_request, reaction_path, reactions_path,
    resource_path, AddReactionParams, BindingPromptParams, DeleteReactionParams,
    DownloadResourceParams, InstallationCredentials, ListMessagesParams, PatchCardParams,
    SendCardParams, SendMarkdownCardParams, SendTextParams, BOT_INFO_PATH,
    CONTACT_USERS_BATCH_PATH, MESSAGES_PATH,
};
use crate::lark::types::{
    BotInfo, BotInfoEnvelope, LarkMessage, MessageIdEnvelope, MessageItemsEnvelope,
    ReactionIdEnvelope, UnionIdEnvelope, UserBatchEnvelope,
};

#[async_trait]
impl ApiClient for HttpApiClient {
    /// 一旦这个客户端被构造出来，出站传输面就是接好的 ⇒ 恒 `true`
    /// （替身是它的反面：那里每个调用都回 [`ApiError::NotConfigured`]）。
    fn is_configured(&self) -> bool {
        true
    }

    async fn send_interactive_card(&self, params: SendCardParams) -> Result<String, ApiError> {
        const OP: &str = "send interactive card";
        if params.chat_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing chat_id",
            });
        }
        if params.card_json.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing card json",
            });
        }
        let (path, body) = outbound_message_request(
            &params.chat_id,
            "interactive",
            &params.card_json,
            &params.reply_target,
        );
        let envelope: MessageIdEnvelope = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::POST,
                &path,
                Some(&body),
                OP,
            )
            .await?;
        envelope.message_id(OP)
    }

    async fn patch_interactive_card(&self, params: PatchCardParams) -> Result<(), ApiError> {
        const OP: &str = "patch interactive card";
        if params.card_message_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing card message id",
            });
        }
        if params.card_json.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing card json",
            });
        }
        // Lark 的 patch 端点**整卡替换**：调用方每次渲染完整的卡片（上游同款）。
        let body = json!({ "content": params.card_json });
        let path = message_path(&params.card_message_id);
        let _: Value = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::PATCH,
                &path,
                Some(&body),
                OP,
            )
            .await?;
        Ok(())
    }

    async fn send_text_message(&self, params: SendTextParams) -> Result<String, ApiError> {
        const OP: &str = "send text message";
        if params.chat_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing chat_id",
            });
        }
        if params.text.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing text",
            });
        }
        // Lark 的 `text` 消息要 `content` 是**再编码一层**的 `{"text": "…"}` —— 由
        // `serde_json` 负责换行 / 引号 / 非 ASCII 的转义，agent 的回复因此原样往返。
        let content = json!({ "text": params.text }).to_string();
        let (path, body) =
            outbound_message_request(&params.chat_id, "text", &content, &params.reply_target);
        let envelope: MessageIdEnvelope = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::POST,
                &path,
                Some(&body),
                OP,
            )
            .await?;
        envelope.message_id(OP)
    }

    async fn send_markdown_card(&self, params: SendMarkdownCardParams) -> Result<String, ApiError> {
        const OP: &str = "send markdown card";
        if params.chat_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing chat_id",
            });
        }
        if params.markdown.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing markdown body",
            });
        }
        let mut card = json!({
            "schema": "2.0",
            "body": {
                "elements": [{ "tag": "markdown", "content": params.markdown }],
            },
        });
        if !params.summary.is_empty() {
            card["config"] = json!({ "summary": { "content": params.summary } });
        }
        let content = card.to_string();
        let (path, body) = outbound_message_request(
            &params.chat_id,
            "interactive",
            &content,
            &params.reply_target,
        );
        let envelope: MessageIdEnvelope = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::POST,
                &path,
                Some(&body),
                OP,
            )
            .await?;
        envelope.message_id(OP)
    }

    async fn send_binding_prompt_card(&self, params: BindingPromptParams) -> Result<(), ApiError> {
        const OP: &str = "send binding prompt";
        if params.open_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing open_id",
            });
        }
        if params.bind_url.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing bind url",
            });
        }
        // 直接发给**这个人的 `open_id`**（不是发到会话），所以 `receive_id_type=open_id`。
        let body = json!({
            "receive_id": params.open_id.as_str(),
            "msg_type": "interactive",
            "content": binding_prompt_card(&params.bind_url),
        });
        let path = format!("{MESSAGES_PATH}?receive_id_type=open_id");
        let _: Value = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::POST,
                &path,
                Some(&body),
                OP,
            )
            .await?;
        Ok(())
    }

    async fn get_bot_info(
        &self,
        credentials: InstallationCredentials,
    ) -> Result<BotInfo, ApiError> {
        const OP: &str = "bot info";
        if !credentials.is_complete() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing app credentials for GetBotInfo",
            });
        }
        let envelope: BotInfoEnvelope = self
            .do_authed_json(&credentials, reqwest::Method::GET, BOT_INFO_PATH, None, OP)
            .await?;
        let open_id = envelope.open_id(OP)?;

        // `union_id` 要再打一次通讯录端点（`/bot/v3/info` 的公开 schema 里没有它）。
        // **软失败**：拿不到就记一行警告、照旧返回（安装仍可用于 p2p）。
        let union_id = match self
            .fetch_bot_union_id(&credentials, open_id.as_str())
            .await
        {
            Ok(union_id) => union_id,
            Err(error) => {
                tracing::warn!(
                    app_id = %credentials.app_id,
                    err = %error,
                    "lark http client: bot union_id lookup failed; continuing without it"
                );
                String::new()
            }
        };
        Ok(BotInfo { open_id, union_id })
    }

    async fn get_message(
        &self,
        credentials: InstallationCredentials,
        message_id: &str,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        const OP: &str = "get message";
        if message_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing message_id",
            });
        }
        // `user_id_type=open_id` ⇒ `sender.id` 与 `mentions[].id` 回来的都是 `open_id`，
        // 与包里其余部分键的那批标识一致。
        let path = format!("{}?user_id_type=open_id", message_path(message_id));
        let envelope: MessageItemsEnvelope = self
            .do_authed_json(&credentials, reqwest::Method::GET, &path, None, OP)
            .await?;
        Ok(envelope.messages())
    }

    async fn list_chat_messages(
        &self,
        credentials: InstallationCredentials,
        params: ListMessagesParams,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        const OP: &str = "list chat messages";
        if params.chat_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing chat_id",
            });
        }
        let path = list_messages_request(&params);
        let envelope: MessageItemsEnvelope = self
            .do_authed_json(&credentials, reqwest::Method::GET, &path, None, OP)
            .await?;
        Ok(envelope.messages())
    }

    async fn download_message_resource(
        &self,
        credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResource, ApiError> {
        const OP: &str = "download message resource";
        let stream = self
            .download_message_resource_stream(credentials, params)
            .await?;
        let content_type = stream.content_type;
        let filename = stream.filename;
        let declared = stream.size_bytes;
        let mut body = stream.body;
        let data = body.read_all_capped(OP, MAX_MESSAGE_RESOURCE_BYTES).await?;
        let size_bytes = if declared == 0 {
            i64::try_from(data.len()).unwrap_or(i64::MAX)
        } else {
            declared
        };
        Ok(DownloadedResource {
            data,
            content_type,
            filename,
            size_bytes,
        })
    }

    async fn download_message_resource_stream(
        &self,
        credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError> {
        const OP: &str = "download message resource";
        if params.message_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing message_id",
            });
        }
        if params.file_key.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing file_key",
            });
        }
        let mut path = resource_path(&params.message_id, &params.file_key);
        if !params.resource_type.is_empty() {
            path.push_str("?type=");
            path.push_str(&escape_path_segment(&params.resource_type));
        }
        let base = self.resolve_base_url(&credentials).to_string();

        // 与 `do_authed_json` 同一份契约：Lark 在**服务任何字节之前**就拒掉坏令牌，
        // 所以重放这一次 GET 不可能产出半截下载。
        let token = self.tenant_access_token(&credentials).await?;
        match self.download_once(&base, &path, &token, OP).await {
            Err(error) if error.class() == ErrorClass::InvalidCredential => {
                tracing::warn!(
                    app_id = %credentials.app_id,
                    "lark http client: tenant_access_token rejected on resource download; \
                     refreshing and retrying once"
                );
                self.invalidate_token(&credentials.app_id);
                let fresh = self.tenant_access_token(&credentials).await?;
                self.download_once(&base, &path, &fresh, OP).await
            }
            other => other,
        }
    }

    async fn batch_get_users(
        &self,
        credentials: InstallationCredentials,
        open_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, String>, ApiError> {
        const OP: &str = "batch get users";
        if open_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let path = format!(
            "{CONTACT_USERS_BATCH_PATH}{}",
            batch_get_users_query(&open_ids)
        );
        let envelope: UserBatchEnvelope = self
            .do_authed_json(&credentials, reqwest::Method::GET, &path, None, OP)
            .await?;
        Ok(envelope.names())
    }

    async fn add_message_reaction(&self, params: AddReactionParams) -> Result<String, ApiError> {
        const OP: &str = "add message reaction";
        if params.message_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing message_id",
            });
        }
        if params.emoji_type.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing emoji_type",
            });
        }
        let body = json!({ "reaction_type": { "emoji_type": params.emoji_type } });
        let path = reactions_path(&params.message_id);
        let envelope: ReactionIdEnvelope = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::POST,
                &path,
                Some(&body),
                OP,
            )
            .await?;
        envelope.reaction_id(OP)
    }

    async fn delete_message_reaction(&self, params: DeleteReactionParams) -> Result<(), ApiError> {
        const OP: &str = "delete message reaction";
        if params.message_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing message_id",
            });
        }
        if params.reaction_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing reaction_id",
            });
        }
        let path = reaction_path(&params.message_id, &params.reaction_id);
        let _: Value = self
            .do_authed_json(
                &params.credentials,
                reqwest::Method::DELETE,
                &path,
                None,
                OP,
            )
            .await?;
        Ok(())
    }
}

impl HttpApiClient {
    /// 通讯录单用户查询，只为补 Bot 的 `union_id`（上游 `fetchBotUnionID`）。
    ///
    /// **空串 + `Ok` 是合法结果**：范围受限的通讯录端点会回 `code = 0` 而**不带** `union_id`。
    /// 调用方记一行警告并继续（单 Bot 部署里 `open_id` 匹配仍然无歧义）。
    ///
    /// # Errors
    ///
    /// `open_id` 为空；链路失败；平台拒绝。
    async fn fetch_bot_union_id(
        &self,
        credentials: &InstallationCredentials,
        open_id: &str,
    ) -> Result<String, ApiError> {
        const OP: &str = "contact users";
        if open_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "empty open_id",
            });
        }
        let path = format!("{}?user_id_type=open_id", contact_user_path(open_id));
        let envelope: UnionIdEnvelope = self
            .do_authed_json(credentials, reqwest::Method::GET, &path, None, OP)
            .await?;
        Ok(envelope.union_id())
    }
}

#[cfg(test)]
mod tests;
