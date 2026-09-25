//! `media_upload.go`（420 行）的本地落点：**把一个文件送进 `WeCom` 聊天**。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §35 的 D1）。
//! - **上游定位**（文件头逐字）：bot **没有** REST 端点做这件事：媒体走的就是别的一切走的那条
//!   WebSocket，三个 `cmd`、没有 `access_token`
//!   （`https://developer.work.weixin.qq.com/document/path/101463`）：
//!
//!   | cmd | 做什么 |
//!   | --- | --- |
//!   | `aibot_upload_media_init` | 声明这个文件，交回一个 `upload_id` |
//!   | `aibot_upload_media_chunk` | 一个字节切片，base64 之后 × N |
//!   | `aibot_upload_media_finish` | 交回一条消息可以携带的 `media_id` |
//!
//!   三步都**不只是**"已受理"，所以三步都走 `request` 并读回来的判决。
//!
//!   中间那一步的两个性质就是这里的全部设计：块可以**任意顺序、并发**发出，而重发一块是幂等的。
//!   所以块是几块几块地发而不是齐步走，而一块的 ack 永远不回来时**就再发一次** ——
//!   丢掉一百块里的一块不该让整个文件失败。
//!
//!   它**不**做的是把一张图片塞进一条回复里：长连接没有 `msg_item`，所以文件永远是它自己的一条
//!   消息；一条带附件的回答就是那条回答、然后是那个文件。
//!
//! # 与上游的两点形态差异（登记 `docs/32` §35 的 D9）
//!
//! 1. **`errgroup.SetLimit(n)` → `buffer_unordered(n)`**：并发度与阶梯逐字相同，而"第一个失败
//!    就取消其余的"落成"第一个错误结束这个流、剩下的 future 被丢掉"。上游的 `errgroup` 会
//!    **主动取消**已在飞的那几块；本仓的版本让它们跑完才被丢弃 —— 差别只在**收尾的时机**，
//!    因为一次上传失败之后那些块的结果没有任何人读。
//! 2. **`sendMsgFrame` 是本仓的 `request`**（**M7-20** 的 `rate_limit.rs` 才有配额与一次重试）
//!    ⇒ 本文件按 M7-16 对 `send_text` 的同一条判例（`docs/32` §33 的 D4）**直接走 `request`**，
//!    而**每个聊一把锁**照旧由本文件持有（上游明确把它放在这里、而不是放在配额那一层）。
//!
//! # 凭据面
//!
//! 本文件不碰任何凭据：`upload_id` / `media_id` 是服务端发下来的会话标识，`base64_data` 是
//! 文件字节。因此这里**没有**手写 `Debug` 的类型 —— 但日志里也不插值文件内容（只插
//! `upload_id` / `chunk_index` / `msgtype`）。

use futures_util::stream::{self, StreamExt as _};
use serde_json::{json, Value};

use super::ws_frame::{CHAT_TYPE_GROUP_INT, CHAT_TYPE_SINGLE_INT, CMD_SEND_MSG};
use super::ws_sender::{Deadline, SenderError, WsSender};

// =====================================================================
// 常量
// =====================================================================

/// 三步上传的 cmd（上游三个 `cmdUploadMedia*`）。
pub const CMD_UPLOAD_MEDIA_INIT: &str = "aibot_upload_media_init";
pub const CMD_UPLOAD_MEDIA_CHUNK: &str = "aibot_upload_media_chunk";
pub const CMD_UPLOAD_MEDIA_FINISH: &str = "aibot_upload_media_finish";

/// 这个文件成为一条消息之后，`WeCom` 会管它叫什么。四个取值是协议自己的，别的都不收。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaMsgType {
    File,
    Image,
    Voice,
    Video,
}

impl MediaMsgType {
    /// 协议里的字面量（`msgtype` 字段的值）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
            Self::Voice => "voice",
            Self::Video => "video",
        }
    }

    /// 四个取值（用例与穷举断言用）。
    pub const ALL: [Self; 4] = [Self::File, Self::Image, Self::Voice, Self::Video];
}

