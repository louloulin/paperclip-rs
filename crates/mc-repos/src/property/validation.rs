//! property 定义面的**纯校验**（无 IO）：与上游 `handler/property.go` +
//! `internal/issueproperty/value.go` 逐条对齐。
//!
//! 从 `property.rs` 拆出（R7 单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩）；
//! 兄弟子模块形态与 `property/tests.rs` 相同。`property.rs` 用
//! `pub use self::validation::*;` 重新导出，对外面（路由层 / 测试）的引用路径不变。

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as JsonValue};
use uuid::Uuid;

use super::{
    ACTOR_KINDS, MAX_ACTOR_VALUES, MAX_DESCRIPTION_LEN, MAX_ICON_LEN, MAX_NAME_LEN,
    MAX_PROPERTIES_BAG_BYTES, MAX_SELECT_OPTIONS, MAX_TEXT_VALUE_LEN, MAX_URL_VALUE_LEN,
    PROPERTY_ICONS, PROPERTY_TYPES, RESERVED_NAMES,
};

/// 规范化定义名（上游 `normalizePropertyName`）：小写 → 去首尾空白 → 空格换下划线。
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase().replace(' ', "_")
}

/// 定义名校验（上游 `validatePropertyName`）：控制字符 → 去空白 → 非空 → ≤32 → 非保留名。
pub fn validate_name(raw: &str) -> std::result::Result<String, String> {
    if raw.chars().any(char::is_control) {
        return Err("name cannot contain tabs, newlines, or control characters".into());
    }
    let name = raw.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!("name must be {MAX_NAME_LEN} characters or fewer"));
    }
    if RESERVED_NAMES.contains(&normalize_name(name).as_str()) {
        return Err(format!("{name:?} is reserved for a built-in issue field"));
    }
    Ok(name.to_string())
}

/// 图标校验（上游 `validatePropertyIcon`）：控制字符 → 去空白 → ≤32 → 空串合法 → 白名单。
pub fn validate_icon(raw: &str) -> std::result::Result<String, String> {
    if raw.chars().any(char::is_control) {
        return Err("icon cannot contain tabs, newlines, or control characters".into());
    }
    let icon = raw.trim();
    if icon.chars().count() > MAX_ICON_LEN {
        return Err(format!("icon must be {MAX_ICON_LEN} characters or fewer"));
    }
    if icon.is_empty() {
        return Ok(String::new());
    }
    if !PROPERTY_ICONS.contains(&icon) {
        return Err("icon must be a supported icon key".into());
    }
    Ok(icon.to_string())
}

/// 类型校验（上游 `validatePropertyType`）。
pub fn validate_type(property_type: &str) -> std::result::Result<(), String> {
    if PROPERTY_TYPES.contains(&property_type) {
        return Ok(());
    }
    Err(format!(
        "invalid type {property_type:?}; valid types: {}",
        PROPERTY_TYPES.join(", ")
    ))
}

/// `select` / `multi_select` 是否带选项。
pub fn type_has_options(property_type: &str) -> bool {
    matches!(property_type, "select" | "multi_select")
}

/// 定义是否按 actor 引用解析（上游 `IsActor`）。
pub fn type_is_actor(property_type: &str) -> bool {
    matches!(property_type, "actor" | "multi_actor")
}

/// 描述长度校验（上游两处同名判断）。
pub fn validate_description(raw: &str) -> std::result::Result<(), String> {
    if raw.chars().count() > MAX_DESCRIPTION_LEN {
        return Err(format!(
            "description must be {MAX_DESCRIPTION_LEN} characters or fewer"
        ));
    }
    Ok(())
}

/// 选项（上游 `PropertyOption`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyOption {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// 配置（上游 `PropertyConfig`；非 select 类型序列化成 `{}`）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PropertyConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<PropertyOption>,
}

/// 解析存储的 config（坏数据 → 空配置，同上游 `parsePropertyConfig`）。
pub fn parse_config(raw: &JsonValue) -> PropertyConfig {
    serde_json::from_value(raw.clone()).unwrap_or_default()
}

