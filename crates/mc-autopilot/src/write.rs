//! autopilot 写面（create / update / delete + 规则版本）。
//!
//! - **写者**：M5-2。
//! - **上游**：`CreateAutopilot`159 / `UpdateAutopilot`260 / `DeleteAutopilot`64 /
//!   `autopilotRuleSubstantiveChange`13 / `recordAutopilotRuleVersion`4 / `parseAutopilotProjectID`23
//!   （`handler/autopilot.go`）+ `db/queries/autopilot.sql`810 / 58 查询。
//! - **`UpdateAutopilot` 是三态补丁大户**：缺失 / `null` / 有值必须区分
//!   （`mc-http` 侧用 `Option<Option<T>>`，见 `routes/issues/mod.rs` 的 `#![allow(clippy::option_option)]`）。
//! - **规则版本 append-only**（`186_autopilot_rule_version`，MUL-4302）：只有
//!   `autopilotRuleSubstantiveChange` 判为「实质变更」才 append 一行；不是每次 update 都写。
//! - **列口径提醒**：`autopilot` 表**没有** `priority` / `concurrency_policy`
//!   （分别被 `058` 与 `043` 删掉，见 `mc_core::autopilot` 的「旧 stub 错在哪」表）⇒
//!   「跳过派发」的真值只能从别处推（M5-4 的 `shouldSkipDispatch`），不要按旧桩字段写。
//!
//! # M5-2 落地了什么
//!
//! 本文件放**纯函数**（不碰 DB）：写面契约里那三条判定 —— 执行模式白名单、`issue_title_template`
//! 校验、规则版本的实质变更判定 + 快照体。仓储（SQL）在 `mc_repos::autopilot::write`，
//! HTTP 面在 `mc_http::routes::autopilots::{crud,subscribers}`。
//!
//! | 上游 | 本地 |
//! | --- | --- |
//! | `req.ExecutionMode` 的两条 400 | [`is_valid_execution_mode`] |
//! | `service.ValidateIssueTitleTemplate`(18) + `isSupportedIssueTitleVariable` | [`validate_issue_title_template`] |
//! | `autopilotRuleSubstantiveChange`(13) | [`substantive_change`] |
//! | `service.RecordAutopilotRuleVersion`(79) 的 `config_summary` | [`rule_config_summary`] |
//!
//! **为什么不引 `regex`**：`Cargo.toml` 的依赖在 M5-0 anchor 一次声明到位，此后各切片不得再加
//! 三方依赖（`crates/mc-autopilot/Cargo.toml` 头注释）。上游那条 `\{\{\s*([^{}]*?)\s*\}\}` 的
//! 语义（只认 `{{`…`}}`、花括号内不许再有花括号、两端空白丢掉）用一次手写扫描逐字复刻，
//! 见 [`validate_issue_title_template`] 的注释与用例。

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::AutopilotError;
use mc_repos::autopilot::AutopilotRow;

/// 上游 `execution_mode` 的两个取值之一：跑完让 agent 建 issue（有审计轨）。
pub const EXECUTION_MODE_CREATE_ISSUE: &str = "create_issue";

/// 上游 `execution_mode` 的两个取值之一：直接派 agent 任务，不建 issue。
pub const EXECUTION_MODE_RUN_ONLY: &str = "run_only";

/// `execution_mode` 的白名单（上游 `CreateAutopilot` 的两条 400 用的就是这两个值）。
pub const EXECUTION_MODES: [&str; 2] = [EXECUTION_MODE_CREATE_ISSUE, EXECUTION_MODE_RUN_ONLY];

/// `autopilot_rule_version.config_summary` 里带的四个键（上游 `autopilotRuleConfigSummary` 的结构体标签）。
///
/// 顺序即上游结构体字段顺序；jsonb 落库后不保序，写出来只为文档。
pub const RULE_CONFIG_SUMMARY_KEYS: [&str; 4] =
    ["assignee_type", "assignee_id", "status", "execution_mode"];