/// 一块的大小上限，**base64 之前** —— 所以 wire 上的一帧还要再大约三分之一。
///
/// 与 [`MAX_MEDIA_CHUNKS`] 一起是这条**传输**能表达的最大文件：50 MB。两个数都是 `WeCom` 自己
/// SDK 的数（`WecomTeam/aibot-node-sdk` 的 `src/client.ts`、`uploadMedia`：`CHUNK_SIZE = 512 * 1024`，
/// 而 `totalChunks > 100` 抛 "max ~50MB"）。
pub const MEDIA_CHUNK_BYTES: usize = 512 * 1024;

/// 协议自己给块数的上限。
pub const MAX_MEDIA_CHUNKS: usize = 100;

/// 这条**传输**能表达的最大字节数（= 512 KiB × 100 = 50 MB）。
pub const MAX_MEDIA_TRANSPORT_BYTES: usize = MEDIA_CHUNK_BYTES * MAX_MEDIA_CHUNKS;

/// 我们**真的**会发的最大文件 —— 它是那条传输能表达的三分之一。
///
/// 块算术允许的 50 MB 是**组帧**能描述的大小，不是平台接受的大小。`WeCom` 把接受的尺寸写在
/// `init` 这个 cmd 自己身上（`document/path/101463`，`aibot_upload_media_init` 的
/// `body.total_size` 一行）：图片最多 10MB、语音 2MB、视频 10MB、普通文件 20MB。经典媒体上传
/// API 说的是同样这四个数（`document/path/90253`）。20 MB 是任何种类里最宽的那一个。
///
/// 逐种类的帽由 [`super::outbound_media`] 的 `wecom_media_kind` 在上一层施加：**超帽的图片被降
/// 级成文件**，而不是拿一个服务端一定会拒的东西去问它。
///
/// 把一个 40 MB 的文件切成块、最后被拒，代价是八十次往返、全程驻留内存、以及一个等了好几分钟
/// 才被告知"不行"的用户。
pub const MAX_MEDIA_UPLOAD_BYTES: usize = 20 << 20;

/// 视频消息两个必填字段的字节帽。
pub const VIDEO_TITLE_BYTES: usize = 64;
pub const VIDEO_DESCRIPTION_BYTES: usize = 512;

/// 一块最多重发几次（上游 `mediaChunkAttempts`）。第二次只为**丢掉的 ack** 存在：服务端拒掉的
/// 块还会被拒，所以一次拒绝当场结束这次上传。
pub const MEDIA_CHUNK_ATTEMPTS: usize = 2;

// =====================================================================
// 错误
// =====================================================================

/// 上传 / 发送媒体的失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaUploadError {
    /// 上游 `errMediaUploadTooLarge`。
    #[error("wecom: media exceeds the {MAX_MEDIA_UPLOAD_BYTES} byte upload limit")]
    UploadTooLarge,
    /// 上游 `errMediaUploadEmpty`。
    #[error("wecom: media has no bytes to upload")]
    UploadEmpty,
    /// 上游 `validate()`：`Kind` 不是四个取值之一。
    #[error("wecom: media type is not uploadable")]
    NotUploadable,
    /// 上游 `validate()`：文件名是空的。
    #[error("wecom: media upload requires a filename")]
    MissingFilename,
    /// 上游 `readObject` 的两条：这个 URL 不是本部署存的对象，或者读它失败。
    #[error("wecom: attachment could not be read from object storage")]
    Storage,
    /// 上游 `sendMsgMediaBody`：没有 `chat_id`。
    #[error("wecom: send_msg requires chat_id")]
    MissingChatId,
    /// 上游 `sendMsgMediaBody`：`chat_type` 不是 1 / 2。
    #[error("wecom: send_msg chat_type must be 1 (single) or 2 (group)")]
    BadChatType,
    /// 上游 `mediaBodyFields`：没有 `media_id`。
    #[error("wecom: media message requires a media_id")]
    MissingMediaId,
    /// 上游 `mediaBodyFields`：`Kind` 不是媒体 `msgtype`。
    #[error("wecom: media msgtype is not accepted")]
    NotMediaMsgType,
    /// 上游 `fmt.Errorf("wecom: decode upload %s response: %w", …)`。
    #[error("wecom: upload response could not be decoded")]
    Decode,
    /// 上游 `errors.New("wecom: upload init returned no upload_id")`。
    #[error("wecom: upload init returned no upload_id")]
    NoUploadId,
    /// 上游 `errors.New("wecom: upload finish returned no media_id")`。
    #[error("wecom: upload finish returned no media_id")]
    NoMediaId,
    /// 上游 `fmt.Errorf("wecom: upload chunk %d: %w", index, lastErr)`。
    #[error("wecom: upload chunk {index} failed")]
    Chunk {
        index: usize,
        #[source]
        cause: SenderError,
    },
    /// 三次 `request` 自身的失败（含服务端拒绝）。
    #[error(transparent)]
    Send(#[from] SenderError),
}

