//! 插件 manifest 的结构化类型与校验（声明式契约的**唯一**真值）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-5…M6-8 只读本文件，不得改。
//! - **上游**：`pkg/plugincontract/manifest.go` —— `Manifest`(177) / `Author`(190) /
//!   `Contributes`(195) / `Surface`(203) / `Hook`(213) / `HookSchedule`(228) /
//!   `HookTransport`(233) / `Resource`(240) / `ConfigField`(248) / `ConfigSchema`(264)。
//! - **落库形态**：`plugin_installation.manifest` 与 `plugin_package_version.manifest` 都是
//!   JSONB —— 校验通过后**原样**落库（不要「先转成 Rust 结构再序列化回去」，那会改键序、
//!   丢未知字段、并把上游的 omitempty 语义抹平）。Rust 类型只用于校验与读取。
//! - **hook 的 `input_schema` 是 `json.RawMessage`**（原样透传的 JSON）。上游的
//!   `pluginHookResponse`（`internal/handler/plugin.go:82`）**不下发** `input_schema` ——
//!   本仓的响应投影要照抄这个「有字段但不外发」的取舍。
//! - **本仓约定**：校验失败返回 `thiserror` 错误 + 稳定错误码（插件作者能据此改 manifest）；
//!   未知字段**不报错**（前向兼容：上游 `Manifest` 有新增字段时老包不能被判死）。
//! - **不做什么**：不做 manifest 的**迁移**（`manifest_version` 不是 v1 的包直接拒，不升级）；
//!   不做 schema 的语义校验（`input_schema` 只做「是不是合法 JSON 对象」）。
//!
//! **状态：M6-1 已落地。**
//!
//! ## 与上游的两处**已知差异**（`docs/32` §9 已登记）
//!
//! 1. **未知字段**：上游 `DisallowUnknownFields` 一律报错，本仓忽略（见上面的「本仓约定」）。
//!    这是本仓的**前向兼容**政策，代价是插件作者写错键名不会被当场指出。
//! 2. **时区**：上游用 `time.LoadLocation` 解析 IANA 名字并**按该时区**算 cron 的最小间隔；
//!    本 crate 没有 tz 依赖（`chrono-tz` 不在本 crate 的依赖里），因此 `timezone` 只做
//!    **结构校验**（形态 + 长度），cron 的间隔下限在 **UTC** 下核算。对「每分钟/每 5 分钟」
//!    这类判定无差异；对「本地时间 02:30」在 DST 跳变日的判定可能有出入。

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;
use std::fmt;

use crate::capabilities::{HookTransport as HookTransportKind, ResourceType, SurfaceType};
use crate::scope;

mod cron;

/// 本宿主**唯一**理解的 manifest 版本（上游 `ManifestVersion1`）。
pub const MANIFEST_VERSION_1: i32 = 1;

/// 插件包内 manifest 的约定文件名（上游 `ManifestFilename`）。
pub const MANIFEST_FILENAME: &str = "multica.plugin.json";

/// 解析前的体积上限（上游 `MaxManifestSize`）。
pub const MAX_MANIFEST_SIZE: usize = 1 << 20;

/// `plugin_installation.version` 列的字节上限（上游 `MaxVersionLength`）。
pub const MAX_VERSION_LENGTH: usize = 64;

/// 计划型 hook 的**最小**调用间隔（上游 `MinimumScheduleInterval`，5 分钟）。
pub const MINIMUM_SCHEDULE_INTERVAL_SECS: u64 = 300;

/// `description` 的字节上限（上游 `len(m.Description) > 2000`）。
pub const MAX_DESCRIPTION_BYTES: usize = 2000;

/// hook 的 `description` 上限（上游 `%s.description must be non-empty and at most 2000 bytes`）。
pub const MAX_HOOK_DESCRIPTION_BYTES: usize = 2000;

/// 配置字段的四个标量类型 + 一个枚举类型（上游 `ConfigString` …）。
pub const CONFIG_STRING: &str = "string";
/// 见 [`CONFIG_STRING`]。
pub const CONFIG_NUMBER: &str = "number";
/// 见 [`CONFIG_STRING`]。
pub const CONFIG_BOOL: &str = "bool";
/// 见 [`CONFIG_STRING`]。
pub const CONFIG_ENUM: &str = "enum";
/// 见 [`CONFIG_STRING`]。
pub const CONFIG_SECRET: &str = "secret";

