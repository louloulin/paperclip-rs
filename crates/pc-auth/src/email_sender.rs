//! Email sender 抽象。
//!
//! 与 Node `auth/better-auth.ts` 中 email 发送路径一致：
//! - 上层（注册 / 修改邮箱 / 重置密码）调用 `EmailSender::send`。
//! - 真实部署用 SMTP / SES / SendGrid adapter；本地 dev 用 NoopEmailSender。
//! - 测试用 LogEmailSender（在内存记录消息）。
//!
//! 设计原则：
//! - 不引入外部邮件库（lettre / ses-sdk 等）；由调用方提供 `EmailSender` 实现。
//! - 字段全在 `EmailMessage` 上；不做 magic（如 subject 自动加前缀），由调用方控制。
//! - 简单模板插值 `{name}` → 实际值（避免拉 handlebars/tera）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ============================================================================
// EmailAddress - 基础校验
// ============================================================================

/// 邮件地址（含可选 display name）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmailAddress {
    pub address: String,
    pub name: Option<String>,
}

impl EmailAddress {
    /// 构造一个新的 email address；返回 `Err` 如果 address 不通过基础校验。
    pub fn new(address: impl Into<String>) -> Result<Self, EmailSenderError> {
        let address = address.into();
        validate_email_address(&address)?;
        Ok(Self { address, name: None })
    }

    /// 构造带 display name 的 email address。
    pub fn with_name(
        address: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Self, EmailSenderError> {
        let address = address.into();
        let name = name.into();
        if name.is_empty() {
            return Err(EmailSenderError::InvalidConfig("display name must be non-empty".into()));
        }
        if name.contains('\n') || name.contains('\r') {
            return Err(EmailSenderError::InvalidConfig(
                "display name must not contain CR/LF".into(),
            ));
        }
        validate_email_address(&address)?;
        Ok(Self {
            address,
            name: Some(name),
        })
    }

    /// 解析 "Name <addr@host>" 或 "addr@host"。
    pub fn parse(raw: &str) -> Result<Self, EmailSenderError> {
        let trimmed = raw.trim();
        if let Some(idx) = trimmed.rfind('<') {
            let name = trimmed[..idx].trim().trim_matches('"').to_string();
            let after = &trimmed[idx + 1..];
            let close = after.rfind('>').ok_or_else(|| {
                EmailSenderError::InvalidFormat("missing closing '>' in address".into())
            })?;
            let addr = after[..close].trim().to_string();
            if name.is_empty() {
                Self::new(addr)
            } else {
                Self::with_name(addr, name)
            }
        } else {
            Self::new(trimmed)
        }
    }

    /// 渲染为 RFC 5322 风格（带 name 时 `"Name" <addr>`）。
    #[must_use]
    pub fn render(&self) -> String {
        match &self.name {
            Some(n) => format!("\"{}\" <{}>", n, self.address),
            None => self.address.clone(),
        }
    }
}

impl std::fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

fn validate_email_address(address: &str) -> Result<(), EmailSenderError> {
    if address.is_empty() {
        return Err(EmailSenderError::InvalidFormat("address is empty".into()));
    }
    if address.len() > 254 {
        return Err(EmailSenderError::InvalidFormat("address too long".into()));
    }
    // 极简校验：单个 @，两侧非空，本地部分不含控制字符或未引用空格
    let mut parts = address.splitn(2, '@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if local.is_empty() || domain.is_empty() {
        return Err(EmailSenderError::InvalidFormat("missing local or domain part".into()));
    }
    if local.contains(char::is_control) || local.contains(' ') {
        return Err(EmailSenderError::InvalidFormat("invalid local part".into()));
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err(EmailSenderError::InvalidFormat("invalid domain".into()));
    }
    if domain.contains(char::is_control) || domain.contains(' ') {
        return Err(EmailSenderError::InvalidFormat("invalid domain".into()));
    }
    Ok(())
}

// ============================================================================
// EmailMessage
// ============================================================================

/// 邮件内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    pub from: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub reply_to: Option<EmailAddress>,
    /// 自定义 headers（不覆盖 From/To/Subject）。
    pub headers: HashMap<String, String>,
}