// =====================================================================
// 形状
// =====================================================================

/// 一个正在去聊天的文件（上游 `outboundMedia`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundMedia {
    /// 决定完成之后那条消息用哪个 `msgtype`，而 `WeCom` 会**按它校验字节**：一个被声明成图片的
    /// `.pptx` 会被拒，不会被转换。
    pub kind: MediaMsgType,
    /// 收件人在文件卡上看到的名字。它也是服务端关于格式的**唯一**提示，所以要保留扩展名。
    pub filename: String,
    pub data: Vec<u8>,
}

impl OutboundMedia {
    /// 上游 `validate()`。
    ///
    /// # Errors
    ///
    /// [`MediaUploadError::NotUploadable`] / [`MediaUploadError::MissingFilename`]。
    pub fn validate(&self) -> Result<(), MediaUploadError> {
        if !MediaMsgType::ALL.contains(&self.kind) {
            return Err(MediaUploadError::NotUploadable);
        }
        if self.filename.trim().is_empty() {
            return Err(MediaUploadError::MissingFilename);
        }
        Ok(())
    }
}

/// 一次完成的上传，被寻址成一条消息（上游 `mediaSend`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSend {
    pub kind: MediaMsgType,
    pub media_id: String,
    /// `Title` 与 `Description` 是**视频独有**的 —— 另外三种没有这个字段，而带它的 body 会被拒。
    pub title: String,
    pub description: String,
}

/// 上游 `mediaChunkParallelism`：同时最多几块在飞，**并且随文件变大而下降**。
///
/// 这个阶梯是 `WeCom` 自己的，来自 SDK 的 `uploadMedia`（`totalChunks <= 4 ? totalChunks :
/// totalChunks <= 10 ? 3 : 2`）。它往下走的原因就在旁边那条注释里 —— 超过十块之后，`WeCom`
/// 后端对一批并发块的回答是一个 system error。那是**服务端**的限制，所以再多的重试也绕不过去，
/// 而一个固定的 4 会恰好在最需要成功的那些大上传上失败。
///
/// 小的一端同样要紧、方向相反：四块或更少时每一块都立刻发，于是一个 2 MB 的文件不会为一条
/// 只对大文件成立的谨慎而被串行化。
///
/// 第二条保持低的理由：这条 socket 与 bot 所在的每个其它聊共用，一个全速的文件会把它们的回复
/// 拖住。
#[must_use]
pub fn media_chunk_parallelism(chunks: usize) -> usize {
    match chunks {
        0 => 1,
        1..=4 => chunks,
        5..=10 => 3,
        _ => 2,
    }
}

/// 上游 `splitMediaChunks`：把文件切成协议收得下的那些片。
///
/// 片**借用**输入（上游也是：直到某一块被 base64 成它自己那一帧之前，什么都不复制）。
///
/// # Errors
///
/// [`MediaUploadError::UploadEmpty`] / [`MediaUploadError::UploadTooLarge`]。
pub fn split_media_chunks(data: &[u8]) -> Result<Vec<&[u8]>, MediaUploadError> {
    if data.is_empty() {
        return Err(MediaUploadError::UploadEmpty);
    }
    if data.len() > MAX_MEDIA_UPLOAD_BYTES {
        return Err(MediaUploadError::UploadTooLarge);
    }
    Ok(data.chunks(MEDIA_CHUNK_BYTES).collect())
}