/// `event` 触发者可订阅的七个产品事件（上游 `knownEvents`，与 `eventcontract` 同名）。
pub const EVENT_ISSUE_CREATED: &str = "issue.created";
/// 见 [`EVENT_ISSUE_CREATED`]。
pub const EVENT_ISSUE_UPDATED: &str = "issue.updated";
/// 见 [`EVENT_ISSUE_CREATED`]。
pub const EVENT_ISSUE_STATUS_CHANGED: &str = "issue.status_changed";
/// 见 [`EVENT_ISSUE_CREATED`]。
pub const EVENT_COMMENT_CREATED: &str = "comment.created";
/// 见 [`EVENT_ISSUE_CREATED`]。
pub const EVENT_TASK_STARTED: &str = "task.started";
/// 见 [`EVENT_ISSUE_CREATED`]。
pub const EVENT_TASK_COMPLETED: &str = "task.completed";
/// 见 [`EVENT_ISSUE_CREATED`]。
pub const EVENT_TASK_FAILED: &str = "task.failed";

/// 七个可订阅事件（顺序即上游 `knownEvents` 的书写顺序）。
pub const KNOWN_EVENTS: &[&str] = &[
    EVENT_ISSUE_CREATED,
    EVENT_ISSUE_UPDATED,
    EVENT_ISSUE_STATUS_CHANGED,
    EVENT_COMMENT_CREATED,
    EVENT_TASK_STARTED,
    EVENT_TASK_COMPLETED,
    EVENT_TASK_FAILED,
];

/// 事件 ⇒ 读同一份内容所需的 scope（上游 `eventReadScope`）。
///
/// 订阅 `issue.*` 拿到的是 description、订阅 `comment.created` 拿到的是正文 —— 没有这道
/// 映射，订阅就成了「读权限没给也能收到」。安装期强制，同意屏因此能显示订阅隐含的读权限。
#[must_use]
pub fn event_read_scope(event: &str) -> Option<&'static str> {
    match event {
        EVENT_ISSUE_CREATED | EVENT_ISSUE_UPDATED | EVENT_ISSUE_STATUS_CHANGED => {
            Some(scope::SCOPE_ISSUES_READ)
        }
        EVENT_COMMENT_CREATED => Some(scope::SCOPE_COMMENTS_READ),
        EVENT_TASK_STARTED | EVENT_TASK_COMPLETED | EVENT_TASK_FAILED => {
            Some(scope::SCOPE_TASKS_READ)
        }
        _ => None,
    }
}

/// 一个事件是否可被 manifest 订阅（上游 `IsKnownEvent`）。
#[must_use]
pub fn is_known_event(event: &str) -> bool {
    KNOWN_EVENTS.contains(&event)
}

/// manifest 解析/校验失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// 空 body。
    #[error("plugin manifest is empty")]
    Empty,
    /// 超过 [`MAX_MANIFEST_SIZE`]。
    #[error("plugin manifest exceeds {limit} bytes")]
    TooLarge {
        /// 生效的上限。
        limit: usize,
    },
    /// JSON 层就解不开（含尾部多余内容）。
    #[error("decode plugin manifest: {0}")]
    Decode(String),
    /// 能解开但违反 v1 契约（消息与上游逐条对齐）。
    #[error("{0}")]
    Invalid(String),
}

impl ManifestError {
    /// 稳定错误码。
    ///
    /// - 读不出 JSON（空 / 超限 / 语法错）⇒ `plugin_manifest_invalid_json`；
    /// - 读得出但违约束 ⇒ `plugin_manifest_invalid`。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty | Self::TooLarge { .. } | Self::Decode(_) => "plugin_manifest_invalid_json",
            Self::Invalid(_) => "plugin_manifest_invalid",
        }
    }
}

