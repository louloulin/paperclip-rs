#![forbid(unsafe_code)]

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult, UsageSummary,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};

pub const ADAPTER_TYPE: &str = "grok_local";

fn default_command(config: &serde_json::Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("grok")
        .to_owned()
}

fn default_model(config: &serde_json::Value) -> Option<String> {
    config
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

/// Parse adapter-specific JSONL or text output from stdout.
fn parse_stdout(stdout: &str) -> Option<String> {
    // Find the last non-empty line that looks like useful output
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Try JSONL parsing for structured events
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_owned());
            }
            if let Some(item) = event.get("item").and_then(|v| v.get("text")).and_then(|v| v.as_str()) {
                return Some(item.to_owned());
            }
        }
        return Some(trimmed.to_owned());
    }
    None
}

pub struct adapter_grok_localAdapter;

impl adapter_grok_localAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for adapter_grok_localAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for adapter_grok_localAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, "Grok Build")
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let model = default_model(&context.adapter_config);
        let mut args: Vec<String> = context
            .adapter_config
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // Standard CLI invocation: <command> [args] <prompt via stdin>
        let spec = ProcessSpec::new(&command, &args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
        let summary = parse_stdout(&execution.stdout);

        let mut result = execution.result;
        result.provider = Some("grok_local".into());
        result.model = model;
        result.summary = summary;
        result.error_message = (result.exit_code != Some(0))
            .then(|| execution.stderr.trim().to_owned())
            .filter(|s| !s.is_empty());

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_returns_correct_type() {
        let adapter = adapter_grok_localAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, "grok_local");
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        let config = serde_json::json!({});
        assert_eq!(default_command(&config), "grok");
    }

    #[test]
    fn default_command_reads_config() {
        let config = serde_json::json!({"command": "/custom/path"});
        assert_eq!(default_command(&config), "/custom/path");
    }

    #[test]
    fn parse_stdout_returns_last_useful_line() {
        let output = "line1\nline2\nhello world\n";
        assert_eq!(parse_stdout(output), Some("hello world".into()));
    }

    #[test]
    fn parse_stdout_handles_jsonl() {
        let output = r#"{"type":"item.completed","item":{"type":"agent_message","text":"Done"}}"#;
        assert_eq!(parse_stdout(output), Some("Done".into()));
    }

    #[test]
    fn parse_stdout_empty_returns_none() {
        assert_eq!(parse_stdout(""), None);
    }
}