// =====================================================================
// 三步上传
// =====================================================================

/// 上游 `uploadMedia`：把一个文件走完三步，交回一条消息可以围绕它建起来的 `media_id`。
/// `deadline` 界住整件事。
///
/// # Errors
///
/// 见 [`MediaUploadError`]。
pub async fn upload_media(
    sender: &WsSender,
    media: &OutboundMedia,
    deadline: Deadline,
) -> Result<String, MediaUploadError> {
    media.validate()?;
    let chunks = split_media_chunks(&media.data)?;
    let upload_id = upload_media_init(sender, media, chunks.len(), deadline).await?;
    upload_media_chunks(sender, &upload_id, &chunks, deadline).await?;
    upload_media_finish(sender, &upload_id, deadline).await
}

/// 上游 `uploadMediaInit`：声明这个文件并取回 `upload_id`。
///
/// 可选的 `md5` 字段**刻意不发**：文档列了它却没说它是对**原始文件**还是对它的 base64 取的，
/// 而一个会核对"我们猜错的那个值"的服务端会用一条完全不说明原因的 `errcode` 拒掉每一次上传。
///
/// # Errors
///
/// 见 [`MediaUploadError`]。
pub async fn upload_media_init(
    sender: &WsSender,
    media: &OutboundMedia,
    chunks: usize,
    deadline: Deadline,
) -> Result<String, MediaUploadError> {
    let body = sender
        .request(
            deadline,
            CMD_UPLOAD_MEDIA_INIT,
            json!({
                "type": media.kind.as_str(),
                "filename": media.filename,
                "total_size": media.data.len(),
                "total_chunks": chunks,
            }),
        )
        .await?;
    let object = body.as_object().ok_or(MediaUploadError::Decode)?;
    let upload_id = object
        .get("upload_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if upload_id.is_empty() {
        return Err(MediaUploadError::NoUploadId);
    }
    Ok(upload_id.to_owned())
}

/// 上游 `uploadMediaChunks`：把每一片都发出去，几片几片地。
///
/// 第一个失败就结束这次上传：没有值得完成的半截上传。
///
/// # Errors
///
/// 见 [`MediaUploadError::Chunk`]。
pub async fn upload_media_chunks(
    sender: &WsSender,
    upload_id: &str,
    chunks: &[&[u8]],
    deadline: Deadline,
) -> Result<(), MediaUploadError> {
    let limit = media_chunk_parallelism(chunks.len());
    // ⚠️ 显式的 `async move` 块：直接把 `upload_media_chunk(..)` 交给 `.map(..)` 会让
    // `buffer_unordered` 的闭包在借用生命周期上不够泛化（`E0308`/`FnOnce` 不是足够泛化），
    // 而这个形状把每一次调用连同它的借用一起推进 future 里。
    let futures: Vec<_> = chunks
        .iter()
        .copied()
        .enumerate()
        .map(|(index, chunk)| upload_media_chunk(sender, upload_id, index, chunk, deadline))
        .collect();
    let mut in_flight = stream::iter(futures).buffer_unordered(limit);
    while let Some(result) = in_flight.next().await {
        result?;
    }
    Ok(())
}

