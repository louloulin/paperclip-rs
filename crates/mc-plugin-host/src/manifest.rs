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

/// 一个配置字段（上游 `validateConfig` 的循环体）。
fn validate_config_field(field: &ConfigField) -> Result<(), ManifestError> {
    if !is_contribution_key(&field.key) {
        return Err(ManifestError::Invalid(format!(
            "config contains invalid field name {:?}",
            field.key
        )));
    }
    let label = format!("config.{}", field.key);
    let invalid = |message: String| Err(ManifestError::Invalid(message));
    match field.kind.as_str() {
        CONFIG_STRING | CONFIG_NUMBER | CONFIG_BOOL | CONFIG_SECRET => {
            if !field.options.is_empty() {
                return invalid(format!("{label}.options is only valid for enum fields"));
            }
            if field.multiline && field.kind != CONFIG_STRING {
                return invalid(format!("{label}.multiline is only valid for string fields"));
            }
        }
        CONFIG_ENUM => {
            if field.options.is_empty() {
                return invalid(format!("{label}.options must not be empty for enum fields"));
            }
            if field.options.len() > 64 {
                return invalid(format!("{label}.options must not exceed 64 entries"));
            }
            let mut seen = BTreeSet::new();
            for option in &field.options {
                validate_display_text(&format!("{label}.options"), option, 160)
                    .map_err(ManifestError::Invalid)?;
                if !seen.insert(option) {
                    return invalid(format!(
                        "{label}.options contains duplicate value {option:?}"
                    ));
                }
            }
        }
        other => return invalid(format!("{label}.type is unsupported: {other:?}")),
    }
    if field.description.len() > 500 {
        return invalid(format!("{label}.description exceeds 500 bytes"));
    }
    if field.placeholder.len() > 160 {
        return invalid(format!("{label}.placeholder exceeds 160 bytes"));
    }
    validate_display_text(&format!("{label}.label"), &field.label, 160)
        .map_err(ManifestError::Invalid)
}

/// 面的入口脚本白名单（上游 `strings.HasSuffix`，**大小写敏感** —— `panel.JS` 是拒的）。
fn is_script_entry(entry: &str) -> bool {
    entry
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension == "js" || extension == "mjs")
}

/// 面列表（上游 `validateContributions` 的 surfaces 段）。
fn validate_surfaces(surfaces: &[Surface]) -> Result<(), ManifestError> {
    let mut keys = BTreeSet::new();
    for (index, surface) in surfaces.iter().enumerate() {
        let field = format!("contributes.surfaces[{index}]");
        if !is_contribution_key(&surface.key) {
            return Err(ManifestError::Invalid(format!("{field}.key is invalid")));
        }
        if !keys.insert(&surface.key) {
            return Err(ManifestError::Invalid(format!(
                "duplicate surface key {:?}",
                surface.key
            )));
        }
        if SurfaceType::parse(&surface.kind).is_none() {
            return Err(ManifestError::Invalid(format!(
                "{field}.type is unsupported: {:?}",
                surface.kind
            )));
        }
        validate_display_text(&format!("{field}.name"), &surface.name, 160)
            .map_err(ManifestError::Invalid)?;
        validate_relative_path(&format!("{field}.entry"), &surface.entry)?;
        // 宿主自己生成面的 HTML 文档并把这个脚本塞进去 —— 这正是宿主能挂上由 `net:` scope
        // 推出的 CSP 的原因：插件自带的 HTML 会带着它自己服务器的策略，`net:` 就只是声明
        // 而不是控制了。
        if !is_script_entry(&surface.entry) {
            return Err(ManifestError::Invalid(format!(
                "{field}.entry must be a .js or .mjs script; the host renders the surface document itself"
            )));
        }
        let mut seen = BTreeSet::new();
        for platform in &surface.platforms {
            if platform != "web" && platform != "desktop" {
                return Err(ManifestError::Invalid(format!(
                    "{field}.platforms contains unsupported platform {platform:?}"
                )));
            }
            if !seen.insert(platform) {
                return Err(ManifestError::Invalid(format!(
                    "{field}.platforms contains duplicate platform {platform:?}"
                )));
            }
        }
    }
    Ok(())
}

