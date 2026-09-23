//! 授权 scope 的裁决（**唯一实现点**）。
//!
//! - **写者**：M6-1（`docs/57` §3.2 / §4.2：「scope 判定唯一实现点」）。
//! - **上游**：`pkg/plugincontract` 的 scope 相关判定 + `internal/handler/plugin*.go` 的
//!   授权检查；落库列是 `plugin_installation.granted_scopes`（JSONB）。
//! - **为什么单独一个文件**：这个判定要被 **安装/启停（M6-5）、运行时面（M6-6）、
//!   公开 API + bridge（M6-7）** 三处调用。抄三份就会出现「预览能装、真装不能」这类
//!   不对称缺陷 —— 上游只有一份，本仓也必须只有一份。
//! - **与 `mc-core::plugin::PluginScope` 的分工**：那个是 **transparent newtype**（只保证
//!   字符串形态与序列化），**取值集合的权威在本文件**；`mc-core` 故意不枚举 scope 值。
//!   （`mc-core` 的注释把这个权威点写成 `mc-plugin-host::capabilities`；`capabilities.rs`
//!   因此 `pub use` 回本文件的常量 —— **定义只有一处**，两个路径取到的是同一个值。）
//! - **本仓约定**：判定是纯函数（入参 `granted_scopes` + 期望 scope）；`granted_scopes`
//!   为空 ⇒ **不给**任何 scope（不是「全给」）。
//! - **不做什么**：不做 OAuth 的授权码流程（那是 remote MCP 的 `mc-mcp/oauth.rs`）；
//!   不做 `net:` 的**后缀**匹配（上游口径是**精确 host**，见 `manifest.rs` 的
//!   `validate_hook_transport`）。
//!
//! **状态：M6-1 已落地。**

/// 十条**固定** scope（上游 `fixedScopes`，闭集：表里没有的一律拒绝）。
pub const SCOPE_ISSUES_READ: &str = "issues:read";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_ISSUES_WRITE: &str = "issues:write";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_COMMENTS_READ: &str = "comments:read";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_COMMENTS_WRITE: &str = "comments:write";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_TASKS_READ: &str = "tasks:read";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_TASKS_WRITE: &str = "tasks:write";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_AGENTS_READ: &str = "agents:read";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_MEMBERS_READ: &str = "members:read";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_STORAGE_USER: &str = "storage:user";
/// 见 [`SCOPE_ISSUES_READ`]。
pub const SCOPE_STORAGE_WORKSPACE: &str = "storage:workspace";

/// 唯一的**参数化**形态：`net:<domain>`。
///
/// 它同时是 iframe 的 CSP `connect-src` 白名单与 hook 出站 host 检查的来源。
pub const SCOPE_NET_PREFIX: &str = "net:";

/// 十条固定 scope（顺序即上游 `fixedScopes` 的书写顺序，便于逐行对账）。
pub const FIXED_SCOPES: &[&str] = &[
    SCOPE_ISSUES_READ,
    SCOPE_ISSUES_WRITE,
    SCOPE_COMMENTS_READ,
    SCOPE_COMMENTS_WRITE,
    SCOPE_TASKS_READ,
    SCOPE_TASKS_WRITE,
    SCOPE_AGENTS_READ,
    SCOPE_MEMBERS_READ,
    SCOPE_STORAGE_USER,
    SCOPE_STORAGE_WORKSPACE,
];

/// scope 判定/校验的失败原因（稳定错误码由 [`ScopeError::code`] 给出）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// 运行时授权失败：安装没有被授予这个 scope（上游 `plugin_action.go` 的文案）。
    #[error("this Plugin was not granted the {scope} scope")]
    NotGranted {
        /// 被拒绝的 scope。
        scope: String,
    },
    /// 安装时的整体相等校验失败（上游 `granted_scopes must match the manifest scopes exactly`）。
    #[error("granted_scopes must match the manifest scopes exactly")]
    NotExactMatch,
    /// 安装时授予了 manifest 没请求的 scope。
    #[error("granted_scopes contains {scope:?}, which the manifest does not request")]
    NotRequested {
        /// 多出来的 scope。
        scope: String,
    },
    /// 不是本宿主定义的 scope（manifest 解析期就拒，不等到安装）。
    #[error("unsupported scope {scope:?}")]
    Unsupported {
        /// 被拒的 scope 字面量。
        scope: String,
    },
    /// `net:` 后面的域名不合法。
    #[error("net scope has an invalid domain {domain:?}")]
    InvalidNetDomain {
        /// 被拒的域名部分。
        domain: String,
    },
}

