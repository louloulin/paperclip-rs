//! [`super::super`] 的一条**完整摄入回路**：脚本化的假 Lark + 假存储 + 假账本。
//!
//! 断言链是"脚本造响应 → 真解析器 → 真 key/文件名/类型 → 真上传"，中间零 mock；
//! 并且钉住上游那条**顺序**契约：意图行先落、上传在后。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, MessageKind};
use mc_core::id::Id;

use crate::lark::client::{ApiClient, ApiError, StubApiClient};
use crate::lark::feishu_channel::{installation_credentials_for, Decrypter, LarkInboundMessage};
use crate::lark::http_client::resource::{DownloadedResourceStream, ResourceBody};
use crate::lark::params::DownloadResourceParams;

use super::super::{LarkMediaResolver, MediaStorage};
use super::fixtures::*;
use crate::engine::resolvers::{
    EngineResult, MediaIntentLedger, MediaResolver, RecordPendingMediaObjectParams,
    ResolvedInstallation,
};

// =====================================================================
// 四、一条完整摄入回路（意图行**先于**上传）
// =====================================================================

/// 脚本化的假 Lark：`download_message_resource_stream` 回一段字节。
struct ScriptedApi {
    bytes: Vec<u8>,
    content_type: String,
    filename: Option<String>,
    error: Option<ApiError>,
    calls: Mutex<Vec<DownloadResourceParams>>,
}

#[async_trait]
impl ApiClient for ScriptedApi {
    fn is_configured(&self) -> bool {
        true
    }