/// 一个 hook（上游 `validateContributions` 的 hooks 循环体）。
fn validate_hook(
    hook: &Hook,
    field: &str,
    manifest_scopes: &[String],
    keys: &mut BTreeSet<String>,
) -> Result<(), ManifestError> {
    if !is_contribution_key(&hook.key) {
        return Err(ManifestError::Invalid(format!("{field}.key is invalid")));
    }
    if !keys.insert(hook.key.clone()) {
        return Err(ManifestError::Invalid(format!(
            "duplicate hook key {:?}",
            hook.key
        )));
    }
    validate_display_text(&format!("{field}.name"), &hook.name, 160)
        .map_err(ManifestError::Invalid)?;
    // 描述会被 agent 当成 MCP 工具描述读 —— 因此必填，且允许换行。
    if hook.description.trim().is_empty() || hook.description.len() > MAX_HOOK_DESCRIPTION_BYTES {
        return Err(ManifestError::Invalid(format!(
            "{field}.description must be non-empty and at most {MAX_HOOK_DESCRIPTION_BYTES} bytes"
        )));
    }
    if let Some(schema) = &hook.input_schema {
        let is_object = schema.get("type").and_then(serde_json::Value::as_str) == Some("object");
        if !is_object {
            return Err(ManifestError::Invalid(format!(
                "{field}.input_schema.type must be object"
            )));
        }
    }
    let triggers = validate_triggers(hook, field)?;
    validate_events(hook, field, manifest_scopes, &triggers)?;
    validate_schedule(hook, field, &triggers)?;
    validate_hook_transport(hook, field, manifest_scopes)?;
    if hook.timeout_ms != 0 && !(100..=30000).contains(&hook.timeout_ms) {
        return Err(ManifestError::Invalid(format!(
            "{field}.timeout_ms must be between 100 and 30000"
        )));
    }
    Ok(())
}

/// 触发者列表（返回 `"ui"` 等字面量集合，供 events/schedule 段判定）。
fn validate_triggers<'a>(hook: &'a Hook, field: &str) -> Result<BTreeSet<&'a str>, ManifestError> {
    if hook.triggers.is_empty() {
        return Err(ManifestError::Invalid(format!(
            "{field}.triggers must not be empty"
        )));
    }
    let mut seen = BTreeSet::new();
    for trigger in &hook.triggers {
        if crate::capabilities::HookTrigger::parse(trigger).is_none() {
            return Err(ManifestError::Invalid(format!(
                "{field}.triggers contains unsupported trigger {trigger:?}"
            )));
        }
        if !seen.insert(trigger.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{field}.triggers contains duplicate trigger {trigger:?}"
            )));
        }
    }
    Ok(seen)
}

/// 事件订阅（上游 `events` 段）。
fn validate_events(
    hook: &Hook,
    field: &str,
    manifest_scopes: &[String],
    triggers: &BTreeSet<&str>,
) -> Result<(), ManifestError> {
    if !triggers.contains("event") {
        if hook.events.is_empty() {
            return Ok(());
        }
        return Err(ManifestError::Invalid(format!(
            "{field}.events requires the event trigger"
        )));
    }
    if hook.events.is_empty() {
        return Err(ManifestError::Invalid(format!(
            "{field}.events must not be empty when the event trigger is declared"
        )));
    }
    let mut seen = BTreeSet::new();
    for event in &hook.events {
        if !is_known_event(event) {
            return Err(ManifestError::Invalid(format!(
                "{field}.events contains unsupported event {event:?}"
            )));
        }
        if !seen.insert(event) {
            return Err(ManifestError::Invalid(format!(
                "{field}.events contains duplicate event {event:?}"
            )));
        }
        if let Some(required) = event_read_scope(event) {
            if !manifest_scopes.iter().any(|scope| scope == required) {
                return Err(ManifestError::Invalid(format!(
                    "{field}.events subscribes to {event:?}, which delivers content requiring the {required} scope"
                )));
            }
        }
    }
    Ok(())
}

/// 计划段（上游 `schedule` 段 + `validateHookSchedule`）。
fn validate_schedule(
    hook: &Hook,
    field: &str,
    triggers: &BTreeSet<&str>,
) -> Result<(), ManifestError> {
    if !triggers.contains("schedule") {
        if hook.schedule.is_some() {
            return Err(ManifestError::Invalid(format!(
                "{field}.schedule requires the schedule trigger"
            )));
        }
        return Ok(());
    }
    let Some(schedule) = &hook.schedule else {
        return Err(ManifestError::Invalid(format!(
            "{field}.schedule is required when the schedule trigger is declared"
        )));
    };
    if hook.transport.kind != HookTransportKind::Http.as_str() {
        return Err(ManifestError::Invalid(format!(
            "{field}.schedule only supports the http transport"
        )));
    }
    let schedule_field = format!("{field}.schedule");
    let expression = schedule.cron.trim();
    if expression.is_empty() {
        return Err(ManifestError::Invalid(format!(
            "{schedule_field}.cron must not be empty"
        )));
    }
    // 时区是**独立字段**：除了保持公开形态单一，拒掉内联前缀还能绕开 robfig/cron 的
    // 畸形前缀 panic 路径，并避免两个时区声明各说一套。
    if expression.starts_with("TZ=") || expression.starts_with("CRON_TZ=") {
        return Err(ManifestError::Invalid(format!(
            "{schedule_field}.cron must not contain an inline timezone"
        )));
    }
    let parsed = match cron::parse(expression) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Err(ManifestError::Invalid(format!(
                "{schedule_field}.cron must be a standard five-field cron expression: {error}"
            )));
        }
    };
    if let Err(error) = parsed.validate_min_interval() {
        return Err(ManifestError::Invalid(format!(
            "{schedule_field}.cron {error}"
        )));
    }
    if schedule.timezone.trim().is_empty() {
        return Err(ManifestError::Invalid(format!(
            "{schedule_field}.timezone must not be empty"
        )));
    }
    if !is_timezone_name(&schedule.timezone) {
        return Err(ManifestError::Invalid(format!(
            "{schedule_field}.timezone is invalid: {:?}",
            schedule.timezone
        )));
    }
    Ok(())
}

