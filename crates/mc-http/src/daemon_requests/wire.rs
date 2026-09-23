//! `daemon_requests` 的**线上形状**：内存结构 → wire JSON 的投影（R7 拆分自
//! `daemon_requests.rs`）。
//!
//! 这里只有两件事，都与「内存结构怎么变成 HTTP 响应」有关：
//!
//! - [`LocalSkillImportAction`]：本地 skill 导入动作的两个取值与它们的 wire 字符串；
//! - [`PendingRequest::to_wire`]：`POST /api/runtimes/{id}/{action}` 类路由的响应体。
//!
//! `to_wire` 的字段集取四类请求的**并集**：daemon 只读它认识的字段，多出来的
//! （`action` / `name`）在各自 kind 下才有效。两处微妙的地方：
//!
//! 1. 四个 kind 的 GET 把各自的结果**摊平**在顶层（`output` / `models` / `skills` /
//!    `skill`），所以 `result` 约定是「要摊平的对象」；
//! 2. `ModelList` / `LocalSkills` 的「无条件 bool」（`supported` / `mcp_supported`）
//!    哪怕请求还在排队也必须出现在线上（上游没有 `omitempty`），所以它们用
//!    `or_insert` 兜**排队期的缺省值**，让上报里带回来的真实值优先。两个缺省值
//!    **不同**：`supported` 缺省 `true`（上游两个 store 的 `Create` 都写
//!    `Supported: true` —— 老客户端因此不会误判「这台机器不支持选模型」），
//!    `mcp_supported` 缺省 `false`（`Create` 没扫过 MCP，不能假装支持）。
//!
//! 时间戳用 [`timestamp`]（RFC3339、秒精度、UTC `Z`），与本切片其余端点一致；
//! 上游这四处是 Go `time.Time` 的默认 marshal（纳秒精度），偏离见 `docs/32` D-3。

use super::{PendingRequest, RequestKind};
use crate::routes::daemon::scope::timestamp;
use serde_json::Value;

/// 本地 skill 导入的解析策略（upstream `LocalSkillImportAction`，`runtime_local_skills.go:41-51`）。
///
/// 只有两个取值：默认 `create`（wire 上是**空串**，带 `omitempty` 会被整个省掉）与
/// `overwrite`（按 `target_skill_id` 覆写，且仅创建者可覆写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalSkillImportAction {
    /// `""` —— 新建；同名已存在时的处置由 `supports_conflict` 决定。
    Create,
    /// `"overwrite"` —— 覆写 `target_skill_id`。
    Overwrite,
}

impl LocalSkillImportAction {
    /// wire 字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "",
            Self::Overwrite => "overwrite",
        }
    }

    /// 解析请求体里的 `action`；未知值 → `None`（调用方回 400 `invalid action`）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "" => Some(Self::Create),
            "overwrite" => Some(Self::Overwrite),
            _ => None,
        }
    }
}