    async fn download_message_resource_stream(
        &self,
        _credentials: crate::lark::params::InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError> {
        self.calls.lock().expect("lock").push(params.clone());
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        Ok(DownloadedResourceStream {
            body: ResourceBody::buffered(self.bytes.clone()),
            content_type: self.content_type.clone(),
            filename: self.filename.clone(),
            size_bytes: i64::try_from(self.bytes.len()).unwrap_or(0),
        })
    }

    // 其余面转发给替身（本片的用例不碰它们）。
    async fn send_interactive_card(
        &self,
        params: crate::lark::params::SendCardParams,
    ) -> Result<String, ApiError> {
        StubApiClient::new().send_interactive_card(params).await
    }
    async fn patch_interactive_card(
        &self,
        params: crate::lark::params::PatchCardParams,
    ) -> Result<(), ApiError> {
        StubApiClient::new().patch_interactive_card(params).await
    }
    async fn send_text_message(
        &self,
        params: crate::lark::params::SendTextParams,
    ) -> Result<String, ApiError> {
        StubApiClient::new().send_text_message(params).await
    }
    async fn send_markdown_card(
        &self,
        params: crate::lark::params::SendMarkdownCardParams,
    ) -> Result<String, ApiError> {
        StubApiClient::new().send_markdown_card(params).await
    }
    async fn send_binding_prompt_card(
        &self,
        params: crate::lark::params::BindingPromptParams,
    ) -> Result<(), ApiError> {
        StubApiClient::new().send_binding_prompt_card(params).await
    }
    async fn get_bot_info(
        &self,
        credentials: crate::lark::params::InstallationCredentials,
    ) -> Result<crate::lark::types::BotInfo, ApiError> {
        StubApiClient::new().get_bot_info(credentials).await
    }
    async fn get_message(
        &self,
        credentials: crate::lark::params::InstallationCredentials,
        message_id: &str,
    ) -> Result<Vec<crate::lark::types::LarkMessage>, ApiError> {
        StubApiClient::new()
            .get_message(credentials, message_id)
            .await
    }
    async fn list_chat_messages(
        &self,
        credentials: crate::lark::params::InstallationCredentials,
        params: crate::lark::params::ListMessagesParams,
    ) -> Result<Vec<crate::lark::types::LarkMessage>, ApiError> {
        StubApiClient::new()
            .list_chat_messages(credentials, params)
            .await
    }
    async fn download_message_resource(
        &self,
        credentials: crate::lark::params::InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<crate::lark::http_client::resource::DownloadedResource, ApiError> {
        StubApiClient::new()
            .download_message_resource(credentials, params)
            .await
    }
    async fn batch_get_users(
        &self,
        credentials: crate::lark::params::InstallationCredentials,
        open_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, String>, ApiError> {
        StubApiClient::new()
            .batch_get_users(credentials, open_ids)
            .await
    }
    async fn add_message_reaction(
        &self,
        params: crate::lark::params::AddReactionParams,
    ) -> Result<String, ApiError> {
        StubApiClient::new().add_message_reaction(params).await
    }
    async fn delete_message_reaction(
        &self,
        params: crate::lark::params::DeleteReactionParams,
    ) -> Result<(), ApiError> {
        StubApiClient::new().delete_message_reaction(params).await
    }
}

/// 记账的假存储。
#[derive(Default)]
struct RecordingStorage {
    uploads: Mutex<Vec<(String, usize, String, String)>>,
}

impl MediaStorage for RecordingStorage {
    fn upload(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
        filename: &str,
    ) -> Result<String, String> {
        self.uploads.lock().expect("lock").push((
            key.to_string(),
            data.len(),
            content_type.to_string(),
            filename.to_string(),
        ));
        Ok(format!("https://objects.test/{key}"))
    }

    fn object_url(&self, key: &str) -> String {
        format!("https://objects.test/{key}")
    }
}

/// 记账的假意图账本。
#[derive(Default)]
struct RecordingLedger {
    pending: Mutex<Vec<RecordPendingMediaObjectParams>>,
    /// `false` = 这个 key 已经归对账器（拒绝上传）。
    owned: bool,
}

#[async_trait]
impl MediaIntentLedger for RecordingLedger {
    async fn record_pending_media_object(
        &self,
        params: RecordPendingMediaObjectParams,
    ) -> EngineResult<bool> {
        self.pending.lock().expect("lock").push(params);
        Ok(!self.owned)
    }
}

/// 什么都不做的存储（`has_media` 用例用）。
pub(super) struct NoopStorage;

impl MediaStorage for NoopStorage {
    fn upload(&self, _key: &str, _data: &[u8], _c: &str, _f: &str) -> Result<String, String> {
        Err("not used".to_string())
    }
    fn object_url(&self, key: &str) -> String {
        key.to_string()
    }
}

/// 什么都不记的账本（`has_media` 用例用）。
pub(super) struct NoopLedger;

#[async_trait]
impl MediaIntentLedger for NoopLedger {
    async fn record_pending_media_object(
        &self,
        _params: RecordPendingMediaObjectParams,
    ) -> EngineResult<bool> {
        Ok(true)
    }
}

fn ingest(
    api: Arc<ScriptedApi>,
    storage: Arc<RecordingStorage>,
    ledger: Arc<RecordingLedger>,
    payload: &LarkInboundMessage,
) -> InboundMessage {
    let resolver = LarkMediaResolver::new(api, Decrypter::login_plaintext(), storage, ledger);
    let installed = resolved(installation(None));
    resolver.resolve_media(
        &installed,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &envelope(payload),
    )
}

/// 正路：意图行**先**落、上传在后，回填一个 `MediaRef`（字段逐个）。
#[test]
fn ingest_records_the_intent_before_the_upload_and_returns_a_ref() {
    let api = Arc::new(ScriptedApi {
        bytes: vec![1, 2, 3, 4],
        content_type: "image/png".to_string(),
        filename: Some("photo.png".to_string()),
        error: None,
        calls: Mutex::new(Vec::new()),
    });
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::default());
    let payload = payload("image", r#"{"image_key":"img_x"}"#);

    let resolved_message = ingest(
        Arc::clone(&api),
        Arc::clone(&storage),
        Arc::clone(&ledger),
        &payload,
    );

    assert_eq!(resolved_message.media_refs.len(), 1);
    let reference = &resolved_message.media_refs[0];
    assert_eq!(reference.message_kind, MessageKind::Image);
    assert_eq!(reference.filename, "photo.png");
    assert_eq!(reference.mime_type, "image/png");
    assert_eq!(reference.size_bytes, 4);
    assert!(reference.storage_key.starts_with("workspaces/"));
    assert_eq!(
        reference.storage_url,
        format!("https://objects.test/{}", reference.storage_key)
    );
    // 平台资源类别与键都按 payload 传给了传输层。
    let calls = api.calls.lock().expect("lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message_id, "om-1");
    assert_eq!(calls[0].file_key, "img_x");
    assert_eq!(calls[0].resource_type, "image");
    // 账本行与上传用的**同一个** key。
    let pending = ledger.pending.lock().expect("lock");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].storage_key, reference.storage_key);
    let uploads = storage.uploads.lock().expect("lock");
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].0, reference.storage_key);
    assert_eq!(uploads[0].1, 4);
}

/// 下载失败 ⇒ **不**上传、不留 ref，但意图行照旧留着（对账器事后收尾）。
#[test]
fn download_failure_leaves_the_intent_row_and_no_reference() {
    let api = Arc::new(ScriptedApi {
        bytes: Vec::new(),
        content_type: String::new(),
        filename: None,
        error: Some(ApiError::Http {
            op: "download_message_resource_stream",
            status: 500,
        }),
        calls: Mutex::new(Vec::new()),
    });
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::default());

    let resolved_message = ingest(
        Arc::clone(&api),
        Arc::clone(&storage),
        Arc::clone(&ledger),
        &payload("image", r#"{"image_key":"img_x"}"#),
    );

    assert!(resolved_message.media_refs.is_empty());
    assert_eq!(storage.uploads.lock().expect("lock").len(), 0, "不该上传");
    assert_eq!(
        ledger.pending.lock().expect("lock").len(),
        1,
        "意图行必须留下（上游：本函数不删任何东西）"
    );
}