/// 上游 `uploadMediaChunk`：发一片，**判决没回来才再发一次**。
///
/// `chunk_index` 上 wire 时是**数字**：文档的字段表把它叫字符串，而旁边的示例传的是数字；
/// `WeCom` 自己的 SDK 发的也是数字，所以服务端被证明接受的就是数字。
///
/// # Errors
///
/// 见 [`MediaUploadError::Chunk`]。
pub async fn upload_media_chunk(
    sender: &WsSender,
    upload_id: &str,
    index: usize,
    chunk: &[u8],
    deadline: Deadline,
) -> Result<(), MediaUploadError> {
    let body = json!({
        "upload_id": upload_id,
        "chunk_index": index,
        "base64_data": base64_encode(chunk),
    });
    let mut last: Option<SenderError> = None;
    for attempt in 0..MEDIA_CHUNK_ATTEMPTS {
        match sender
            .request(deadline, CMD_UPLOAD_MEDIA_CHUNK, body.clone())
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => {
                // 一次**拒绝**是服务端的答案，而它还会是这个答案。只有一次**从未回来**的判决
                // 值得再问一次，而让这件事安全的正是协议自己的幂等性。
                let retryable = matches!(error, SenderError::AckTimeout);
                last = Some(error);
                if !retryable {
                    break;
                }
                tracing::warn!(
                    upload_id,
                    chunk_index = index,
                    attempt = attempt + 1,
                    "wecom: media chunk got no verdict, sending it again"
                );
            }
        }
    }
    Err(MediaUploadError::Chunk {
        index,
        cause: last.unwrap_or(SenderError::NotAttempted),
    })
}

/// 上游 `uploadMediaFinish`：封口这次上传并取回 `media_id`。
///
/// # Errors
///
/// 见 [`MediaUploadError`]。
pub async fn upload_media_finish(
    sender: &WsSender,
    upload_id: &str,
    deadline: Deadline,
) -> Result<String, MediaUploadError> {
    let body = sender
        .request(
            deadline,
            CMD_UPLOAD_MEDIA_FINISH,
            json!({ "upload_id": upload_id }),
        )
        .await?;
    let object = body.as_object().ok_or(MediaUploadError::Decode)?;
    let media_id = object
        .get("media_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if media_id.is_empty() {
        return Err(MediaUploadError::NoMediaId);
    }
    Ok(media_id.to_owned())
}

/// base64（`StdEncoding`，**带填充** —— 上游 `base64.StdEncoding`）。
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

// =====================================================================
// 发送
// =====================================================================

/// 上游 `sendMedia`：把一个已经上传好的文件作为一条消息投递，走的是驮文本的那条
/// `aibot_send_msg` 推送。
///
/// 文档留了两条路而且自相矛盾：`aibot_send_msg` 的字段表只列 `template_card` 与 `markdown`，
/// 而 `aibot_respond_msg` 的 `msgtype` 表确实点了 `file` / `image` / `voice` / `video`。
/// **推送赢了**，理由两条（上游逐字）：`WeCom` 自己的 Node SDK 在 `aibot_send_msg` 上推媒体，
/// 这是两边唯一一条来自**能跑的代码**的证据；而 `aibot_respond_msg` 是按打开这一轮的那个回调的
/// `req_id` 寻址的，这条投递路径**手上没有**：回答已经作为自己的推送出去了，没有活着的轮次可回。
///
/// 真实 bot 接受两者中的哪一个**没有被观察过**。一次拒绝会被原样返回、不会换另一条路重试 ——
/// 调用方告诉用户文件没送到，这是诚实的；而在一个我们并不持有的地址上再试一次不是。
///
/// # Errors
///
/// 见 [`MediaUploadError`] 与 [`SenderError`]。
pub async fn send_media(
    sender: &WsSender,
    chat_id: &str,
    chat_type: i32,
    media: &MediaSend,
    deadline: Deadline,
) -> Result<(), MediaUploadError> {
    let body = send_msg_media_body(chat_id, chat_type, media)?;
    // 与每一条文本推送取的是**同一把**每聊锁，理由相同、防的是同一个读者：一个文件如果真的
    // 在一段回答还在写的时候送达，就会落在它两段之间，而 `(1/3)` 里的计数不再说明它们属于
    // 哪条文本。附件投递是**被 spawn 出来的** ⇒ 两者按构造就是并发的。
    //
    // 取在这里，**不是**取在上面那次上传周围：这把锁的任务是**这个聊收到东西的顺序**，而一次
    // 上传往聊里放的东西是零 —— 它是好几百次往返、好几十兆，为一个还没决定要变成消息的传输
    // 占住这个聊的轮次，会把发往这个聊的每一条别的消息都停在它后面。
    let guard = sender.chat_locks().acquire(chat_id, deadline).await?;
    // 走 `request` 而不是别的：对 `WeCom` 来说一个文件与一段回答**是同一条** `aibot_send_msg`、
    // 花的是**同一份**每聊配额，所以一轮用文字回答、又发三个附件，必须算四次。这里缺的那份
    // 配额正是 M7-20 的 `sendMsgFrame`（见模块文档差异 2），而它对这条路径的意义与对
    // `send_text` 完全一样。
    let result = sender.request(deadline, CMD_SEND_MSG, body).await;
    drop(guard);
    match result {
        Ok(_) => {
            tracing::info!(
                media_id = media.media_id.as_str(),
                msgtype = media.kind.as_str(),
                "wecom: media delivered"
            );
            Ok(())
        }
        Err(error) => {
            // 一次从未到达的判决不是一次拒绝。帧出去了，而读循环可能只是忙。在那上面重发会
            // **同一条 `media_id`** 再出去一次，而对方就会看到两次那张图、且没有东西能撤回
            // —— 所以它原样上报，并在日志里**说清是哪一种**，因为"可能已经到了"与"被拒了"
            // 是要追的两件不同的事。
            if matches!(error, SenderError::AckTimeout) {
                tracing::warn!(
                    media_id = media.media_id.as_str(),
                    msgtype = media.kind.as_str(),
                    "wecom: media push not acknowledged in time; not resending"
                );
            }
            Err(MediaUploadError::Send(error))
        }
    }
}