impl EmailMessage {
    pub fn new(
        from: EmailAddress,
        to: Vec<EmailAddress>,
        subject: impl Into<String>,
        body_text: impl Into<String>,
    ) -> Self {
        Self {
            from,
            to,
            subject: subject.into(),
            body_text: body_text.into(),
            body_html: None,
            reply_to: None,
            headers: HashMap::new(),
        }
    }

    /// 设置 HTML body。
    #[must_use]
    pub fn with_html(mut self, html: impl Into<String>) -> Self {
        self.body_html = Some(html.into());
        self
    }

    /// 设置 reply-to。
    pub fn with_reply_to(mut self, addr: EmailAddress) -> Self {
        self.reply_to = Some(addr);
        self
    }

    /// 加一个自定义 header。
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// 校验消息字段。
    pub fn validate(&self) -> Result<(), EmailSenderError> {
        if self.subject.is_empty() {
            return Err(EmailSenderError::InvalidConfig("subject is empty".into()));
        }
        if self.body_text.is_empty() && self.body_html.as_deref().unwrap_or("").is_empty() {
            return Err(EmailSenderError::InvalidConfig("body is empty".into()));
        }
        if self.to.is_empty() {
            return Err(EmailSenderError::InvalidConfig("to is empty".into()));
        }
        // subject 不能含 CRLF（防 header injection）
        if self.subject.contains('\n') || self.subject.contains('\r') {
            return Err(EmailSenderError::InvalidConfig(
                "subject must not contain CR/LF (header injection)".into(),
            ));
        }
        for (k, v) in &self.headers {
            if k.contains('\n') || v.contains('\n') || k.contains('\r') || v.contains('\r') {
                return Err(EmailSenderError::InvalidConfig(
                    "header must not contain CR/LF (header injection)".into(),
                ));
            }
        }
        Ok(())
    }
}

// ============================================================================
// 模板插值
// ============================================================================

/// 简单 `{key}` 模板插值。`{key}` 会被 `vars.get("key")` 替换（缺失则保留原文）。
#[must_use]
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // 找匹配的 '}'
            if let Some(end_rel) = template[i + 1..].find('}') {
                let end = i + 1 + end_rel;
                let key = &template[i + 1..end];
                if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    if let Some(value) = vars.get(key) {
                        out.push_str(value);
                    } else {
                        // 保留原文 {key}
                        out.push_str(&template[i..=end]);
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        // 把当前 UTF-8 字符追加（按 char 边界安全）
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ============================================================================
// EmailSender trait + 实现
// ============================================================================

/// 邮件发送错误分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailSenderError {
    /// 配置错误（from 地址无效、subject 为空等）。
    InvalidConfig(String),
    /// 格式错误（address 解析失败）。
    InvalidFormat(String),
    /// 网络/上游错误（重试可恢复）。
    Upstream(String),
    /// 限流（429）—— 可重试。
    RateLimited { retry_after_ms: Option<u64> },
    /// 鉴权失败（401/403）—— 不可重试。
    AuthFailed(String),
    /// 其它。
    Other(String),
}

impl EmailSenderError {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Upstream(_) | Self::RateLimited { .. })
    }
}

impl std::fmt::Display for EmailSenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(s) => write!(f, "config: {s}"),
            Self::InvalidFormat(s) => write!(f, "format: {s}"),
            Self::Upstream(s) => write!(f, "upstream: {s}"),
            Self::RateLimited { retry_after_ms } => {
                write!(f, "rate limited (retry_after_ms={retry_after_ms:?})")
            }
            Self::AuthFailed(s) => write!(f, "auth: {s}"),
            Self::Other(s) => write!(f, "other: {s}"),
        }
    }
}

impl std::error::Error for EmailSenderError {}

/// Email sender trait。生产实现：SMTP / SES / SendGrid；测试：LogEmailSender。
#[async_trait::async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, message: &EmailMessage) -> Result<String, EmailSenderError>;
    /// Provider 标识（"smtp" / "ses" / "noop" / "log"）。
    fn provider_id(&self) -> &'static str;
}

/// Noop 实现：仅记录到 tracing；返回 synthetic message id。
/// 用于本地 dev / 单元测试 / `PAPERCLIP_EMAIL_PROVIDER=off`。
pub struct NoopEmailSender {
    from: EmailAddress,
}

impl NoopEmailSender {
    #[must_use]
    pub fn new(from: EmailAddress) -> Self {
        Self { from }
    }
}

