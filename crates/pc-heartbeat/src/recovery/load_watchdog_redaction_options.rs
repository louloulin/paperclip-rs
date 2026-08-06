//! 从实例设置加载 watchdog evidence 脱敏选项。

use pc_repos::Db;
use serde_json::Value;

use super::redact_watchdog_evidence_text::CurrentUserRedactionOptions;

pub fn watchdog_redaction_options_from_general(
    general: &Value,
) -> Option<CurrentUserRedactionOptions> {
    let enabled = general
        .get("censorUsernameInLogs")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return None;
    }

    Some(CurrentUserRedactionOptions {
        enabled: true,
        user_names: string_array(general.get("usernames")),
        home_dirs: string_array(general.get("homeDirs")),
        replacement: None,
    })
}

pub async fn load_watchdog_redaction_options(
    db: &Db,
) -> sqlx::Result<Option<CurrentUserRedactionOptions>> {
    let general: Option<Value> = sqlx::query_scalar(
        "SELECT general FROM instance_settings WHERE singleton_key = 'singleton'",
    )
    .fetch_optional(db.pool())
    .await?;
    Ok(general
        .as_ref()
        .and_then(watchdog_redaction_options_from_general))
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn disabled_setting_returns_none() {
        assert!(watchdog_redaction_options_from_general(&json!({})).is_none());
    }

    #[test]
    fn enabled_setting_parses_string_lists() {
        let options = watchdog_redaction_options_from_general(&json!({
            "censorUsernameInLogs": true,
            "usernames": ["alice", 3],
            "homeDirs": ["/Users/alice"]
        }))
        .unwrap();
        assert_eq!(options.user_names, vec!["alice"]);
        assert_eq!(options.home_dirs, vec!["/Users/alice"]);
    }
}
