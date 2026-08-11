use serde_json::{Number, Value};

pub fn canonical(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => canonical_number(value),
        Value::String(value) => serde_json::to_string(value).expect("JSON string serialization"),
        Value::Array(values) => {
            let body = values.iter().map(canonical).collect::<Vec<_>>().join(",");
            format!("[{body}]")
        }
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let body = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = serde_json::to_string(key).expect("JSON key serialization");
                    format!("{key}:{}", canonical(value))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
    }
}

pub fn canonical_number(value: &Number) -> String {
    let value = value
        .as_f64()
        .expect("serde_json numbers are representable as JavaScript numbers");
    let mut buffer = ryu_js::Buffer::new();
    buffer.format(value).to_string()
}