/// 上游 `execution_mode ∈ {create_issue, run_only}`（`CreateAutopilot` 的两条 400）。
///
/// 注意**没有**「空串 = 默认值」这一档：空串在上游先被 `execution_mode is required` 拦掉，
/// 因此默认值只存在于「没传这个字段」的语义里（JSON 里 `execution_mode` 缺失 = 空串）。
#[must_use]
pub fn is_valid_execution_mode(raw: &str) -> bool {
    EXECUTION_MODES.contains(&raw)
}

/// 上游 `service.ValidateIssueTitleTemplate`（18 行）+ `isSupportedIssueTitleVariable`（5 行）。
///
/// 契约三条，逐条复刻：
///
/// 1. **空模板合法**（空串 = 回退到 autopilot 自己的 title，不是错误）；
/// 2. 只认 `{{date}}`（`SupportedIssueTitleTemplateVariables = ["date"]`），大小写敏感；
/// 3. 报第一条非法 token：`unknown template variable "x"; supported: {{date}}`
///    （`%q` 就是带引号的字面量；本文用 `{name:?}` —— 对普通名字与 Go 的 `%q` 同形）。
///
/// 手写扫描对上游正则 `\{\{\s*([^{}]*?)\s*\}\}` 的等价性（`regex` 不在依赖表里，见模块文档）：
/// 正则是**从左到右找第一个匹配**，匹配体是 `{{` + 前导空白 + **不含花括号的短串** + 尾部空白 + `}}`；
/// 由于 `[^{}]*?` 是惰性的、只有末尾的 `\s*` 是贪婪的，等价于「`{{` 之后取到下一个花括号为止的
/// 一段、两端去空白，再要求紧跟 `}}`」。※ 关键推论：**失配时前进一个字符重扫**（不是两个），
/// 所以 `{{{{date}}` 会在第 3 个 `{` 处匹配成功 —— 上游亦然。
pub fn validate_issue_title_template(template: &str) -> Result<(), AutopilotError> {
    if template.is_empty() {
        return Ok(());
    }
    let chars: Vec<char> = template.chars().collect();
    let mut index = 0;
    while index + 1 < chars.len() {
        if chars[index] != '{' || chars[index + 1] != '{' {
            index += 1;
            continue;
        }
        // `{{` 之后：跳过前导空白，再吃到第一个花括号（`[^{}]*?`）。
        let mut cursor = index + 2;
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < chars.len() && chars[cursor] != '{' && chars[cursor] != '}' {
            cursor += 1;
        }
        // 末尾空白不属于名字（正则里 `([^{}]*?)\s*`）。
        let mut name_end = cursor;
        while name_end > name_start && chars[name_end - 1].is_whitespace() {
            name_end -= 1;
        }
        let closed = cursor + 1 < chars.len() && chars[cursor] == '}' && chars[cursor + 1] == '}';
        if closed {
            let name: String = chars[name_start..name_end].iter().collect();
            if !is_supported_issue_title_variable(&name) {
                return Err(AutopilotError::validation(format!(
                    "unknown template variable {name:?}; supported: {{{{date}}}}"
                )));
            }
            index = cursor + 2;
            continue;
        }
        // 失配：上游正则从下一个字符继续找（RE2 的 `FindAll` 语义），不是跳过 `{{`。
        index += 1;
    }
    Ok(())
}

/// 上游 `isSupportedIssueTitleVariable`（当前只有 `date`）。
#[must_use]
pub fn is_supported_issue_title_variable(name: &str) -> bool {
    name == "date"
}

