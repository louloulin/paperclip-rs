//! Paperclip 插件协议错误码。
//!
//! 与原 `@paperclipai/plugin-sdk` 的 `protocol.ts` 中错误码定义等价。

use serde::{Deserialize, Serialize};

/// 标准 JSON-RPC 2.0 错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStandardErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
}

/// Paperclip 插件协议自定义错误码。
///
/// 范围：-32000 ~ -32099（Server Error 区段）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginErrorCode {
    /// 插件 manifest 加载失败
    ManifestLoadFailed = -32001,
    /// 插件配置无效
    ConfigInvalid = -32002,
    /// 插件版本不兼容
    IncompatibleVersion = -32003,
    /// 插件没有所需 capability
    CapabilityDenied = -32004,
    /// 作业执行超时
    JobTimeout = -32005,
    /// 作业执行失败
    JobFailed = -32006,
    /// 资源访问被拒绝（policy gate）
    AccessDenied = -32007,
    /// webhook 投递失败
    WebhookDeliveryFailed = -32008,
    /// 数据查询失败
    DataQueryFailed = -32009,
    /// Action 执行失败
    ActionFailed = -32010,
    /// Tool 执行失败
    ToolExecutionFailed = -32011,
    /// Health check 失败
    HealthCheckFailed = -32012,
    /// 插件未 ready / 未 initialized
    NotReady = -32013,
    /// 数据库桥接失败
    DatabaseBridgeFailed = -32014,
    /// 限流
    RateLimited = -32015,
}

impl PluginErrorCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_error_codes_are_stable() {
        assert_eq!(PluginErrorCode::ManifestLoadFailed.as_i32(), -32001);
        assert_eq!(PluginErrorCode::IncompatibleVersion.as_i32(), -32003);
        assert_eq!(PluginErrorCode::CapabilityDenied.as_i32(), -32004);
        assert_eq!(PluginErrorCode::JobTimeout.as_i32(), -32005);
    }

    #[test]
    fn plugin_error_codes_in_server_error_range() {
        assert!(PluginErrorCode::ManifestLoadFailed.as_i32() < -32000);
        assert!(PluginErrorCode::ManifestLoadFailed.as_i32() >= -32100);
    }
}