/// v1 manifest 的根对象（上游 `Manifest`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// 必须等于 [`MANIFEST_VERSION_1`]。
    pub manifest_version: i32,
    /// 反向 DNS 命名空间，至少两段（`com.example.demo`）。
    pub key: String,
    /// 显示名（单行、无首尾空白、≤160 字节）。
    pub name: String,
    /// 描述（≤2000 字节、无 `\r`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// 语义化版本（≤64 字节，落 `plugin_installation.version`）。
    pub version: String,
    /// 作者。
    pub author: Author,
    /// 图标：包内相对路径。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// 请求的 scope（非空、≤64 项、无重复、取值必须在 [`crate::scope`] 的词表里）。
    pub scopes: Vec<String>,
    /// 宿主渲染的配置表单（保持声明顺序）。
    #[serde(default)]
    pub config: ConfigSchema,
    /// 静态贡献（面 / hook / 资源）。
    pub contributes: Contributes,
}

/// 作者信息（上游 `Author`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Author {
    /// 作者名（单行、≤160 字节）。
    pub name: String,
    /// 主页（可选，必须是**明文 HTTPS** URL、≤2048 字节）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

/// 静态贡献三件套（上游 `Contributes`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Contributes {
    /// 宿主挂载的 iframe。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<Surface>,
    /// 插件侧能力（宿主按声明调用）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<Hook>,
    /// 不产生任何调用的静态资源。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<Resource>,
}

/// 一个面（上游 `Surface`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// 贡献键（`[a-z][a-z0-9]*([_-][a-z0-9]+)*`）。
    pub key: String,
    /// 面类型（`issue_panel` / `sidebar_panel` / `modal`）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 显示名（单行、≤160 字节）。
    pub name: String,
    /// 入口脚本：包内相对路径，且必须以 `.js` / `.mjs` 结尾（宿主自己渲染 HTML 文档）。
    pub entry: String,
    /// 允许运行的平台（`web` / `desktop`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

/// 一个 hook（上游 `Hook`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hook {
    /// 贡献键。
    pub key: String,
    /// 显示名（单行、≤160 字节）。
    pub name: String,
    /// 描述：会作为 MCP 工具描述被 agent 读到，因此**必填**且可多行（≤2000 字节）。
    pub description: String,
    /// 入参 JSON Schema（`json.RawMessage` 语义：原样透传；本仓**不下发**给前端）。
    #[serde(
        rename = "input_schema",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_schema: Option<serde_json::Value>,
    /// 触发者（至少一个，无重复）。
    pub triggers: Vec<String>,
    /// 订阅的事件（仅 `event` 触发者允许；取值见 [`KNOWN_EVENTS`]）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    /// 自动节奏（仅 `schedule` 触发者允许，且只支持 http 传输）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<HookSchedule>,
    /// 出站传输。
    pub transport: HookTransport,
    /// 单次调用超时（0 = 用宿主默认；否则 100..=30000）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub timeout_ms: i64,
}

// serde 的 `skip_serializing_if` 只接受 `fn(&T) -> bool`，所以这里的引用不是多余。
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// 计划型 hook 的唯一节奏（上游 `HookSchedule`）。
///
/// 多个节奏 = 多个 hook key —— 这样「一次持久执行」的身份才没有歧义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookSchedule {
    /// 五字段标准 cron（**不含**内联 `TZ=` / `CRON_TZ=` 前缀）。
    pub cron: String,
    /// IANA 时区名（本仓只做结构校验，见文件头注的差异 2）。
    pub timezone: String,
}

/// hook 的出站传输（上游 `HookTransport`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookTransport {
    /// `http` 或 `mcp`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 端点 URL：必须是明文 HTTPS，且 host 被某个 `net:` scope **精确**覆盖。
    pub url: String,
}

/// 一个静态资源（上游 `Resource`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    /// 目前只有 `skill`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 贡献键。
    pub key: String,
    /// 入口：skill 资源必须**恰好**是 `skills/<key>/SKILL.md`。
    pub entry: String,
}

/// 一个宿主渲染的配置输入（上游 `ConfigField`）。
///
/// `key` 是**外层对象的键**（上游 `json:"-"`），因此本类型单独序列化时不带它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigField {
    /// 字段名（由 [`ConfigSchema`] 的 map 键回填）。
    #[serde(skip)]
    pub key: String,
    /// `string` / `number` / `bool` / `enum` / `secret`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 显示标签（单行、≤160 字节）。
    pub label: String,
    /// 说明（≤500 字节）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// 是否必填。
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// 枚举取值（仅 `enum` 允许；非空、≤64 项、无重复、每项单行 ≤160 字节）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// 占位提示（≤160 字节）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    /// 是否多行输入（仅 `string` 允许）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiline: bool,
}

