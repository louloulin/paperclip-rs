//! Multica plugin IPC protocol：JSON-RPC 2.0 over stdio。
//!
//! 与 multica `packages/plugin-sdk` 兼容；envelope 与 paperclip `pc-plugin-protocol` 同构。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub mod envelope;
pub mod manifest;
pub mod method;

pub use envelope::{ErrorCode, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
pub use manifest::PluginManifestV1;
pub use method::{Method, MethodResult};

/// 协议版本。
pub const PROTOCOL_VERSION: &str = "v1";

/// Generate a new request id.
pub fn new_request_id() -> String {
    Uuid::new_v4().to_string()
}

/// Send a JSON-RPC request over an async writer; newline-delimited.
pub async fn write_request<W, S>(writer: &mut W, method: &str, params: S) -> anyhow::Result<()>
where
    W: tokio::io::AsyncWriteExt + Unpin,
    S: Serialize,
{
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: new_request_id(),
        method: method.into(),
        params: serde_json::to_value(params)?,
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read a JSON-RPC response from an async reader.
pub async fn read_response<R: tokio::io::AsyncBufReadExt + Unpin>(
    reader: &mut R,
) -> anyhow::Result<JsonRpcResponse> {
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let resp: JsonRpcResponse = serde_json::from_str(&line)?;
    Ok(resp)
}

/// Helper: encode + dispatch a method by name.
pub fn encode_method(name: &str, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": new_request_id(),
        "method": name,
        "params": params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_stable() {
        assert_eq!(PROTOCOL_VERSION, "v1");
    }

    #[test]
    fn encode_method_includes_id() {
        let v = encode_method("initialize", serde_json::json!({}));
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "initialize");
        assert!(v["id"].is_string());
    }
}