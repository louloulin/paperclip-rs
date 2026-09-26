//! onboarding 面的**领域形状**（上游 `internal/handler/onboarding.go` +
//! `onboarding_shim.go`，995 行 handler；`docs/62` §4.1 的 `M9-3`）。
//!
//! # 上游事实（逐条实测，`docs/62` §9.7）
//!
//! `"user"` 表上与本波有关的列是 **5 个**（不是计划里写的 6 个 —— `098` 是**纯 DROP**
//! 迁移，它没有建任何列，见 [`ONBOARDING_USER_COLUMNS`] 的说明）：
//!
//! | 列 | 迁移 | 说明 |
//! | --- | --- | --- |
//! | `onboarded_at` | `050` | `COALESCE` 幂等：重复 `complete` 保留第一次的时间戳 |
//! | `onboarding_questionnaire` | `051` + `094` | 问卷的**唯一**载体（`NOT NULL DEFAULT '{}'`） |
//! | `cloud_waitlist_email` | `052` | `VARCHAR(254)` |
//! | `cloud_waitlist_reason` | `052` | `TEXT`，本地再限 500 字符 |
//! | `starter_content_state` | `054` + `095` | 只有 `imported` 这一个被回填过的取值 |
//!
//! ⚠️ **`"user".onboarding_state` 是本地独有列**（`migrations/compat/537_local_only_columns.up.sql`
//! 第 43 行），**上游未建模** ⇒ M9-3 **只读不改**（既有 `crates/mc-repos/src/user.rs` 在读它）。
//!
//! # 问卷：v2 形状 + `stringOrSlice` 的宽容
//!
//! 上游注释逐字：v1 有 `team_size`，v2 把它删掉、把 `role`/`use_case` 重映射到新词表；
//! `source` 是单选但历史上写过裸字符串 ⇒ 反序列化必须**先试数组、再退单串**
//! （[`deserialize_string_or_slice`]）。历史 `NULL` 保留为 `None` 而**不是**标成
//! `*_skipped = true`（上游：回填 skip 意图会污染分析）。
//!
//! ⚠️ **在流问卷的 `complete()` 只看 `role` + `use_case`**
//! （上游逐字：「complete covers the IN-FLOW questionnaire only: `role` + `use_case`」）
//! ⇒ 判据是 [`QuestionnaireAnswers::in_flow_resolved`]，**不是**三个字段全有。

use serde::{Deserialize, Deserializer, Serialize};

use crate::timestamp::Timestamp;

/// 问卷 schema 版本（上游 `questionnaireSchemaVersion = 2`）。
pub const QUESTIONNAIRE_SCHEMA_VERSION: i32 = 2;

/// cloud waitlist 的 `reason` 上限（上游 `cloudWaitlistReasonMaxLen = 500`）。
pub const CLOUD_WAITLIST_REASON_MAX_LEN: usize = 500;

/// waitlist 邮箱上限（上游逐字注释：RFC 5321 上限，列是 `VARCHAR(254)`）。
pub const CLOUD_WAITLIST_EMAIL_MAX_LEN: usize = 254;

/// 与本波有关的 `"user"` 列（逐字，**5 个**）。
///
/// 🔴 **计划勘误**：`docs/62` §9.7 把 `onboarding_runtime_choice`（`098`）也算成一个列。
/// 实测 `migrations/upstream/098_user_onboarding_runtime_choice.up.sql` **只做 DROP**
/// （`onboarding_runtime_skipped` / `onboarding_runtime_id` / 一条 check 约束），
/// 它**没有**建任何列；那两列的设计后来移到前端 transient store。登记 `docs/32` §9.13。
pub const ONBOARDING_USER_COLUMNS: [&str; 5] = [
    "onboarded_at",
    "onboarding_questionnaire",
    "cloud_waitlist_email",
    "cloud_waitlist_reason",
    "starter_content_state",
];