/// 传输（上游 `validateHookTransport`）。
fn validate_hook_transport(
    hook: &Hook,
    field: &str,
    manifest_scopes: &[String],
) -> Result<(), ManifestError> {
    if HookTransportKind::parse(&hook.transport.kind).is_none() {
        return Err(ManifestError::Invalid(format!(
            "{field}.transport.type is unsupported: {:?}",
            hook.transport.kind
        )));
    }
    let host = parse_https_host(&format!("{field}.transport.url"), &hook.transport.url)?;
    // **精确** host，绝不后缀匹配：同意屏每个 scope 渲染一行（「把数据发给 example.com」），
    // 同一份 scope 又变成 iframe 的 CSP `connect-src`（也是精确 host）。后缀匹配会让同一个
    // scope 在两处含义不同。需要子域的插件就自己声明 `net:api.example.com`。
    let host = normalise_host(&host);
    if scope::net_domains(manifest_scopes).contains(&host) {
        return Ok(());
    }
    Err(ManifestError::Invalid(format!(
        "{field}.transport.url host {host:?} is not covered by a net: scope"
    )))
}

/// 资源列表（上游 `validateContributions` 的 resources 段）。
fn validate_resources(resources: &[Resource]) -> Result<(), ManifestError> {
    let mut keys = BTreeSet::new();
    for (index, resource) in resources.iter().enumerate() {
        let field = format!("contributes.resources[{index}]");
        if ResourceType::parse(&resource.kind) != Some(ResourceType::Skill) {
            return Err(ManifestError::Invalid(format!(
                "{field}.type is unsupported: {:?}",
                resource.kind
            )));
        }
        if !is_contribution_key(&resource.key) {
            return Err(ManifestError::Invalid(format!("{field}.key is invalid")));
        }
        if !keys.insert(&resource.key) {
            return Err(ManifestError::Invalid(format!(
                "duplicate resource key {:?}",
                resource.key
            )));
        }
        validate_relative_path(&format!("{field}.entry"), &resource.entry)?;
        let want = format!("skills/{}/SKILL.md", resource.key);
        if resource.entry != want {
            return Err(ManifestError::Invalid(format!(
                "{field}.entry must be {want:?}"
            )));
        }
    }
    Ok(())
}

/// 插件键：反向 DNS（≥2 段），整体 ≤255 字节。
fn validate_plugin_key(key: &str) -> Result<(), String> {
    if key.len() > 255 {
        return Err("key exceeds 255 bytes".to_owned());
    }
    let segments: Vec<&str> = key.split('.').collect();
    if segments.len() < 2 {
        return Err("key must use a reverse-DNS namespace".to_owned());
    }
    for segment in segments {
        if !is_plugin_key_segment(segment) {
            return Err(format!("key contains invalid segment {segment:?}"));
        }
    }
    Ok(())
}

/// `^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$`
fn is_plugin_key_segment(segment: &str) -> bool {
    let mut parts = segment.split('-');
    let Some(first) = parts.next() else {
        return false;
    };
    let first_bytes = first.as_bytes();
    let starts_with_letter = first_bytes.first().is_some_and(u8::is_ascii_lowercase);
    if !starts_with_letter {
        return false;
    }
    if !first_bytes.iter().all(|byte| is_lower_alnum(*byte)) {
        return false;
    }
    parts.all(|part| !part.is_empty() && part.bytes().all(is_lower_alnum))
}