// 同上：serde 回调的签名就是 `fn(&bool) -> bool`。
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// 配置表单的**有序**字段表（上游 `ConfigSchema`）。
///
/// wire 形态是一个以字段名为键的 JSON 对象，但**顺序必须保住**（生成的表单要跨安装稳定），
/// 而 `serde_json::Map` 默认是排序的 —— 所以这里手写 `Serialize`/`Deserialize`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigSchema {
    /// 按声明顺序排列的字段。
    pub fields: Vec<ConfigField>,
}

impl ConfigSchema {
    /// 字段个数（上游 `Len`）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// 没有字段。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// 按名取字段（上游 `Field`）。
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&ConfigField> {
        self.fields.iter().find(|field| field.key == key)
    }
}

/// 序列化时的字段体（复刻上游 `MarshalJSON` 里那个匿名结构的 omitempty 语义）。
#[derive(Serialize)]
struct ConfigFieldBody<'a> {
    #[serde(rename = "type")]
    kind: &'a String,
    label: &'a String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: &'a String,
    #[serde(skip_serializing_if = "is_false")]
    required: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: &'a Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    placeholder: &'a String,
    #[serde(skip_serializing_if = "is_false")]
    multiline: bool,
}

impl Serialize for ConfigSchema {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for field in &self.fields {
            let body = ConfigFieldBody {
                kind: &field.kind,
                label: &field.label,
                description: &field.description,
                required: field.required,
                options: &field.options,
                placeholder: &field.placeholder,
                multiline: field.multiline,
            };
            serde::ser::SerializeMap::serialize_entry(&mut map, &field.key, &body)?;
        }
        serde::ser::SerializeMap::end(map)
    }
}

impl<'de> Deserialize<'de> for ConfigSchema {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ConfigSchemaVisitor)
    }
}

struct ConfigSchemaVisitor;

impl<'de> Visitor<'de> for ConfigSchemaVisitor {
    type Value = ConfigSchema;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object of config fields")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = Vec::new();
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "config contains duplicate field {key:?}"
                )));
            }
            let mut field: ConfigField = map.next_value()?;
            field.key = key;
            fields.push(field);
        }
        Ok(ConfigSchema { fields })
    }
}

/// 解析并校验一份 v1 manifest（上游 `ParseManifest` 的去 canonical 版本）。
///
/// 返回的 [`Manifest`] 只用于**校验与读取**；落库的是调用方手上的 `raw` **原样字节**
/// （本仓不做 canonical 化，理由见文件头注）。
///
/// # Errors
///
/// 见 [`ManifestError`]：空 / 超限 / JSON 语法错 / 违反 v1 约束。
pub fn parse_manifest(raw: &[u8]) -> Result<Manifest, ManifestError> {
    if raw.is_empty() {
        return Err(ManifestError::Empty);
    }
    if raw.len() > MAX_MANIFEST_SIZE {
        return Err(ManifestError::TooLarge {
            limit: MAX_MANIFEST_SIZE,
        });
    }
    // `from_slice` 对尾部多余内容会报错（等价上游的 `rejectTrailingJSON`）。
    let manifest: Manifest =
        serde_json::from_slice(raw).map_err(|error| ManifestError::Decode(error.to_string()))?;
    manifest.validate()?;
    Ok(manifest)
}

impl Manifest {
    /// 逐项校验（上游 `Validate`）。
    ///
    /// # Errors
    ///
    /// [`ManifestError::Invalid`]，消息与上游逐条对齐（插件作者据此改 manifest）。
    pub fn validate(&self) -> Result<(), ManifestError> {
        let invalid = |message: String| Err(ManifestError::Invalid(message));
        if self.manifest_version != MANIFEST_VERSION_1 {
            return invalid(format!("manifest_version must be {MANIFEST_VERSION_1}"));
        }
        self.validate_identity()?;
        self.validate_scopes()?;
        self.validate_config()?;
        self.validate_contributions()
    }