/// 上游 `autopilotRuleSubstantiveChange`(13)：这条变更是否**实质**到要重发规则版本。
///
/// 判据是 `autopilot` **行**上的六列（不是请求字段）—— 上游注释把边界钉死为「改变**做什么**、
/// **谁**做、**是否**在做」：
///
/// ```text
/// assignee_type / assignee_id   谁执行（agent / squad）
/// status                        开关（active / paused / archived）
/// execution_mode                run_only vs create_issue
/// description                   上游把它当 run 的 PROMPT（任务指令本身）
/// issue_title_template          create_issue 模式产出 issue 的标题模板
/// ```
///
/// **故意不算实质**的两列：`title`（展示名）与 `project_id`（产出 issue 归档到哪个项目）。
///
/// 之所以能直接比「行 vs 行」而不必逐字段推请求：上游 `UpdateAutopilot` 的 `params` 全部以
/// `prev` 打底，未出现的字段原样回写 ⇒ 行级 diff 与字段级 diff 等价（本地由
/// `mc_repos::autopilot::write::UpdateAutopilot` 的 COALESCE 补丁 + handler 回填保证）。
/// 归档（`DeleteAutopilot`）走的是 status 变化，天然命中本判定。
#[must_use]
pub fn substantive_change(prev: &AutopilotRow, next: &AutopilotRow) -> bool {
    prev.assignee_type != next.assignee_type
        || prev.assignee_id != next.assignee_id
        || prev.status != next.status
        || prev.execution_mode != next.execution_mode
        || prev.description != next.description
        || prev.issue_title_template != next.issue_title_template
}

/// 上游 `service.autopilotRuleConfigSummary` 的 JSON 体（`RecordAutopilotRuleVersion` 的载荷）。
///
/// 四个键固定：`assignee_type` / `assignee_id`（**规范 UUID 串**，不是 `{hi,lo}` 也不是大写形态）/
/// `status` / `execution_mode`。`title` / `description` / `issue_title_template` 与 trigger 配置
/// **故意不在**这里（改它们不转移责任）。
#[must_use]
pub fn rule_config_summary(row: &AutopilotRow) -> Value {
    json!({
        "assignee_type": row.assignee_type,
        "assignee_id": row.assignee_id.to_string(),
        "status": row.status,
        "execution_mode": row.execution_mode,
    })
}

/// 规则版本快照的四列（`rule_config_summary` 的强类型形态，测试与 M5-4 的 `rule_owner` 读回用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleConfigSummary {
    /// `agent` / `squad`（原样，不做空串兜底 —— 兜底是**响应**映射的事）。
    pub assignee_type: String,
    /// 多态引用（`assignee_type` 决定它指向哪张表）。
    pub assignee_id: Uuid,
    /// `active` / `paused` / `archived`。
    pub status: String,
    /// `create_issue` / `run_only`。
    pub execution_mode: String,
}

impl RuleConfigSummary {
    /// 从 autopilot 行取四列（与 [`rule_config_summary`] 同源，供 M5-4 读回快照时用）。
    #[must_use]
    pub fn from_row(row: &AutopilotRow) -> Self {
        Self {
            assignee_type: row.assignee_type.clone(),
            assignee_id: row.assignee_id,
            status: row.status.clone(),
            execution_mode: row.execution_mode.clone(),
        }
    }

    /// 折成落库的 JSON 体（键与顺序同上游结构体）。
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "assignee_type": self.assignee_type,
            "assignee_id": self.assignee_id.to_string(),
            "status": self.status,
            "execution_mode": self.execution_mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    /// 造一行 autopilot（只填判定用得到的列）。
    fn row() -> AutopilotRow {
        AutopilotRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            title: "t".into(),
            description: Some("prompt".into()),
            project_id: None,
            assignee_type: "agent".into(),
            assignee_id: Uuid::new_v4(),
            status: "active".into(),
            pause_reason: None,
            execution_mode: "run_only".into(),
            issue_title_template: None,
            created_by_type: "member".into(),
            created_by_id: Uuid::new_v4(),
            last_run_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn execution_mode_whitelist() {
        assert!(is_valid_execution_mode("create_issue"));
        assert!(is_valid_execution_mode("run_only"));
        // 空串与大小写变体都不合法（上游的 400 分支）。
        assert!(!is_valid_execution_mode(""));
        assert!(!is_valid_execution_mode("CREATE_ISSUE"));
        assert!(!is_valid_execution_mode("create-issue"));
    }

    #[test]
    fn empty_title_template_is_valid() {
        assert!(validate_issue_title_template("").is_ok());
        assert!(validate_issue_title_template("no tokens here").is_ok());
    }

    #[test]
    fn supported_variable_passes_with_surrounding_whitespace() {
        for tmpl in [
            "{{date}}",
            "{{ date }}",
            "Issue @ {{date}}",
            "{{  date  }}!",
        ] {
            assert!(validate_issue_title_template(tmpl).is_ok(), "{tmpl}");
        }
    }

    #[test]
    fn unknown_variable_names_the_first_offender() {
        let err = validate_issue_title_template("{{date}} {{nope}}").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown template variable \"nope\"; supported: {{date}}"
        );
        assert_eq!(err.http_status(), 400);
    }