#[async_trait::async_trait]
impl EmailSender for NoopEmailSender {
    async fn send(&self, message: &EmailMessage) -> Result<String, EmailSenderError> {
        message.validate()?;
        tracing::info!(
            target: "pc_auth::email",
            from = %message.from,
            to = ?message.to,
            subject = %message.subject,
            "noop email send (no actual delivery)",
        );
        Ok(format!("noop-{}", uuid::Uuid::new_v4()))
    }
    fn provider_id(&self) -> &'static str {
        "noop"
    }
}

/// Log 实现：把所有消息存到内存；用于测试断言 "发送了几封" / "内容是什么"。
pub struct LogEmailSender {
    from: EmailAddress,
    log: Arc<Mutex<Vec<EmailMessage>>>,
}

impl LogEmailSender {
    #[must_use]
    pub fn new(from: EmailAddress) -> Self {
        Self {
            from,
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 取得消息快照。
    #[must_use]
    pub fn messages(&self) -> Vec<EmailMessage> {
        self.log.lock().expect("log mutex").clone()
    }

    /// 计数。
    #[must_use]
    pub fn count(&self) -> usize {
        self.log.lock().expect("log mutex").len()
    }

    /// 清空。
    pub fn clear(&self) {
        self.log.lock().expect("log mutex").clear();
    }
}

#[async_trait::async_trait]
impl EmailSender for LogEmailSender {
    async fn send(&self, message: &EmailMessage) -> Result<String, EmailSenderError> {
        message.validate()?;
        let id = format!("log-{}", uuid::Uuid::new_v4());
        self.log.lock().expect("log mutex").push(message.clone());
        Ok(id)
    }
    fn provider_id(&self) -> &'static str {
        "log"
    }
}

// ============================================================================
// 工厂
// ============================================================================

/// 根据 JSON 配置构造一个 EmailSender。
///
/// 支持的 provider_id：
/// - `"noop"`：NoopEmailSender（默认）
/// - `"log"`：LogEmailSender（测试用；返回 Arc 包装以便测试代码读取）
pub fn build_email_sender(
    provider_id: &str,
    from: EmailAddress,
) -> Result<Arc<dyn EmailSender>, EmailSenderError> {
    match provider_id {
        "noop" | "" => Ok(Arc::new(NoopEmailSender::new(from))),
        "log" => Ok(Arc::new(LogEmailSender::new(from))),
        other => Err(EmailSenderError::InvalidConfig(format!(
            "unknown email provider `{other}` (expected noop|log)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r568_email_address_new_validates() {
        assert!(EmailAddress::new("a@b.co").is_ok());
        assert!(EmailAddress::new("").is_err());
        assert!(EmailAddress::new("no-at-sign").is_err());
        assert!(EmailAddress::new("@b.co").is_err());
        assert!(EmailAddress::new("a@").is_err());
        assert!(EmailAddress::new("a@b").is_err()); // domain 无 .
    }

    #[test]
    fn r568_email_address_with_name() {
        let a = EmailAddress::with_name("a@b.co", "Alice").unwrap();
        assert_eq!(a.render(), "\"Alice\" <a@b.co>");
        assert!(EmailAddress::with_name("a@b.co", "").is_err());
    }

    #[test]
    fn r568_email_address_rejects_crlf_in_name() {
        assert!(EmailAddress::with_name("a@b.co", "Bad\rName").is_err());
        assert!(EmailAddress::with_name("a@b.co", "Bad\nName").is_err());
    }

    #[test]
    fn r568_email_address_parse_rfc5322_style() {
        let a = EmailAddress::parse("\"Alice\" <a@b.co>").unwrap();
        assert_eq!(a.address, "a@b.co");
        assert_eq!(a.name.as_deref(), Some("Alice"));
        let a = EmailAddress::parse("a@b.co").unwrap();
        assert_eq!(a.address, "a@b.co");
        assert!(a.name.is_none());
        let a = EmailAddress::parse("Alice <a@b.co>").unwrap();
        assert_eq!(a.name.as_deref(), Some("Alice"));
    }

    #[test]
    fn r568_email_address_parse_rejects_missing_close() {
        assert!(EmailAddress::parse("Alice <a@b.co").is_err());
    }

    #[test]
    fn r568_email_message_validate_rejects_empty() {
        let from = EmailAddress::new("noreply@b.co").unwrap();
        let to = vec![EmailAddress::new("a@b.co").unwrap()];
        let mut m = EmailMessage::new(from.clone(), to.clone(), "subj", "body");
        assert!(m.clone().validate().is_ok());
        m.subject = "".into();
        assert!(m.validate().is_err());
        m.subject = "subj".into();
        m.body_text = "".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn r568_email_message_validate_rejects_header_injection() {
        let from = EmailAddress::new("noreply@b.co").unwrap();
        let to = vec![EmailAddress::new("a@b.co").unwrap()];
        let m = EmailMessage::new(from, to, "subj\r\nBcc: evil@b.co", "body");
        assert!(m.validate().is_err());
        let m2 = EmailMessage::new(
            EmailAddress::new("noreply@b.co").unwrap(),
            vec![EmailAddress::new("a@b.co").unwrap()],
            "subj",
            "body",
        )
        .with_header("X-Custom", "ok")
        .with_header("X-Evil", "val\r\nBcc: x");
        assert!(m2.validate().is_err());
    }

    #[test]
    fn r568_render_template_replaces_known_keys() {
        let mut vars = HashMap::new();
        vars.insert("name".into(), "Alice".into());
        vars.insert("url".into(), "https://x".into());
        let out = render_template("Hi {name}, click {url}", &vars);
        assert_eq!(out, "Hi Alice, click https://x");
    }

    #[test]
    fn r568_render_template_keeps_unknown_keys() {
        let vars = HashMap::new();
        let out = render_template("Hi {name}, token={tok}", &vars);
        assert_eq!(out, "Hi {name}, token={tok}");
    }

    #[test]
    fn r568_render_template_rejects_malformed() {
        let vars = HashMap::new();
        // 没有匹配的 }：原文保留
        let out = render_template("partial {nokey", &vars);
        assert_eq!(out, "partial {nokey");
    }

    #[tokio::test]
    async fn r568_noop_sender_records_and_returns_id() {
        let from = EmailAddress::new("noreply@b.co").unwrap();
        let s = NoopEmailSender::new(from.clone());
        let m = EmailMessage::new(
            from,
            vec![EmailAddress::new("a@b.co").unwrap()],
            "Hi",
            "body",
        );
        let id = s.send(&m).await.unwrap();
        assert!(id.starts_with("noop-"));
    }

    #[tokio::test]
    async fn r568_noop_rejects_invalid_message() {
        let from = EmailAddress::new("noreply@b.co").unwrap();
        let s = NoopEmailSender::new(from.clone());
        let m = EmailMessage::new(from, vec![], "Hi", "body");
        assert!(s.send(&m).await.is_err());
    }

    #[tokio::test]
    async fn r568_log_sender_records_messages() {
        let from = EmailAddress::new("noreply@b.co").unwrap();
        let s = LogEmailSender::new(from.clone());
        let m1 = EmailMessage::new(
            from.clone(),
            vec![EmailAddress::new("a@b.co").unwrap()],
            "Hi 1",
            "body 1",
        );
        let m2 = EmailMessage::new(
            from,
            vec![EmailAddress::new("b@b.co").unwrap()],
            "Hi 2",
            "body 2",
        );
        s.send(&m1).await.unwrap();
        s.send(&m2).await.unwrap();
        assert_eq!(s.count(), 2);
        let captured = s.messages();
        assert_eq!(captured[0].subject, "Hi 1");
        assert_eq!(captured[1].subject, "Hi 2");
        s.clear();
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn r568_build_email_sender_factory() {
        let from = EmailAddress::new("noreply@b.co").unwrap();
        let s = build_email_sender("noop", from.clone()).unwrap();
        assert_eq!(s.provider_id(), "noop");
        let s = build_email_sender("log", from.clone()).unwrap();
        assert_eq!(s.provider_id(), "log");
        let s = build_email_sender("", from.clone()).unwrap();
        assert_eq!(s.provider_id(), "noop");
        assert!(build_email_sender("ses", from).is_err());
    }

    #[test]
    fn r568_email_sender_error_is_transient() {
        assert!(EmailSenderError::Upstream("oops".into()).is_transient());
        assert!(EmailSenderError::RateLimited { retry_after_ms: Some(1000) }.is_transient());
        assert!(!EmailSenderError::InvalidConfig("bad".into()).is_transient());
        assert!(!EmailSenderError::AuthFailed("bad".into()).is_transient());
    }
}