/// `^[a-z][a-z0-9]*(?:[_-][a-z0-9]+)*$`
fn is_contribution_key(key: &str) -> bool {
    let mut parts = key.split(['-', '_']);
    let Some(first) = parts.next() else {
        return false;
    };
    let first_bytes = first.as_bytes();
    if !first_bytes.first().is_some_and(u8::is_ascii_lowercase) {
        return false;
    }
    if !first_bytes.iter().all(|byte| is_lower_alnum(*byte)) {
        return false;
    }
    parts.all(|part| !part.is_empty() && part.bytes().all(is_lower_alnum))
}

const fn is_lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// 显示文本：非空、无首尾空白、单行、≤`max_bytes`（上游 `validateDisplayText`）。
fn validate_display_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!(
            "{field} must be non-empty without surrounding whitespace"
        ));
    }
    if value.contains(['\r', '\n']) {
        return Err(format!("{field} must be single-line"));
    }
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds {max_bytes} bytes"));
    }
    Ok(())
}

/// 包内相对路径：≤1024 字节、只允许 `[A-Za-z0-9._-]` 与 `/`、拒绝 `.` / `..` 段。
fn validate_relative_path(field: &str, value: &str) -> Result<(), ManifestError> {
    if value.is_empty() || value.len() > 1024 {
        return Err(ManifestError::Invalid(format!(
            "{field} must be a relative path of at most 1024 bytes"
        )));
    }
    if !value.split('/').all(is_relative_path_segment) {
        return Err(ManifestError::Invalid(format!(
            "{field} must be a relative path without protocol, leading slash, or traversal"
        )));
    }
    if value
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(ManifestError::Invalid(format!(
            "{field} must not contain path traversal"
        )));
    }
    Ok(())
}

/// 单个路径段：非空且只含 `[A-Za-z0-9._-]`。
fn is_relative_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 明文 HTTPS URL（上游 `validateHTTPSURL`）：≤2048 字节、scheme `https`、有 host、
/// **无** userinfo、**无** fragment。返回原始 host（未小写）。
fn parse_https_host(field: &str, value: &str) -> Result<String, ManifestError> {
    if value.len() > 2048 {
        return Err(ManifestError::Invalid(format!(
            "{field} exceeds 2048 bytes"
        )));
    }
    let plain = || ManifestError::Invalid(format!("{field} must be a plain HTTPS URL"));
    let trimmed = value.trim();
    if trimmed
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(plain());
    }
    let Some((scheme, rest)) = trimmed.split_once(':') else {
        return Err(plain());
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(plain());
    }
    let Some(authority_and_path) = rest.strip_prefix("//") else {
        return Err(plain());
    };
    // 只取 authority：到第一个 `/`、`?` 或 `#` 为止。
    let end = authority_and_path
        .find(['/', '?', '#'])
        .unwrap_or(authority_and_path.len());
    let authority = &authority_and_path[..end];
    let target = &authority_and_path[end..];
    if target.contains('#') {
        return Err(plain());
    }
    if authority.contains('@') {
        return Err(plain());
    }
    let host = host_without_port(authority).ok_or_else(plain)?;
    if host.is_empty() {
        return Err(plain());
    }
    Ok(host.to_owned())
}

/// 校验一个 HTTPS URL 并丢弃 host（`author.url` 这类只需要「合法」的场合）。
fn validate_https_url(field: &str, value: &str) -> Result<(), ManifestError> {
    parse_https_host(field, value).map(|_| ())
}

/// 去掉 `authority` 里的端口（IPv6 字面量带方括号），返回 host 本身。
fn host_without_port(authority: &str) -> Option<&str> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let tail_ok = tail.is_empty()
            || tail.strip_prefix(':').is_some_and(|port| {
                !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
            });
        return tail_ok.then_some(host);
    }
    match authority.split_once(':') {
        Some((host, port)) => {
            let port_ok = !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit());
            port_ok.then_some(host)
        }
        None => Some(authority),
    }
}

/// host 比较形态：小写 + 去尾点（上游 `strings.ToLower(strings.TrimSuffix(...))`）。
fn normalise_host(host: &str) -> String {
    host.to_ascii_lowercase().trim_end_matches('.').to_owned()
}

/// 语义化版本（上游 `semverPattern` 的逐字等价实现）。
#[must_use]
pub fn is_semver(value: &str) -> bool {
    let mut rest = value;
    let mut build = None;
    if let Some((head, tail)) = rest.split_once('+') {
        build = Some(tail);
        rest = head;
    }
    let mut prerelease = None;
    if let Some((head, tail)) = rest.split_once('-') {
        prerelease = Some(tail);
        rest = head;
    }
    let core: Vec<&str> = rest.split('.').collect();
    if core.len() != 3 || !core.iter().all(|part| is_semver_number(part)) {
        return false;
    }
    if prerelease.is_some_and(|value| !is_semver_identifier_list(value)) {
        return false;
    }
    if build.is_some_and(|value| !is_semver_identifier_list(value)) {
        return false;
    }
    true
}

