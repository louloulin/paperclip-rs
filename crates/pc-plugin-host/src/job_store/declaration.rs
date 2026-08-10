//! Plugin manifest job declaration 输入 DTO —— 与 Node 1:1 对齐。

use serde::{Deserialize, Serialize};

use super::types::{JobRunStatus, JobRunTrigger};

/// Manifest 中声明的一个 scheduled job。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginJobDeclaration {
    pub job_key: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
}

impl PluginJobDeclaration {
    pub fn new(job_key: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            job_key: job_key.into(),
            display_name: display_name.into(),
            description: None,
            schedule: None,
        }
    }

    pub fn schedule_or_empty(&self) -> &str {
        self.schedule.as_deref().unwrap_or("")
    }
}

/// 创建 job run 的输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobRunInput {
    pub job_id: String,
    pub plugin_id: String,
    pub trigger: JobRunTrigger,
}

/// 完成 job run 的输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteJobRunInput {
    pub status: JobRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r729_declaration_new_minimal() {
        let d = PluginJobDeclaration::new("job_a", "Job A");
        assert_eq!(d.job_key, "job_a");
        assert_eq!(d.display_name, "Job A");
        assert!(d.description.is_none());
        assert!(d.schedule.is_none());
        assert_eq!(d.schedule_or_empty(), "");
    }

    #[test]
    fn r729_declaration_schedule_or_empty() {
        let mut d = PluginJobDeclaration::new("a", "A");
        d.schedule = Some("0 * * * *".to_string());
        assert_eq!(d.schedule_or_empty(), "0 * * * *");
        d.schedule = None;
        assert_eq!(d.schedule_or_empty(), "");
    }

    #[test]
    fn r729_declaration_camel_case_serialize() {
        let d = PluginJobDeclaration::new("a", "A");
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["jobKey"], "a");
        assert_eq!(v["displayName"], "A");
        assert!(v.get("description").is_none());
        assert!(v.get("schedule").is_none());
    }

    #[test]
    fn r729_create_run_input_camel_case() {
        let i = CreateJobRunInput {
            job_id: "j1".into(),
            plugin_id: "p1".into(),
            trigger: JobRunTrigger::Manual,
        };
        let v = serde_json::to_value(&i).unwrap();
        assert_eq!(v["jobId"], "j1");
        assert_eq!(v["pluginId"], "p1");
        assert_eq!(v["trigger"], "manual");
    }

    #[test]
    fn r729_complete_run_input_camel_case() {
        let i = CompleteJobRunInput {
            status: JobRunStatus::Failed,
            error: Some("boom".into()),
            duration_ms: Some(123),
        };
        let v = serde_json::to_value(&i).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(v["error"], "boom");
        assert_eq!(v["durationMs"], 123);
    }

    #[test]
    fn r729_complete_run_input_omits_optional_fields() {
        let i = CompleteJobRunInput {
            status: JobRunStatus::Succeeded,
            error: None,
            duration_ms: None,
        };
        let v = serde_json::to_value(&i).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert!(v.get("error").is_none());
        assert!(v.get("durationMs").is_none());
    }

    #[test]
    fn r729_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginJobDeclaration>();
        assert_send_sync::<CreateJobRunInput>();
        assert_send_sync::<CompleteJobRunInput>();
    }

    #[test]
    fn r729_trigger_json_round_trip() {
        let i = CreateJobRunInput {
            job_id: "j".into(),
            plugin_id: "p".into(),
            trigger: JobRunTrigger::Schedule,
        };
        let s = serde_json::to_string(&i).unwrap();
        let parsed: CreateJobRunInput = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.trigger, JobRunTrigger::Schedule);
    }

    #[test]
    fn r729_declaration_from_json() {
        let v = json!({
            "jobKey": "nightly",
            "displayName": "Nightly Run",
            "description": "Run nightly",
            "schedule": "0 0 * * *"
        });
        let d: PluginJobDeclaration = serde_json::from_value(v).unwrap();
        assert_eq!(d.job_key, "nightly");
        assert_eq!(d.display_name, "Nightly Run");
        assert_eq!(d.description.as_deref(), Some("Run nightly"));
        assert_eq!(d.schedule.as_deref(), Some("0 0 * * *"));
    }
}
