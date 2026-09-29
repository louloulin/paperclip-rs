use super::{
    fmt, Arc, Auditor, Deduper, IdentityResolver, InstallationResolver, MediaResolver,
    OutboundReplier, SessionBinder, TypingNotifier,
};

/// 每平台一组的端口包（上游 `ResolverSet`）。
///
/// `installation` / `identity` / `dedup` / `session` / `audit` 是**必需**的；
/// `media` / `replier` / `typing` 可选（`None` = 该平台没有这一面）。
/// `origin_type` 是 `/issue` 写给 `issue.origin_type` 的渠道标签。
pub struct ResolverSet {
    pub installation: Arc<dyn InstallationResolver>,
    pub identity: Arc<dyn IdentityResolver>,
    pub dedup: Arc<dyn Deduper>,
    pub session: Arc<dyn SessionBinder>,
    pub audit: Arc<dyn Auditor>,
    pub media: Option<Arc<dyn MediaResolver>>,
    pub replier: Option<Arc<dyn OutboundReplier>>,
    pub typing: Option<Arc<dyn TypingNotifier>>,
    /// `/issue` 的 `origin_type`（Feishu: `lark_chat`）。
    pub origin_type: String,
}

impl fmt::Debug for ResolverSet {
    /// 端口是 trait 对象 ⇒ 只列**存在性**（不含任何端口内部状态）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolverSet")
            .field("installation", &"<dyn InstallationResolver>")
            .field("identity", &"<dyn IdentityResolver>")
            .field("dedup", &"<dyn Deduper>")
            .field("session", &"<dyn SessionBinder>")
            .field("audit", &"<dyn Auditor>")
            .field("media", &self.media.is_some())
            .field("replier", &self.replier.is_some())
            .field("typing", &self.typing.is_some())
            .field("origin_type", &self.origin_type)
            .finish()
    }
}

impl ResolverSet {
    /// 必填端口的便捷构造（可选端口为 `None`）。
    pub fn new(
        installation: Arc<dyn InstallationResolver>,
        identity: Arc<dyn IdentityResolver>,
        dedup: Arc<dyn Deduper>,
        session: Arc<dyn SessionBinder>,
        audit: Arc<dyn Auditor>,
        origin_type: impl Into<String>,
    ) -> Self {
        Self {
            installation,
            identity,
            dedup,
            session,
            audit,
            media: None,
            replier: None,
            typing: None,
            origin_type: origin_type.into(),
        }
    }

    /// 挂上媒体面。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn MediaResolver>) -> Self {
        self.media = Some(media);
        self
    }

    /// 挂上出站回复器。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn OutboundReplier>) -> Self {
        self.replier = Some(replier);
        self
    }

    /// 挂上打字指示器。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<dyn TypingNotifier>) -> Self {
        self.typing = Some(typing);
        self
    }
}