/// 上游 `sendMsgMediaBody`：构造一条携带媒体的 `aibot_send_msg` body。
///
/// # Errors
///
/// 见 [`MediaUploadError`]。
pub fn send_msg_media_body(
    chat_id: &str,
    chat_type: i32,
    media: &MediaSend,
) -> Result<Value, MediaUploadError> {
    if chat_id.is_empty() {
        return Err(MediaUploadError::MissingChatId);
    }
    if chat_type != CHAT_TYPE_SINGLE_INT && chat_type != CHAT_TYPE_GROUP_INT {
        return Err(MediaUploadError::BadChatType);
    }
    let mut body = media_body_fields(media)?;
    body["chatid"] = json!(chat_id);
    body["chat_type"] = json!(chat_type);
    Ok(body)
}

/// 上游 `mediaBodyFields`：一条媒体帧携带的 `{msgtype, <kind>:{…}}` 对。
///
/// # Errors
///
/// 见 [`MediaUploadError`]。
pub fn media_body_fields(media: &MediaSend) -> Result<Value, MediaUploadError> {
    if media.media_id.is_empty() {
        return Err(MediaUploadError::MissingMediaId);
    }
    if !MediaMsgType::ALL.contains(&media.kind) {
        return Err(MediaUploadError::NotMediaMsgType);
    }
    let mut nested = json!({ "media_id": media.media_id });
    if media.kind == MediaMsgType::Video {
        nested["title"] = json!(clip_utf8(&media.title, VIDEO_TITLE_BYTES));
        nested["description"] = json!(clip_utf8(&media.description, VIDEO_DESCRIPTION_BYTES));
    }
    let mut body = serde_json::Map::new();
    body.insert("msgtype".to_owned(), json!(media.kind.as_str()));
    body.insert(media.kind.as_str().to_owned(), nested);
    Ok(Value::Object(body))
}

/// 上游 `clipUTF8`：把一个字符串按**字节**预算切在字符边界上。
///
/// `WeCom` 数的是字节，一个中文标题每个字花三个，而切在一个字的中间会把坏掉的 UTF-8 发给服务端。
#[must_use]
pub fn clip_utf8(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut cut = max;
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    value[..cut].to_owned()
}

#[cfg(test)]
mod tests;