    /// 标识信息：key / name / description / version / author / icon。
    fn validate_identity(&self) -> Result<(), ManifestError> {
        if let Err(message) = validate_plugin_key(&self.key) {
            return Err(ManifestError::Invalid(message));
        }
        if let Err(message) = validate_display_text("name", &self.name, 160) {
            return Err(ManifestError::Invalid(message));
        }
        if self.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(ManifestError::Invalid(format!(
                "description exceeds {MAX_DESCRIPTION_BYTES} bytes"
            )));
        }
        if self.description.contains('\r') {
            return Err(ManifestError::Invalid(
                "description must not contain carriage returns".to_owned(),
            ));
        }
        // 先卡长度再信正则：semver 允许不定长的 build/prerelease 段，而
        // `plugin_installation.version` 是 64 字节。这里拒掉，INSERT 期的约束违反就变成
        // 一个**点了名字段**的解析错误。
        if self.version.len() > MAX_VERSION_LENGTH {
            return Err(ManifestError::Invalid(format!(
                "version exceeds {MAX_VERSION_LENGTH} bytes"
            )));
        }
        if !is_semver(&self.version) {
            return Err(ManifestError::Invalid(format!(
                "version must be semantic versioning, got {:?}",
                self.version
            )));
        }
        if let Err(message) = validate_display_text("author.name", &self.author.name, 160) {
            return Err(ManifestError::Invalid(message));
        }
        if !self.author.url.is_empty() {
            validate_https_url("author.url", &self.author.url)?;
        }
        if !self.icon.is_empty() {
            validate_relative_path("icon", &self.icon)?;
        }
        Ok(())
    }

    /// scope 列表（上游 `validateScopes`）。
    fn validate_scopes(&self) -> Result<(), ManifestError> {
        if self.scopes.is_empty() {
            return Err(ManifestError::Invalid(
                "scopes must not be empty".to_owned(),
            ));
        }
        if self.scopes.len() > 64 {
            return Err(ManifestError::Invalid(
                "scopes must not exceed 64 entries".to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, scope) in self.scopes.iter().enumerate() {
            if !seen.insert(scope) {
                return Err(ManifestError::Invalid(format!(
                    "scopes contains duplicate value {scope:?}"
                )));
            }
            if let Err(error) = scope::validate_scope(scope) {
                return Err(ManifestError::Invalid(format!("scopes[{index}]: {error}")));
            }
        }
        Ok(())
    }

    /// 配置表单（上游 `validateConfig`）。
    fn validate_config(&self) -> Result<(), ManifestError> {
        if self.config.len() > 32 {
            return Err(ManifestError::Invalid(
                "config must not exceed 32 fields".to_owned(),
            ));
        }
        for field in &self.config.fields {
            validate_config_field(field)?;
        }
        Ok(())
    }

    /// 贡献三件套（上游 `validateContributions`）的入口：总数 + 三类各自校验。
    fn validate_contributions(&self) -> Result<(), ManifestError> {
        let contributes = &self.contributes;
        let total =
            contributes.surfaces.len() + contributes.hooks.len() + contributes.resources.len();
        if total == 0 {
            return Err(ManifestError::Invalid(
                "contributes must declare at least one surface, hook, or resource".to_owned(),
            ));
        }
        if total > 64 {
            return Err(ManifestError::Invalid(
                "contributes must not exceed 64 entries".to_owned(),
            ));
        }
        validate_surfaces(&contributes.surfaces)?;
        self.validate_hooks()?;
        validate_resources(&contributes.resources)
    }

    /// hook 列表（上游 `validateContributions` 的 hooks 段）。
    fn validate_hooks(&self) -> Result<(), ManifestError> {
        let mut keys = BTreeSet::new();
        for (index, hook) in self.contributes.hooks.iter().enumerate() {
            let field = format!("contributes.hooks[{index}]");
            validate_hook(hook, &field, &self.scopes, &mut keys)?;
        }
        Ok(())
    }
}

mod rules;

use rules::{
    is_semver, validate_config_field, validate_display_text, validate_hook, validate_https_url,
    validate_plugin_key, validate_relative_path, validate_resources, validate_surfaces,
};

#[cfg(test)]
mod tests;