/// onboarding 的**完成路径**（上游 `analytics.OnboardingPath*`，5 个合法值 + 1 个兜底）。
///
/// `unknown` **不在** [`CompletionPath::VALID`] 里：它是服务端推导不出来时的兜底，
/// `complete` 请求体**不接受**它（上游 `validCompletionPaths` 逐字只有 5 个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionPath {
    /// `full` —— 走到首个 issue 的流程末端。
    Full,
    /// `runtime_skipped` —— 没连 runtime 就完成。
    RuntimeSkipped,
    /// `cloud_waitlist` —— 从云 waitlist 软退出。
    CloudWaitlist,
    /// `skip_existing` —— welcome 页的「我以前做过」。
    SkipExisting,
    /// `invite_accept` —— 从 `/invitations` 接受过至少一个邀请。
    InviteAccept,
}

impl CompletionPath {
    /// 请求体接受的 5 个取值（上游 `validCompletionPaths`）。
    pub const VALID: [Self; 5] = [
        Self::Full,
        Self::RuntimeSkipped,
        Self::CloudWaitlist,
        Self::SkipExisting,
        Self::InviteAccept,
    ];

    /// 服务端兜底值（**不可**由请求体提交）。
    pub const UNKNOWN: &'static str = "unknown";

    /// 线上字符串。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::RuntimeSkipped => "runtime_skipped",
            Self::CloudWaitlist => "cloud_waitlist",
            Self::SkipExisting => "skip_existing",
            Self::InviteAccept => "invite_accept",
        }
    }

    /// 解析（**只认**那 5 个；`unknown` / 空 / 未知 ⇒ `None`）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::VALID
            .into_iter()
            .find(|candidate| candidate.as_str() == raw)
    }
}

/// `POST /api/me/onboarding/complete` 的请求体（上游 `completeOnboardingRequest`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteOnboardingRequest {
    /// 完成路径（`None` = 服务端自己推）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_path: Option<String>,
    /// 工作区（`None` = 从上下文取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

impl CompleteOnboardingRequest {
    /// 请求体里的路径是否合法（`None` 合法 —— 服务端会自己推）。
    #[must_use]
    pub fn parse_completion_path(&self) -> Option<CompletionPath> {
        self.completion_path
            .as_deref()
            .and_then(CompletionPath::parse)
    }
}

/// `PATCH /api/me/onboarding` 的请求体（上游 `patchOnboardingRequest`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchOnboardingRequest {
    /// 问卷答案（`None` = 这次不碰问卷）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questionnaire: Option<QuestionnaireAnswers>,
}

/// `stringOrSlice`：**先试数组、再退单串**（上游 `UnmarshalJSON`）。
///
/// 三态语义逐字：
/// - `null` / 缺省 / 空字节 ⇒ 空 `Vec`；
/// - JSON 数组 ⇒ 逐元素；
/// - JSON 字符串 ⇒ 空串折成空 `Vec`（「没回答」），否则单元素 `Vec`。
///
/// # Errors
///
/// 既不是数组也不是字符串 ⇒ `Err`（调用方给 400）。
pub fn deserialize_string_or_slice<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    match raw {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                serde_json::Value::String(text) => Ok(text),
                other => Err(serde::de::Error::custom(format!(
                    "expected a string in the questionnaire list, got {other}"
                ))),
            })
            .collect(),
        serde_json::Value::String(text) => Ok(if text.is_empty() {
            Vec::new()
        } else {
            vec![text]
        }),
        other => Err(serde::de::Error::custom(format!(
            "expected a string or a list of strings, got {other}"
        ))),
    }
}