    /// `{{}}` 的名字是空串 ⇒ 非法（上游 `unknown template variable ""`）。
    #[test]
    fn empty_token_is_rejected() {
        let err = validate_issue_title_template("{{}}").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown template variable \"\"; supported: {{date}}"
        );
    }

    /// 花括号里再带花括号：正则从下一个字符重扫 ⇒ 第 3 个 `{` 处命中。
    #[test]
    fn nested_braces_rescan_from_next_character() {
        assert!(validate_issue_title_template("{{{{date}}").is_ok());
        // 单层 `{date}` 不是 token（少了第二个花括号），整体放行。
        assert!(validate_issue_title_template("{date}").is_ok());
        // 未闭合的 `{{date` 也放行（正则无匹配）。
        assert!(validate_issue_title_template("{{date").is_ok());
    }

    #[test]
    fn substantive_change_covers_six_columns() {
        let prev = row();
        assert!(!substantive_change(&prev, &prev.clone()));

        let mut display_only = prev.clone();
        display_only.title = "other".into();
        display_only.project_id = Some(Uuid::new_v4());
        assert!(
            !substantive_change(&prev, &display_only),
            "title / project_id 是展示与归档，不算实质变更"
        );

        for (name, mutate) in [
            (
                "assignee_type",
                Box::new(|r: &mut AutopilotRow| r.assignee_type = "squad".into())
                    as Box<dyn Fn(&mut AutopilotRow)>,
            ),
            (
                "assignee_id",
                Box::new(|r: &mut AutopilotRow| r.assignee_id = Uuid::new_v4()),
            ),
            (
                "status",
                Box::new(|r: &mut AutopilotRow| r.status = "paused".into()),
            ),
            (
                "execution_mode",
                Box::new(|r: &mut AutopilotRow| r.execution_mode = "create_issue".into()),
            ),
            (
                "description",
                Box::new(|r: &mut AutopilotRow| r.description = Some("new".into())),
            ),
            (
                "description->None",
                Box::new(|r: &mut AutopilotRow| r.description = None),
            ),
            (
                "issue_title_template",
                Box::new(|r: &mut AutopilotRow| r.issue_title_template = Some("{{date}}".into())),
            ),
        ] {
            let mut next = prev.clone();
            mutate(&mut next);
            assert!(substantive_change(&prev, &next), "{name} 必须算实质变更");
        }
    }

    #[test]
    fn rule_config_summary_shape() {
        let prev = row();
        let summary = rule_config_summary(&prev);
        assert_eq!(summary["assignee_type"], "agent");
        assert_eq!(summary["assignee_id"], prev.assignee_id.to_string());
        assert_eq!(summary["status"], "active");
        assert_eq!(summary["execution_mode"], "run_only");
        // 只有那四个键（`title` / `description` / 模板都不进快照）。
        assert_eq!(
            summary.as_object().map(serde_json::Map::len),
            Some(RULE_CONFIG_SUMMARY_KEYS.len())
        );
        assert_eq!(RuleConfigSummary::from_row(&prev).to_json(), summary);
    }
}