/// key 已归对账器（`Ok(false)`）⇒ 连下载都不做（**不**复活那一行）。
#[test]
fn reconciler_owned_key_skips_the_whole_ingest() {
    let api = Arc::new(ScriptedApi {
        bytes: vec![1],
        content_type: "image/png".to_string(),
        filename: None,
        error: None,
        calls: Mutex::new(Vec::new()),
    });
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger {
        owned: true,
        ..RecordingLedger::default()
    });

    let resolved_message = ingest(
        Arc::clone(&api),
        Arc::clone(&storage),
        Arc::clone(&ledger),
        &payload("image", r#"{"image_key":"img_x"}"#),
    );

    assert!(resolved_message.media_refs.is_empty());
    assert_eq!(api.calls.lock().expect("lock").len(), 0, "不该下载");
    assert_eq!(storage.uploads.lock().expect("lock").len(), 0);
}

/// 没有 `chat_message` 行 ⇒ 跳过（**不猜** key，也**不**发请求）。
#[test]
fn missing_chat_message_row_skips_ingest() {
    let api = Arc::new(ScriptedApi {
        bytes: vec![1],
        content_type: "image/png".to_string(),
        filename: None,
        error: None,
        calls: Mutex::new(Vec::new()),
    });
    let resolver = LarkMediaResolver::new(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::login_plaintext(),
        Arc::new(RecordingStorage::default()),
        Arc::new(RecordingLedger::default()),
    );
    let installed = resolved(installation(None));
    let resolved_message = resolver.resolve_media(
        &installed,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        None,
        &envelope(&payload("image", r#"{"image_key":"img_x"}"#)),
    );
    assert!(resolved_message.media_refs.is_empty());
    assert_eq!(api.calls.lock().expect("lock").len(), 0);
}

/// 多资源：一个失败不牵连其余（上游注释逐字）。
#[test]
fn one_failed_resource_does_not_affect_the_others() {
    let api = Arc::new(ScriptedApi {
        bytes: vec![1, 2],
        content_type: "image/png".to_string(),
        filename: None,
        error: None,
        calls: Mutex::new(Vec::new()),
    });
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::default());
    let payload = payload(
        "post",
        r#"{"content":[[{"tag":"img","image_key":"img_a"},{"tag":"img","image_key":"img_b"}]]}"#,
    );

    let resolved_message = ingest(
        Arc::clone(&api),
        Arc::clone(&storage),
        Arc::clone(&ledger),
        &payload,
    );

    assert_eq!(resolved_message.media_refs.len(), 2, "两条都该成功");
    assert_eq!(api.calls.lock().expect("lock").len(), 2);
}

/// 安装投影不可用（不是本 adapter 的 `ResolvedInstallation`）⇒ 跳过（不猜凭据）。
#[test]
fn missing_installation_payload_skips_ingest() {
    let api = Arc::new(ScriptedApi {
        bytes: vec![1],
        content_type: "image/png".to_string(),
        filename: None,
        error: None,
        calls: Mutex::new(Vec::new()),
    });
    let resolver = LarkMediaResolver::new(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::login_plaintext(),
        Arc::new(RecordingStorage::default()),
        Arc::new(RecordingLedger::default()),
    );
    let bare = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        crate::lark::resolvers::TYPE_LARK,
        true,
    );
    let resolved_message = resolver.resolve_media(
        &bare,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &envelope(&payload("image", r#"{"image_key":"img_x"}"#)),
    );
    assert!(resolved_message.media_refs.is_empty());
    assert_eq!(api.calls.lock().expect("lock").len(), 0);
}

/// 解密失败 ⇒ 跳过（**不**回显任何密文/明文）。
#[test]
fn credential_failure_skips_ingest_without_echoing_anything() {
    let api = Arc::new(ScriptedApi {
        bytes: vec![1],
        content_type: "image/png".to_string(),
        filename: None,
        error: None,
        calls: Mutex::new(Vec::new()),
    });
    let resolver = LarkMediaResolver::new(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::fail_closed(),
        Arc::new(RecordingStorage::default()),
        Arc::new(RecordingLedger::default()),
    );
    let installed = resolved(installation(None));
    let resolved_message = resolver.resolve_media(
        &installed,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &envelope(&payload("image", r#"{"image_key":"img_x"}"#)),
    );
    assert!(resolved_message.media_refs.is_empty());
    assert_eq!(api.calls.lock().expect("lock").len(), 0);
    // 解密器的判决与 region 透传走的是同一条 `installation_credentials_for`。
    assert!(installation_credentials_for(&installation(None), &Decrypter::fail_closed()).is_err());
}