impl ScopeError {
    /// 稳定错误码（route 层据此映射 HTTP 状态与响应体 `code`）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotGranted { .. } => "plugin_scope_denied",
            Self::NotExactMatch
            | Self::NotRequested { .. }
            | Self::Unsupported { .. }
            | Self::InvalidNetDomain { .. } => "plugin_scope_invalid",
        }
    }
}

/// 单个 scope 是否属于本宿主定义的集合（上游 `ValidateScope`）。
///
/// 注意：**只验取值是否合法**，不涉及「这个安装被授予了没有」。
pub fn validate_scope(scope: &str) -> Result<(), ScopeError> {
    if FIXED_SCOPES.contains(&scope) {
        return Ok(());
    }
    if let Some(domain) = scope.strip_prefix(SCOPE_NET_PREFIX) {
        if domain.len() > 253 || !is_net_domain(domain) {
            return Err(ScopeError::InvalidNetDomain {
                domain: domain.to_owned(),
            });
        }
        return Ok(());
    }
    Err(ScopeError::Unsupported {
        scope: scope.to_owned(),
    })
}

/// 只从**已授予**的 scope 里取 `net:` 域名（上游 `NetDomains`）。
///
/// 顺序与入参一致（上游也是保序的 `append`），重复项不去重 —— 调用方按需求去重。
#[must_use]
pub fn net_domains(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter_map(|scope| scope.strip_prefix(SCOPE_NET_PREFIX))
        .map(str::to_owned)
        .collect()
}

/// 精确匹配判定（上游 `hasScope`：`scope == want`，**没有**层级/前缀语义）。
///
/// `granted` 为空 ⇒ 恒 `false`：**空不等于全给**。
#[must_use]
pub fn is_granted(granted: &[String], wanted: &str) -> bool {
    granted.iter().any(|scope| scope == wanted)
}

/// 运行时授权：`required` 为空串表示这条路径**不需要** scope（`ContractPluginExtension`
/// 的无 scope 路径），否则必须精确命中（上游 `AuthorizePluginAction` 的 `scope == "" || …`）。
pub fn authorize(granted: &[String], required: &str) -> Result<(), ScopeError> {
    if required.is_empty() || is_granted(granted, required) {
        return Ok(());
    }
    Err(ScopeError::NotGranted {
        scope: required.to_owned(),
    })
}

/// 安装时：授予集必须与 manifest 请求集**逐元素相等**（上游安装路径的两条文案）。
///
/// 先查「有没有多给的」（上游先查 `manifest` 里有没有这个 scope，再比集合相等），
/// 因为多给的那一项才是管理员最需要知道的具体值。
pub fn validate_granted_scopes(manifest: &[String], granted: &[String]) -> Result<(), ScopeError> {
    for scope in granted {
        if !manifest.iter().any(|requested| requested == scope) {
            return Err(ScopeError::NotRequested {
                scope: scope.clone(),
            });
        }
    }
    if manifest.len() == granted.len() {
        return Ok(());
    }
    Err(ScopeError::NotExactMatch)
}

/// `net:` 域名的形态判定（上游 `netDomainPattern` 的逐字等价实现）：
/// `^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$`，长度 ≤ 253。
///
/// 与上游一致：**不**额外检查单个 label 的 63 字节上限（那是 DNS 的约束，不是本契约的），
/// 也**不**做 IDN/punycode 归一化 —— manifest 里写什么就必须与 hook URL 的 host 逐字相等。
#[must_use]
pub fn is_net_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    let mut labels = 0_usize;
    for label in domain.split('.') {
        if !is_net_domain_label(label) {
            return false;
        }
        labels += 1;
    }
    // 至少一个点：`net:localhost` 这类单 label 不是合法域名。
    labels >= 2
}