/// `PATCH /api/me/onboarding` 的 `questionnaire`（上游 `questionnaireAnswers`，v2 形状）。
///
/// 九个字段 + `version`：三个「多选 + 其它 + 跳过」三元组，加 v2 版本号。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionnaireAnswers {
    /// 获客渠道（**单选**语义，但历史上是数组；空 = 没答）。
    #[serde(default, deserialize_with = "deserialize_string_or_slice")]
    pub source: Vec<String>,
    /// `source` 选「其它」时填的文本。
    #[serde(default)]
    pub source_other: String,
    /// 是否显式跳过 `source`。
    #[serde(default)]
    pub source_skipped: bool,
    /// 角色（单选）。
    #[serde(default)]
    pub role: String,
    /// `role` 选「其它」时填的文本。
    #[serde(default)]
    pub role_other: String,
    /// 是否显式跳过 `role`。
    #[serde(default)]
    pub role_skipped: bool,
    /// 用例（**多选**）。
    #[serde(default, deserialize_with = "deserialize_string_or_slice")]
    pub use_case: Vec<String>,
    /// `use_case` 选「其它」时填的文本。
    #[serde(default)]
    pub use_case_other: String,
    /// 是否显式跳过 `use_case`。
    #[serde(default)]
    pub use_case_skipped: bool,
    /// schema 版本（当前 [`QUESTIONNAIRE_SCHEMA_VERSION`]）。
    #[serde(default)]
    pub version: i32,
}

impl QuestionnaireAnswers {
    /// 上游 `sourceResolved()`：答了**或**显式跳过。
    #[must_use]
    pub fn source_resolved(&self) -> bool {
        !self.source.is_empty() || self.source_skipped
    }

    /// 上游 `roleResolved()`。
    #[must_use]
    pub fn role_resolved(&self) -> bool {
        !self.role.is_empty() || self.role_skipped
    }

    /// 上游 `use_case_resolved()`。
    #[must_use]
    pub fn use_case_resolved(&self) -> bool {
        !self.use_case.is_empty() || self.use_case_skipped
    }

    /// 上游 `complete()` 的判据：**只看 `role` + `use_case`**（在流问卷的字段）。
    ///
    /// ⚠️ `source` 是 v2 新增的**非在流**字段 ⇒ 它未答**不**阻塞 `complete`。
    #[must_use]
    pub fn in_flow_resolved(&self) -> bool {
        self.role_resolved() && self.use_case_resolved()
    }

    /// 版本是否是本 handler 认识的 v2（未来的 v3 行不得按 v2 语义计数）。
    #[must_use]
    pub fn is_current_schema(&self) -> bool {
        self.version == QUESTIONNAIRE_SCHEMA_VERSION
    }

    /// 换成 v2 的版本号（写入前盖章）。
    pub fn stamp_current_version(&mut self) {
        self.version = QUESTIONNAIRE_SCHEMA_VERSION;
    }
}

/// 一个用户的 onboarding 档案（[`ONBOARDING_USER_COLUMNS`] 的投影）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingProfile {
    /// `user.onboarded_at`（`None` = 还没完成）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarded_at: Option<Timestamp>,
    /// `user.onboarding_questionnaire`（`NOT NULL DEFAULT '{}'`）。
    #[serde(default)]
    pub onboarding_questionnaire: QuestionnaireAnswers,
    /// `user.cloud_waitlist_email`（已小写 + trim，见 [`normalize_waitlist_email`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_waitlist_email: Option<String>,
    /// `user.cloud_waitlist_reason`（空串折成 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_waitlist_reason: Option<String>,
    /// `user.starter_content_state`（上游现存的唯一取值是 `imported`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter_content_state: Option<String>,
}

impl OnboardingProfile {
    /// `starter_content_state` 的上游已知取值（`054`/`095` 只写过这一个）。
    pub const STARTER_CONTENT_IMPORTED: &'static str = "imported";

    /// 是否已完成 onboarding。
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.onboarded_at.is_some()
    }

    /// 是否已加入云 waitlist（`email` 是唯一的载体）。
    #[must_use]
    pub fn has_joined_cloud_waitlist(&self) -> bool {
        self.cloud_waitlist_email.is_some()
    }
}

/// `POST /api/me/onboarding/cloud-waitlist` 的请求体（上游 `joinCloudWaitlistRequest`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCloudWaitlistRequest {
    /// 邮箱（会被 `to_lowercase().trim()` 规范化）。
    #[serde(default)]
    pub email: String,
    /// 想用云 runtime 的理由（可空）。
    #[serde(default)]
    pub reason: String,
}