/// `0|[1-9][0-9]*`
fn is_semver_number(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'))
}

/// `[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*`
fn is_semver_identifier_list(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// IANA 时区名的**结构**校验（本仓没有 tz 数据库，差异见文件头注）。
///
/// 形态：`Area/Location` 或单段 `UTC`；每段 `[A-Za-z][A-Za-z0-9_+.-]*`；整体 ≤255 字节。
/// 注意 `Etc/GMT+8`、`America/Argentina/Buenos_Aires` 这类多段名是合法的。
#[must_use]
pub fn is_timezone_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    value.split('/').all(|segment| {
        let mut bytes = segment.bytes();
        let Some(first) = bytes.next() else {
            return false;
        };
        first.is_ascii_alphabetic()
            && bytes.all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'.' | b'-')
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
      "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
      "version": "1.2.3", "author": {"name": "Example"},
      "scopes": ["issues:read", "net:example.com"],
      "contributes": {
        "hooks": [{"key": "on_issue", "name": "On issue", "description": "d",
          "triggers": ["event"], "events": ["issue.created"],
          "transport": {"type": "http", "url": "https://example.com/hook"}}]
      }
    }"#;

    fn parse(raw: &str) -> Result<Manifest, ManifestError> {
        parse_manifest(raw.as_bytes())
    }

    fn invalid_message(raw: &str) -> String {
        match parse(raw) {
            Err(ManifestError::Invalid(message)) => message,
            other => panic!("expected invalid, got {other:?}"),
        }
    }

    #[test]
    fn minimal_manifest_parses() {
        let manifest = parse(MINIMAL).expect("parses");
        assert_eq!(manifest.manifest_version, 1);
        assert_eq!(manifest.key, "com.example.demo");
        assert_eq!(manifest.contributes.hooks.len(), 1);
        assert_eq!(manifest.contributes.hooks[0].transport.kind, "http");
        assert!(manifest.config.is_empty());
        assert_eq!(MANIFEST_FILENAME, "multica.plugin.json");
    }

    #[test]
    fn empty_and_oversized_and_trailing_are_rejected_by_json_code() {
        assert_eq!(
            parse_manifest(b"").unwrap_err().code(),
            "plugin_manifest_invalid_json"
        );
        assert_eq!(
            parse_manifest(&vec![b'{'; MAX_MANIFEST_SIZE + 1])
                .unwrap_err()
                .code(),
            "plugin_manifest_invalid_json"
        );
        // 尾部多余内容（上游 rejectTrailingJSON）
        let trailing = format!("{MINIMAL}{MINIMAL}");
        assert_eq!(
            parse(&trailing).unwrap_err().code(),
            "plugin_manifest_invalid_json"
        );
        // 未知字段**不**报错（本仓前向兼容政策）
        let with_unknown =
            MINIMAL.replace("\"name\": \"Demo\"", "\"name\": \"Demo\", \"future\": 1");
        assert!(parse(&with_unknown).is_ok());
        // manifest_version 不是 1（且**不**做迁移）
        let v2 = MINIMAL.replace("\"manifest_version\": 1", "\"manifest_version\": 2");
        assert_eq!(invalid_message(&v2), "manifest_version must be 1");
    }

    #[test]
    fn identity_rules_match_upstream_messages() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "\"name\": \"Demo\"",
                "\"name\": \"\"",
                "name must be non-empty without surrounding whitespace",
            ),
            (
                "\"name\": \"Demo\"",
                "\"name\": \" Demo\"",
                "name must be non-empty without surrounding whitespace",
            ),
            (
                "\"name\": \"Demo\"",
                "\"name\": \"De\\nmo\"",
                "name must be single-line",
            ),
            (
                "\"version\": \"1.2.3\"",
                "\"version\": \"1.2\"",
                "version must be semantic versioning, got \"1.2\"",
            ),
            (
                "\"version\": \"1.2.3\"",
                "\"version\": \"01.2.3\"",
                "version must be semantic versioning, got \"01.2.3\"",
            ),
            (
                "\"key\": \"com.example.demo\"",
                "\"key\": \"demo\"",
                "key must use a reverse-DNS namespace",
            ),
            (
                "\"key\": \"com.example.demo\"",
                "\"key\": \"com.Example.demo\"",
                "key contains invalid segment \"Example\"",
            ),
            (
                "\"key\": \"com.example.demo\"",
                "\"key\": \"com.ex--ample.demo\"",
                "key contains invalid segment \"ex--ample\"",
            ),
            (
                "\"author\": {\"name\": \"Example\"}",
                "\"author\": {\"name\": \"Example\", \"url\": \"http://example.com\"}",
                "author.url must be a plain HTTPS URL",
            ),
            (
                "\"author\": {\"name\": \"Example\"}",
                "\"author\": {\"name\": \"Example\", \"url\": \"https://user@example.com\"}",
                "author.url must be a plain HTTPS URL",
            ),
            (
                "\"author\": {\"name\": \"Example\"}",
                "\"author\": {\"name\": \"Example\", \"url\": \"https://example.com#frag\"}",
                "author.url must be a plain HTTPS URL",
            ),
        ];
        for (from, to, expected) in cases {
            let raw = MINIMAL.replace(from, to);
            assert_eq!(invalid_message(&raw), *expected, "case {to}");
        }
    }

    #[test]
    fn version_length_and_description_carriage_return() {
        let long_version = format!("\"version\": \"1.2.3+{}\"", "a".repeat(MAX_VERSION_LENGTH));
        assert_eq!(
            invalid_message(&MINIMAL.replace("\"version\": \"1.2.3\"", &long_version)),
            format!("version exceeds {MAX_VERSION_LENGTH} bytes")
        );
        let cr = MINIMAL.replace(
            "\"name\": \"Demo\"",
            "\"name\": \"Demo\", \"description\": \"a\\rb\"",
        );
        assert_eq!(
            invalid_message(&cr),
            "description must not contain carriage returns"
        );
    }

    #[test]
    fn semver_matcher_covers_upstream_regex() {
        for ok in [
            "0.0.0",
            "1.2.3",
            "1.2.3-0.3.7",
            "1.2.3-alpha.beta.1",
            "1.2.3+build.1",
            "1.2.3-rc.1+build.2",
            "10.20.30",
        ] {
            assert!(is_semver(ok), "{ok}");
        }
        for bad in [
            "", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.03", "1.2.3-", "1.2.3+", "v1.2.3",
            "1.2.3-+", "1.2.3-á",
        ] {
            assert!(!is_semver(bad), "{bad}");
        }
    }

    #[test]
    fn scopes_are_validated_as_a_closed_set() {
        let raw = MINIMAL.replace(
            "\"issues:read\", \"net:example.com\"",
            "\"issues:read\", \"bogus\"",
        );
        assert_eq!(
            invalid_message(&raw),
            "scopes[1]: unsupported scope \"bogus\""
        );
        let dup = MINIMAL.replace(
            "\"issues:read\", \"net:example.com\"",
            "\"issues:read\", \"issues:read\"",
        );
        assert_eq!(
            invalid_message(&dup),
            "scopes contains duplicate value \"issues:read\""
        );
        let empty = MINIMAL.replace(
            "\"scopes\": [\"issues:read\", \"net:example.com\"],",
            "\"scopes\": [],",
        );
        assert_eq!(invalid_message(&empty), "scopes must not be empty");
    }

    #[test]
    fn config_schema_keeps_declaration_order_and_rejects_duplicates() {
        let raw = MINIMAL.replace(
            "\"contributes\": {",
            r#""config": {
              "zeta": {"type": "string", "label": "Z", "multiline": true},
              "alpha": {"type": "enum", "label": "A", "options": ["x", "y"]},
              "token": {"type": "secret", "label": "Token", "required": true}
            },
            "contributes": {"#,
        );
        let manifest = parse(&raw).expect("parses");
        let keys: Vec<&str> = manifest
            .config
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect();
        assert_eq!(keys, vec!["zeta", "alpha", "token"]);
        assert!(manifest
            .config
            .field("alpha")
            .is_some_and(|field| field.kind == "enum"));
        // 序列化仍然是对象，且键序保住（`serde_json::Map` 默认排序，因此这里手写了 Serialize）
        let encoded = serde_json::to_string(&manifest.config).expect("encode");
        assert_eq!(
            encoded,
            r#"{"zeta":{"type":"string","label":"Z","multiline":true},"alpha":{"type":"enum","label":"A","options":["x","y"]},"token":{"type":"secret","label":"Token","required":true}}"#
        );
        // 往返
        let decoded: ConfigSchema = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, manifest.config);
        // 重复键由 visitor 拦下（`serde_json` 默认会静默取最后一个）
        let duplicate = r#"{"a":{"type":"string","label":"A"},"a":{"type":"string","label":"A"}}"#;
        assert!(serde_json::from_str::<ConfigSchema>(duplicate).is_err());
        // 非对象
        assert!(serde_json::from_str::<ConfigSchema>("[]").is_err());
    }

    #[test]
    fn config_field_rules_match_upstream_messages() {
        let with_config = |body: &str| {
            parse(&MINIMAL.replace(
                "\"contributes\": {",
                &format!("\"config\": {body}, \"contributes\": {{"),
            ))
        };
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"a\": {\"type\": \"string\", \"label\": \"A\", \"options\": [\"x\"]}}, \"contributes\": {",
            )),
            "config.a.options is only valid for enum fields"
        );
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"a\": {\"type\": \"number\", \"label\": \"A\", \"multiline\": true}}, \"contributes\": {",
            )),
            "config.a.multiline is only valid for string fields"
        );
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"a\": {\"type\": \"enum\", \"label\": \"A\"}}, \"contributes\": {",
            )),
            "config.a.options must not be empty for enum fields"
        );
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"a\": {\"type\": \"date\", \"label\": \"A\"}}, \"contributes\": {",
            )),
            "config.a.type is unsupported: \"date\""
        );
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"Bad Key\": {\"type\": \"string\", \"label\": \"A\"}}, \"contributes\": {",
            )),
            "config contains invalid field name \"Bad Key\""
        );
        assert!(with_config(r#"{"a": {"type": "bool", "label": "A"}}"#).is_ok());
    }

    #[test]
    fn surface_rules() {
        let build = |surfaces: &str| {
            format!(
                r#"{{
                  "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
                  "version": "1.2.3", "author": {{"name": "Example"}},
                  "scopes": ["issues:read", "net:example.com"],
                  "contributes": {{"surfaces": [{surfaces}]}}
                }}"#
            )
        };
        let surface_error = |surface: &str| match parse(&build(surface)) {
            Err(ManifestError::Invalid(message)) => message,
            other => panic!("expected invalid for {surface}, got {other:?}"),
        };
        assert!(parse(&build(
            r#"{"key": "panel", "type": "issue_panel", "name": "Panel", "entry": "panel.js", "platforms": ["web"]}"#
        ))
        .is_ok());
        let cases: &[(&str, &str)] = &[
            (
                r#"{"key": "panel", "type": "sidebar_panel", "name": "", "entry": "p.js"}"#,
                "contributes.surfaces[0].name must be non-empty without surrounding whitespace",
            ),
            (
                r#"{"key": "panel", "type": "floating", "name": "P", "entry": "p.js"}"#,
                "contributes.surfaces[0].type is unsupported: \"floating\"",
            ),
            (
                r#"{"key": "panel", "type": "modal", "name": "P", "entry": "p.html"}"#,
                "contributes.surfaces[0].entry must be a .js or .mjs script; the host renders the surface document itself",
            ),
            (
                r#"{"key": "panel", "type": "modal", "name": "P", "entry": "../p.js"}"#,
                "contributes.surfaces[0].entry must not contain path traversal",
            ),
            (
                r#"{"key": "panel", "type": "modal", "name": "P", "entry": "p.js", "platforms": ["ios"]}"#,
                "contributes.surfaces[0].platforms contains unsupported platform \"ios\"",
            ),
            (
                r#"{"key": "panel", "type": "issue_panel", "name": "P", "entry": "panel.js", "platforms": ["web", "web"]}"#,
                "contributes.surfaces[0].platforms contains duplicate platform \"web\"",
            ),
            (
                r#"{"key": "panel", "type": "issue_panel", "name": "P", "entry": "panel.js"}, {"key": "panel", "type": "modal", "name": "P2", "entry": "p2.js"}"#,
                "duplicate surface key \"panel\"",
            ),
        ];
        for (surface, expected) in cases {
            assert_eq!(surface_error(surface), *expected, "{surface}");
        }
    }

    #[test]
    fn hook_rules() {
        let build = |hooks: &str| {
            format!(
                r#"{{
                  "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
                  "version": "1.2.3", "author": {{"name": "Example"}},
                  "scopes": ["issues:read", "net:example.com"],
                  "contributes": {{"hooks": [{hooks}]}}
                }}"#
            )
        };
        let hook_error = |hook: &str| match parse(&build(hook)) {
            Err(ManifestError::Invalid(message)) => message,
            other => panic!("expected invalid for {hook}, got {other:?}"),
        };
        assert!(parse(&build(
            r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#
        ))
        .is_ok());
        let cases: &[(&str, &str)] = &[
            (
                r#"{"key": "k", "name": "K", "description": "  ", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].description must be non-empty and at most 2000 bytes",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": [], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].triggers must not be empty",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "events": ["issue.created"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].events requires the event trigger",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "schedule": {"cron": "* * * * *", "timezone": "UTC"}, "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].schedule requires the schedule trigger",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["event"], "events": ["comment.created"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].events subscribes to \"comment.created\", which delivers content requiring the comments:read scope",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://api.example.com/h"}}"#,
                "contributes.hooks[0].transport.url host \"api.example.com\" is not covered by a net: scope",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "smtp", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].transport.type is unsupported: \"smtp\"",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}, "timeout_ms": 99}"#,
                "contributes.hooks[0].timeout_ms must be between 100 and 30000",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "input_schema": {"type": "array"}, "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].input_schema.type must be object",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}, {"key": "k", "name": "K2", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "duplicate hook key \"k\"",
            ),
        ];
        for (hook, expected) in cases {
            assert_eq!(hook_error(hook), *expected, "{hook}");
        }
    }

    #[test]
    fn schedule_rules_cover_inline_timezone_and_cadence() {
        let with_cron = |cron_expr: &str, timezone: &str| {
            parse(&MINIMAL.replace(
                r#""triggers": ["event"], "events": ["issue.created"],"#,
                &format!(
                    r#""triggers": ["schedule"], "schedule": {{"cron": "{cron_expr}", "timezone": "{timezone}"}},"#
                ),
            ))
        };
        assert!(with_cron("0 * * * *", "UTC").is_ok());
        assert!(with_cron("*/5 * * * *", "America/Argentina/Buenos_Aires").is_ok());
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                r#""triggers": ["event"], "events": ["issue.created"],"#,
                r#""triggers": ["schedule"], "schedule": {"cron": "TZ=UTC 0 * * * *", "timezone": "UTC"},"#,
            )),
            "contributes.hooks[0].schedule.cron must not contain an inline timezone"
        );
        let too_frequent = match with_cron("* * * * *", "UTC") {
            Err(ManifestError::Invalid(message)) => message,
            other => panic!("expected invalid, got {other:?}"),
        };
        assert_eq!(
            too_frequent,
            "contributes.hooks[0].schedule.cron must not run more often than every five minutes"
        );
        let bad_timezone = match with_cron("0 * * * *", "Not a zone!") {
            Err(ManifestError::Invalid(message)) => message,
            other => panic!("expected invalid, got {other:?}"),
        };
        assert_eq!(
            bad_timezone,
            "contributes.hooks[0].schedule.timezone is invalid: \"Not a zone!\""
        );
        assert!(is_timezone_name("UTC"));
        assert!(is_timezone_name("Etc/GMT+8"));
        assert!(!is_timezone_name(""));
        assert!(!is_timezone_name("a/b/c d"));
    }

    #[test]
    fn resource_rules() {
        let with_resource = |resource: &str| {
            parse(&MINIMAL.replace(
                "\"hooks\": [",
                &format!("\"resources\": [{resource}], \"hooks\": ["),
            ))
        };
        assert!(with_resource(
            r#"{"type": "skill", "key": "demo", "entry": "skills/demo/SKILL.md"}"#
        )
        .is_ok());
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"hooks\": [",
                "\"resources\": [{\"type\": \"skill\", \"key\": \"demo\", \"entry\": \"skills/other/SKILL.md\"}], \"hooks\": [",
            )),
            "contributes.resources[0].entry must be \"skills/demo/SKILL.md\""
        );
        assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"hooks\": [",
                "\"resources\": [{\"type\": \"widget\", \"key\": \"demo\", \"entry\": \"skills/demo/SKILL.md\"}], \"hooks\": [",
            )),
            "contributes.resources[0].type is unsupported: \"widget\""
        );
    }

    #[test]
    fn contributes_must_not_be_empty() {
        let empty = r#"{
          "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
          "version": "1.0.0", "author": {"name": "Example"},
          "scopes": ["issues:read"], "contributes": {}
        }"#;
        assert_eq!(
            invalid_message(empty),
            "contributes must declare at least one surface, hook, or resource"
        );
    }

    #[test]
    fn https_url_parsing_matches_go_trimming() {
        assert!(parse_https_host("u", "https://example.com/x?y=1").is_ok());
        assert!(parse_https_host("u", " HTTPS://EXAMPLE.com ").is_ok());
        assert!(parse_https_host("u", "https://[::1]:8443/x").is_ok());
        // 上游 `url.Parse(strings.TrimSpace(value))` ⇒ 首尾空白被吃掉是**接受**。
        assert!(parse_https_host("u", "https://example.com\n").is_ok());
        for bad in [
            "http://example.com",
            "https://",
            "https://:8443",
            "https://user@example.com",
            "https://example.com/#f",
            "https://exa mple.com",
            "example.com",
        ] {
            assert!(parse_https_host("u", bad).is_err(), "{bad}");
        }
        assert_eq!(normalise_host("Example.COM."), "example.com");
        assert_eq!(
            parse_https_host("u", "https://EXAMPLE.com./x").unwrap(),
            "EXAMPLE.com."
        );
    }
}