/// 单个 label：首尾必须是 `[a-z0-9]`，中间允许 `-`。
fn is_net_domain_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    if !is_lower_alnum(*first) || !is_lower_alnum(*last) {
        return false;
    }
    bytes
        .iter()
        .all(|byte| is_lower_alnum(*byte) || *byte == b'-')
}

const fn is_lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn fixed_scopes_are_closed_and_complete() {
        assert_eq!(FIXED_SCOPES.len(), 10);
        for scope in FIXED_SCOPES {
            assert!(validate_scope(scope).is_ok(), "{scope}");
        }
        assert_eq!(
            validate_scope("issues:delete").unwrap_err().code(),
            "plugin_scope_invalid"
        );
        assert_eq!(
            validate_scope("").unwrap_err().code(),
            "plugin_scope_invalid"
        );
        assert_eq!(
            validate_scope("net:").unwrap_err(),
            ScopeError::InvalidNetDomain {
                domain: String::new()
            }
        );
    }

    #[test]
    fn net_scope_accepts_and_rejects_by_pattern() {
        for ok in [
            "net:example.com",
            "net:api.example.com",
            "net:a-b.example.co",
        ] {
            assert!(validate_scope(ok).is_ok(), "{ok}");
        }
        for bad in [
            "net:localhost",     // 单 label
            "net:Example.com",   // 大写
            "net:-bad.com",      // label 以 - 开头
            "net:bad-.com",      // label 以 - 结尾
            "net:a..b",          // 空 label
            "net:.example.com",  // 前导点
            "net:example.com.",  // 尾点
            "net:example.com/x", // 路径
        ] {
            assert!(validate_scope(bad).is_err(), "{bad} should be rejected");
        }
        assert!(!is_net_domain(&"a".repeat(254)));
    }

    /// scope 判定矩阵（`DoD`：表驱动）。
    #[test]
    fn authorize_matrix() {
        let granted = owned(&["issues:read", "comments:write", "net:example.com"]);
        let cases: &[(&str, bool)] = &[
            ("issues:read", true),
            ("issues:write", false),
            ("comments:write", true),
            ("comments:read", false),
            ("storage:user", false),
            ("net:example.com", true),
            // 精确匹配：不能靠前缀/后缀吃掉别的 scope
            ("issues", false),
            ("issues:read:extra", false),
            ("net:example.com.evil", false),
            ("net:sub.example.com", false),
            // 空串 = 不需要 scope（`ContractPluginExtension` 的无 scope 路径）
            ("", true),
        ];
        for (required, expected) in cases {
            assert_eq!(
                authorize(&granted, required).is_ok(),
                *expected,
                "required={required:?}"
            );
        }
        // 空授予集不给任何 scope（**不是**全给）
        let none: Vec<String> = Vec::new();
        assert!(!is_granted(&none, "issues:read"));
        assert!(authorize(&none, "").is_ok());
        assert_eq!(
            authorize(&none, "issues:read").unwrap_err(),
            ScopeError::NotGranted {
                scope: "issues:read".to_owned()
            }
        );
    }

    #[test]
    fn granted_scopes_must_equal_manifest_scopes() {
        let manifest = owned(&["issues:read", "issues:write"]);
        assert!(validate_granted_scopes(&manifest, &manifest).is_ok());
        // 顺序不同也算相等（集合语义）
        assert!(
            validate_granted_scopes(&manifest, &owned(&["issues:write", "issues:read"])).is_ok()
        );
        // 少给
        assert_eq!(
            validate_granted_scopes(&manifest, &owned(&["issues:read"])).unwrap_err(),
            ScopeError::NotExactMatch
        );
        // 多给：报出具体那一个
        assert_eq!(
            validate_granted_scopes(
                &manifest,
                &owned(&["issues:read", "issues:write", "comments:read"])
            )
            .unwrap_err(),
            ScopeError::NotRequested {
                scope: "comments:read".to_owned()
            }
        );
    }

    #[test]
    fn net_domains_preserves_order_and_prefix_only() {
        let scopes = owned(&["net:b.com", "issues:read", "net:a.com", "net:b.com"]);
        assert_eq!(net_domains(&scopes), owned(&["b.com", "a.com", "b.com"]));
        assert!(net_domains(&owned(&["issues:read"])).is_empty());
    }
}