impl PendingRequest {
    /// 序列化成 `POST /api/runtimes/{id}/{action}` 的 201 响应体。
    ///
    /// 上游四类 store 的 `Create` 返回体字段集不完全一致，这里取并集 —— daemon 只读
    /// 它认识的字段，多出来的（`action` / `name`）在各自 kind 下才是有效字段。
    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("id".into(), Value::String(self.id.to_string()));
        obj.insert(
            "runtime_id".into(),
            Value::String(self.runtime_id.to_string()),
        );
        obj.insert("status".into(), Value::String(self.status.wire().into()));
        obj.insert(
            "created_at".into(),
            Value::String(timestamp(self.created_at)),
        );
        if let Some(action) = &self.action {
            // 上游 `action` 带 `omitempty`，而 `create` 的 wire 值就是空串 ⇒ 空串不输出。
            if !action.is_empty() {
                obj.insert("action".into(), Value::String(action.clone()));
            }
        }
        if let Some(name) = &self.name {
            obj.insert("name".into(), Value::String(name.clone()));
        }
        if let Some(description) = &self.description {
            obj.insert("description".into(), Value::String(description.clone()));
        }
        if let Some(target) = self.target_skill_id {
            obj.insert("target_skill_id".into(), Value::String(target.to_string()));
        }
        if let Some(key) = &self.skill_key {
            obj.insert("skill_key".into(), Value::String(key.clone()));
        }
        if self.supports_conflict {
            obj.insert("supports_conflict".into(), Value::Bool(true));
        }
        if let Some(version) = &self.target_version {
            obj.insert("target_version".into(), Value::String(version.clone()));
        }
        if let Some(started) = self.started_at {
            obj.insert("started_at".into(), Value::String(timestamp(started)));
        }
        if let Some(done) = self.completed_at {
            obj.insert("completed_at".into(), Value::String(timestamp(done)));
        }
        // 四个 kind 的 GET 响应把各自的结果摊平在顶层（`output` / `models` / `skills` /
        // `skill`），所以 `result` 约定是一个「要摊平的对象」。
        if let Some(Value::Object(fields)) = &self.result {
            for (key, value) in fields {
                obj.insert(key.clone(), value.clone());
            }
        } else if let Some(result) = &self.result {
            obj.insert("result".into(), result.clone());
        }
        if let Some(error) = &self.error {
            obj.insert("error".into(), Value::String(error.clone()));
        }
        // 三个 kind 的「无条件 bool」必须始终出现在线上，哪怕请求还在排队：
        // 上游 `ModelListRequest.supported`、`RuntimeLocalSkillListRequest.supported`
        // 与 `.mcp_supported` 都没有 `omitempty`，排队期输出各自 store `Create` 写入的
        // 缺省值（`supported` = true、`mcp_supported` = false，见模块文档）。
        // 放在摊平之后用 `or_insert`，让上报里带回来的真实值优先。
        match self.kind {
            RequestKind::ModelList => {
                obj.entry("supported".to_string())
                    .or_insert(Value::Bool(true));
            }
            RequestKind::LocalSkills => {
                obj.entry("supported".to_string())
                    .or_insert(Value::Bool(true));
                obj.entry("mcp_supported".to_string())
                    .or_insert(Value::Bool(false));
            }
            RequestKind::Update | RequestKind::LocalSkillImport => {}
        }
        let updated = self
            .completed_at
            .or(self.started_at)
            .unwrap_or(self.created_at);
        obj.insert("updated_at".into(), Value::String(timestamp(updated)));
        Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_requests::RequestStore;
    use mc_core::Id;
    use serde_json::json;

    /// 排队期的模型清单请求：`supported` 必须是 `true`（上游 `Create` 的缺省值），
    /// `models` 因为 `omitempty` 不出现。
    #[test]
    fn pending_model_list_defaults_supported_true() {
        let store = RequestStore::new();
        let req = store.create_kind(RequestKind::ModelList, Id::new(), Id::new(), None);
        let wire = req.to_wire();
        assert_eq!(wire["supported"], json!(true));
        assert_eq!(wire["status"], json!("pending"));
        assert!(wire.get("models").is_none());
        assert!(wire.get("error").is_none());
        assert_eq!(wire["created_at"], wire["updated_at"]);
    }

    /// 排队期的本地技能清单：`supported` 缺省 `true`，`mcp_supported` 缺省 `false`。
    #[test]
    fn pending_local_skills_defaults_flags_distinctly() {
        let store = RequestStore::new();
        let req = store.create_kind(RequestKind::LocalSkills, Id::new(), Id::new(), None);
        let wire = req.to_wire();
        assert_eq!(wire["supported"], json!(true));
        assert_eq!(wire["mcp_supported"], json!(false));
    }

    /// 上报带回来的真实值优先于排队期缺省值。
    #[test]
    fn reported_result_wins_over_defaults() {
        let store = RequestStore::new();
        let req = store.create_kind(RequestKind::ModelList, Id::new(), Id::new(), None);
        store
            .complete(req.id, json!({ "supported": false, "models": ["m"] }))
            .expect("complete");
        let wire = store.get(req.id).expect("row").to_wire();
        assert_eq!(wire["supported"], json!(false));
        assert_eq!(wire["models"], json!(["m"]));
    }

    /// `action = create` 的 wire 值是空串 ⇒ 带 `omitempty` 的字段整个不出现。
    #[test]
    fn create_action_is_omitted_but_overwrite_is_not() {
        let store = RequestStore::new();
        let rt = Id::new();
        let ws = Id::new();
        let created = store.create_local_skill_import(
            rt,
            ws,
            None,
            "key".into(),
            LocalSkillImportAction::Create,
            None,
            None,
            None,
            false,
        );
        assert!(created.to_wire().get("action").is_none());
        let overwritten = store.create_local_skill_import(
            rt,
            ws,
            None,
            "key".into(),
            LocalSkillImportAction::Overwrite,
            Some(Id::new()),
            Some("name".into()),
            Some("desc".into()),
            true,
        );
        let wire = overwritten.to_wire();
        assert_eq!(wire["action"], json!("overwrite"));
        assert_eq!(wire["supports_conflict"], json!(true));
        assert_eq!(wire["name"], json!("name"));
        assert!(wire.get("target_skill_id").is_some());
    }
}