/// config 规范化（上游 `validatePropertyConfig`）：select 类 1..=50 选项、缺 id 由服务端补
/// UUID；非 select 类必须无选项且存成 `{}`。
pub fn validate_config(
    property_type: &str,
    config: Option<&PropertyConfig>,
) -> std::result::Result<JsonValue, String> {
    if !type_has_options(property_type) {
        if config.is_some_and(|c| !c.options.is_empty()) {
            return Err(format!("type {property_type:?} does not accept options"));
        }
        return Ok(json!({}));
    }
    let input = config.cloned().unwrap_or_default();
    if input.options.is_empty() {
        return Err("select properties require at least one option".into());
    }
    if input.options.len() > MAX_SELECT_OPTIONS {
        return Err(format!(
            "a property cannot have more than {MAX_SELECT_OPTIONS} options"
        ));
    }
    let mut seen_ids: Vec<String> = Vec::with_capacity(input.options.len());
    let mut seen_names: Vec<String> = Vec::with_capacity(input.options.len());
    let mut options: Vec<PropertyOption> = Vec::with_capacity(input.options.len());
    for opt in &input.options {
        let name = crate::label::validate_name(&opt.name).map_err(|e| format!("option {e}"))?;
        let lower = name.to_lowercase();
        if seen_names.contains(&lower) {
            return Err(format!("duplicate option name {name:?}"));
        }
        seen_names.push(lower);
        let color = crate::label::normalize_color(&opt.color)
            .map_err(|e| format!("option {name:?}: {e}"))?;
        let id = opt.id.trim().to_string();
        let id = if id.is_empty() {
            Uuid::new_v4().to_string()
        } else if Uuid::parse_str(&id).is_err() {
            return Err(format!("option {name:?}: id must be a UUID"));
        } else {
            id
        };
        if seen_ids.contains(&id) {
            return Err(format!("duplicate option id {id:?}"));
        }
        seen_ids.push(id.clone());
        options.push(PropertyOption { id, name, color });
    }
    serde_json::to_value(PropertyConfig { options }).map_err(|e| e.to_string())
}

/// 选项 id → 下标（顺序即展示顺序）。
pub(crate) fn option_order(config: &PropertyConfig) -> Vec<(String, usize)> {
    config
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| (o.id.clone(), i))
        .collect()
}