/// 上游 `JoinCloudWaitlist` 的邮箱规范化：`ToLower(TrimSpace(email))`。
#[must_use]
pub fn normalize_waitlist_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// 上游 `JoinCloudWaitlist` 的 `reason` 规范化：`TrimSpace`（**不**小写）。
#[must_use]
pub fn normalize_waitlist_reason(raw: &str) -> String {
    raw.trim().to_string()
}

/// 上游的邮箱校验：非空、≤ [`CLOUD_WAITLIST_EMAIL_MAX_LEN`]、且 `net/mail.ParseAddress` 能解析。
///
/// ⚠️ 本仓**不**引入 RFC 5322 解析库（依赖边被 anchor 冻结）：这里做**保守**校验
/// （唯一 `@`、`@` 两侧非空、无空白、域名有 `.`），比上游**更严**的方向是可以接受的
/// （更严只会把某些上游接受的串拒成 400，不会把非法的放进来）。
/// 偏离登记 `docs/32` §9.13。
#[must_use]
pub fn is_acceptable_waitlist_email(normalized: &str) -> bool {
    if normalized.is_empty() || normalized.len() > CLOUD_WAITLIST_EMAIL_MAX_LEN {
        return false;
    }
    if normalized.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = normalized.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// `reason` 是否超长（上游 `len(reason) > cloudWaitlistReasonMaxLen`）。
#[must_use]
pub fn is_acceptable_waitlist_reason(normalized: &str) -> bool {
    normalized.len() <= CLOUD_WAITLIST_REASON_MAX_LEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_paths_match_the_five_accepted_values() {
        assert_eq!(CompletionPath::VALID.len(), 5);
        for path in CompletionPath::VALID {
            assert_eq!(CompletionPath::parse(path.as_str()), Some(path));
        }
        // ⚠️ `unknown` 是服务端兜底，**不可**由请求体提交。
        assert_eq!(CompletionPath::parse(CompletionPath::UNKNOWN), None);
        assert_eq!(CompletionPath::parse(""), None);
        assert_eq!(CompletionPath::CloudWaitlist.as_str(), "cloud_waitlist");
    }

    #[test]
    fn questionnaire_accepts_both_the_array_and_the_legacy_string_shape() {
        // 现代形状（v2：`source` 是长度 1 的数组）。
        let answers: QuestionnaireAnswers = serde_json::from_str(
            r#"{"source":["search"],"role":"engineer","use_case":["ship_code","manage_team"],"version":2}"#,
        )
        .expect("array shape");
        assert_eq!(answers.source, vec!["search"]);
        assert_eq!(answers.use_case, vec!["ship_code", "manage_team"]);
        assert!(answers.is_current_schema());
        assert!(answers.in_flow_resolved());

        // 历史形状（`source` 是裸字符串）——必须仍能读回来。
        let legacy: QuestionnaireAnswers = serde_json::from_str(
            r#"{"source":"friend","role":"writer","use_case":"write_publish"}"#,
        )
        .expect("legacy string shape");
        assert_eq!(legacy.source, vec!["friend"]);
        assert_eq!(legacy.use_case, vec!["write_publish"]);
        // 历史行没有 version ⇒ `0` ⇒ 不是当前 schema（不得按 v2 语义计数）。
        assert!(!legacy.is_current_schema());

        // 空串 = 「没回答」⇒ 空数组（不是 `[""]`）。
        let blank: QuestionnaireAnswers =
            serde_json::from_str(r#"{"source":"","role":"","use_case":""}"#).expect("blank");
        assert!(blank.source.is_empty());
        assert!(!blank.source_resolved());

        // null / 缺省同样折成空。
        let absent: QuestionnaireAnswers = serde_json::from_str("{}").expect("empty");
        assert!(absent.source.is_empty() && absent.use_case.is_empty());
        assert!(absent.source_other.is_empty());

        // 类型不对 ⇒ 报错（调用方 400），不是静默丢字段。
        assert!(serde_json::from_str::<QuestionnaireAnswers>(r#"{"source":{"a":1}}"#).is_err());
        assert!(serde_json::from_str::<QuestionnaireAnswers>(r#"{"source":[1,2]}"#).is_err());
    }

    #[test]
    fn in_flow_completion_only_needs_role_and_use_case() {
        let mut answers = QuestionnaireAnswers::default();
        assert!(!answers.in_flow_resolved());
        // `source` 未答**不**阻塞在流完成。
        answers.role = "founder".into();
        assert!(!answers.in_flow_resolved());
        answers.use_case_skipped = true;
        assert!(answers.in_flow_resolved());
        assert!(!answers.source_resolved());
        // 显式跳过也算 resolved（三个字段各自独立）。
        answers.source_skipped = true;
        assert!(answers.source_resolved());
        answers.stamp_current_version();
        assert_eq!(answers.version, QUESTIONNAIRE_SCHEMA_VERSION);
        assert!(answers.is_current_schema());
    }

    #[test]
    fn profile_projects_the_five_upstream_columns() {
        assert_eq!(ONBOARDING_USER_COLUMNS.len(), 5);
        assert!(!ONBOARDING_USER_COLUMNS.contains(&"onboarding_state"));
        let mut profile = OnboardingProfile::default();
        assert!(!profile.is_complete());
        assert!(!profile.has_joined_cloud_waitlist());
        profile.onboarded_at = Some(Timestamp::default());
        assert!(profile.is_complete());
        // `starter_content_state` 的唯一已知取值。
        assert_eq!(OnboardingProfile::STARTER_CONTENT_IMPORTED, "imported");
    }

    #[test]
    fn waitlist_normalization_and_validation_match_upstream() {
        assert_eq!(
            normalize_waitlist_email("  User@Example.TEST "),
            "user@example.test"
        );
        assert_eq!(normalize_waitlist_reason("  why  "), "why");

        assert!(is_acceptable_waitlist_email("user@example.test"));
        assert!(!is_acceptable_waitlist_email(""));
        assert!(!is_acceptable_waitlist_email("no-at-sign"));
        assert!(!is_acceptable_waitlist_email("two@at@signs"));
        assert!(!is_acceptable_waitlist_email("@example.test"));
        assert!(!is_acceptable_waitlist_email("user@localhost"));
        assert!(!is_acceptable_waitlist_email("user name@example.test"));
        assert!(!is_acceptable_waitlist_email(&format!(
            "{}@example.test",
            "a".repeat(CLOUD_WAITLIST_EMAIL_MAX_LEN)
        )));
        // 254 是上界本身（列宽），255 才越界。
        let boundary = format!(
            "{}@e.test",
            "a".repeat(CLOUD_WAITLIST_EMAIL_MAX_LEN - "@e.test".len())
        );
        assert_eq!(boundary.len(), CLOUD_WAITLIST_EMAIL_MAX_LEN);
        assert!(is_acceptable_waitlist_email(&boundary));

        assert!(is_acceptable_waitlist_reason(""));
        assert!(is_acceptable_waitlist_reason(
            &"x".repeat(CLOUD_WAITLIST_REASON_MAX_LEN)
        ));
        assert!(!is_acceptable_waitlist_reason(
            &"x".repeat(CLOUD_WAITLIST_REASON_MAX_LEN + 1)
        ));
    }

    #[test]
    fn complete_request_path_is_optional_and_validated() {
        let empty = CompleteOnboardingRequest::default();
        assert_eq!(empty.parse_completion_path(), None);
        let bad = CompleteOnboardingRequest {
            completion_path: Some("unknown".into()),
            workspace_id: None,
        };
        assert_eq!(bad.parse_completion_path(), None);
        let good = CompleteOnboardingRequest {
            completion_path: Some("full".into()),
            workspace_id: None,
        };
        assert_eq!(good.parse_completion_path(), Some(CompletionPath::Full));
    }
}
