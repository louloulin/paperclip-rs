//! 请求体校验 — 对齐 Node `middleware/validate.ts`。
use serde::de::DeserializeOwned;
pub fn validate_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Value> {
    let de = &mut serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize::<_, T>(de).map_err(zod_details)
}
pub fn zod_details(err: serde_path_to_error::Error<serde_json::Error>) -> serde_json::Value {
    let path_str = err.path().to_string();
    let inner = err.into_inner();
    let msg_str = inner.to_string();
    let mut path: Vec<serde_json::Value> = path_str
        .split('.')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::Value::String(s.to_string()))
        .collect();
    let message = if inner.is_data() && msg_str.starts_with("missing field") {
        let field = msg_str.split('`').nth(1).unwrap_or("").to_string();
        path.push(serde_json::Value::String(field));
        "Required".to_string()
    } else {
        msg_str
    };
    serde_json::json!([{ "code": "invalid_type", "path": path, "message": message }])
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    #[derive(Deserialize, Debug, PartialEq)]
    struct Create {
        name: String,
        age: u32,
    }
    #[test]
    fn validates_valid_body() {
        let v: Create = validate_body(br#"{"name": "alice", "age": 30}"#).expect("valid");
        assert_eq!(
            v,
            Create {
                name: "alice".into(),
                age: 30
            }
        );
    }
    #[test]
    fn missing_field_yields_required() {
        let details = validate_body::<Create>(br#"{"name": "alice"}"#).unwrap_err();
        let arr = details.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["code"], "invalid_type");
        assert_eq!(arr[0]["path"], serde_json::json!(["age"]));
        assert_eq!(arr[0]["message"], "Required");
    }
    #[test]
    fn wrong_type_yields_invalid_type_message() {
        let details = validate_body::<Create>(br#"{"name": "alice", "age": "x"}"#).unwrap_err();
        assert_eq!(
            validate_body::<Create>(br#"{"name": "alice", "age": "x"}"#).unwrap_err()[0]["path"],
            serde_json::json!(["age"])
        );
    }
    #[test]
    fn nested_path() {
        #[derive(Deserialize, Debug)]
        struct Wrap {
            inner: Create,
        }
        let details = validate_body::<Wrap>(br#"{"inner": {}}"#).unwrap_err();
        assert_eq!(details[0]["path"], serde_json::json!(["inner", "name"]));
    }
}