/// `id (<name>), ...`（上游 `selectOptionsHint`）。
pub(crate) fn options_hint(config: &PropertyConfig) -> String {
    config
        .options
        .iter()
        .map(|o| format!("{} ({})", o.id, o.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 去掉 `\0`（上游 `util.SanitizeTextForPostgres`）。
pub(crate) fn sanitize_null_bytes(text: &str) -> String {
    text.replace('\0', "")
}

/// actor 引用（`member:<uuid>`）解析（上游 `ParseActorRef`）。
pub fn parse_actor_ref(value: &str) -> std::result::Result<String, String> {
    let Some((kind, id)) = value.split_once(':') else {
        return Err(format!(
            "value must look like \"<kind>:<uuid>\" where kind is one of: {}",
            ACTOR_KINDS.join(" / ")
        ));
    };
    if !ACTOR_KINDS.contains(&kind) {
        return Err(format!(
            "unknown actor kind {kind:?}; valid kinds: {}",
            ACTOR_KINDS.join(" / ")
        ));
    }
    let Ok(parsed) = Uuid::parse_str(id) else {
        return Err(format!("actor id in {value:?} must be a UUID"));
    };
    Ok(format!("{kind}:{parsed}"))
}

/// 值里出现的 actor 引用（已通过 [`validate_value`] ⇒ 只做提取）。
pub fn actor_refs_in_value(property_type: &str, value: &JsonValue) -> Vec<String> {
    match property_type {
        "actor" => value
            .as_str()
            .and_then(|s| parse_actor_ref(s).ok())
            .map(|s| vec![s])
            .unwrap_or_default(),
        "multi_actor" => value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().and_then(|s| parse_actor_ref(s).ok()))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 值校验 + 规范化（上游 `issueproperty.ValidateValue` 的逐条移植）。
///
/// 返回可直接落库的规范 JSON。错误消息**逐字**对齐上游，因为 agent 直接消费它们自我纠正。
#[allow(clippy::too_many_lines)] // 九种类型各一条规则，按上游顺序平铺；拆成子函数反而看不出「同一份判定」
pub fn validate_value(
    property_type: &str,
    config: &JsonValue,
    raw: &JsonValue,
) -> std::result::Result<JsonValue, String> {
    if raw.is_null() {
        return Err("value cannot be null (use DELETE to unset a property)".into());
    }
    let config = parse_config(config);
    match property_type {
        "text" => {
            let Some(text) = raw.as_str() else {
                return Err("value must be a string".into());
            };
            if text.trim().is_empty() {
                return Err("value cannot be empty (use DELETE to unset a property)".into());
            }
            if text.chars().count() > MAX_TEXT_VALUE_LEN {
                return Err(format!(
                    "value must be {MAX_TEXT_VALUE_LEN} characters or fewer"
                ));
            }
            Ok(json!(sanitize_null_bytes(text)))
        }
        "url" => {
            let Some(text) = raw.as_str() else {
                return Err("value must be a URL string".into());
            };
            let text = text.trim();
            if text.len() > MAX_URL_VALUE_LEN {
                return Err(format!(
                    "value must be {MAX_URL_VALUE_LEN} characters or fewer"
                ));
            }
            if !is_http_url(text) {
                return Err("value must be an http(s) URL".into());
            }
            Ok(json!(text))
        }
        "number" => {
            if !raw.is_number() {
                return Err("value must be a number".into());
            }
            Ok(raw.clone())
        }
        "checkbox" => {
            if !raw.is_boolean() {
                return Err("value must be true or false".into());
            }
            Ok(raw.clone())
        }
        "date" => {
            let Some(text) = raw.as_str() else {
                return Err("value must be a date string in YYYY-MM-DD format".into());
            };
            if !is_yyyy_mm_dd(text) {
                return Err("value must be a date string in YYYY-MM-DD format".into());
            }
            Ok(json!(text))
        }
        "select" => {
            let hint = options_hint(&config);
            let Some(id) = raw.as_str() else {
                return Err(format!("value must be one of the option ids: {hint}"));
            };
            if !option_order(&config).iter().any(|(known, _)| known == id) {
                return Err(format!("value must be one of the option ids: {hint}"));
            }
            Ok(json!(id))
        }
        "multi_select" => {
            let hint = options_hint(&config);
            let order = option_order(&config);
            let Some(items) = raw.as_array() else {
                return Err(format!(
                    "value must be a non-empty array of option ids: {hint}"
                ));
            };
            if items.is_empty() {
                return Err(format!(
                    "value must be a non-empty array of option ids: {hint}"
                ));
            }
            let mut ids: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                let Some(id) = item.as_str() else {
                    return Err(format!(
                        "value must be a non-empty array of option ids: {hint}"
                    ));
                };
                if !order.iter().any(|(known, _)| known == id) {
                    return Err(format!(
                        "unknown option id {id:?}; valid option ids: {hint}"
                    ));
                }
                if !ids.iter().any(|known| known == id) {
                    ids.push(id.to_string());
                }
            }
            // 稳定排序回定义里的选项顺序（上游 `sort.SliceStable`）。
            ids.sort_by_key(|id| {
                order
                    .iter()
                    .find(|(known, _)| known == id)
                    .map_or(usize::MAX, |(_, i)| *i)
            });
            Ok(json!(ids))
        }
        "actor" => {
            let Some(text) = raw.as_str() else {
                return Err(format!(
                    "value must be an actor reference string like \"member:<uuid>\" (kinds: {})",
                    ACTOR_KINDS.join(" / ")
                ));
            };
            Ok(json!(parse_actor_ref(text)?))
        }
        "multi_actor" => {
            let Some(items) = raw.as_array() else {
                return Err(format!(
                    "value must be an array of actor reference strings like \"member:<uuid>\" (kinds: {})",
                    ACTOR_KINDS.join(" / ")
                ));
            };
            if items.is_empty() {
                return Err("value must be a non-empty array of actor references".into());
            }
            if items.len() > MAX_ACTOR_VALUES {
                return Err(format!(
                    "value cannot list more than {MAX_ACTOR_VALUES} actors"
                ));
            }
            let mut refs: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err("value must be an array of actor reference strings".into());
                };
                let reference = parse_actor_ref(text)?;
                if !refs.contains(&reference) {
                    refs.push(reference);
                }
            }
            Ok(json!(refs))
        }
        other => Err(format!("unsupported property type {other:?}")),
    }
}

/// `http(s)://` + 非空 host（上游用 `net/url` 解析；本仓不引入 `url` crate，
/// 只做 scheme + authority 非空判定，见 `docs/59` §4 偏差表）。
fn is_http_url(text: &str) -> bool {
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"));
    let Some(rest) = rest else {
        return false;
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    !authority.is_empty()
}

/// 严格 `YYYY-MM-DD`（chrono 的 `%m` 接受 1 位数字，故先按形状判定，与 Go 的
/// `time.Parse("2006-01-02", ...)` 对齐）。
fn is_yyyy_mm_dd(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
}

/// 值袋大小（上游 DB CHECK `pg_column_size(properties) <= 16384` 的前置近似判定）。
pub fn bag_exceeds_limit(bag: &JsonValue) -> bool {
    serde_json::to_string(bag).is_ok_and(|s| s.len() > MAX_PROPERTIES_BAG_BYTES)
}

/// 把某个 key 写进值袋（返回新袋；非对象袋按空袋处理）。
pub fn merge_bag(bag: &JsonValue, key: &str, value: &JsonValue) -> JsonValue {
    let mut map = match bag {
        JsonValue::Object(map) => map.clone(),
        _ => Map::new(),
    };
    map.insert(key.to_string(), value.clone());
    JsonValue::Object(map)
}
