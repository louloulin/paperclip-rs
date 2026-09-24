//! `manifest.rs` 的**校验规则与词法/URL 助手**（兄弟模块，拆出以守门 ⑩：本体 800 行硬上限）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-5 的 `preview` 入口复用同一批判据，只读。
//! - **上游**：`pkg/plugincontract/manifest.go` 的 `validate*` 与 `is*`/`parse*` 助手。
//! - **为什么拆**：这些行数是**逐条判据的数量**而不是复杂度；挤在一个文件里会越过门 ⑩
//!   （单文件 800 行硬上限），而 R7 的并发冲突面正是「一个大文件被多片改」。
//!   拆分按**引用点**而非行数：调用方只有 `Manifest::validate*`（`manifest.rs`）与
//!   `manifest/tests.rs`，两者都在同一父模块下 ⇒ 本文件的条目一律 `pub(crate)`。
//! - **不做什么**：不改判据 —— 消息文本、顺序、边界值都与搬来之前逐字一致。

use super::{
    cron, event_read_scope, is_known_event, scope, BTreeSet, ConfigField, Hook, HookTransportKind,
    ManifestError, Resource, ResourceType, Surface, SurfaceType, CONFIG_BOOL, CONFIG_ENUM,
    CONFIG_NUMBER, CONFIG_SECRET, CONFIG_STRING, MAX_HOOK_DESCRIPTION_BYTES,
};

/// 一个配置字段（上游 `validateConfig` 的循环体）。
pub(crate) fn validate_config_field(field: &ConfigField) -> Result<(), ManifestError> {
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
pub(crate) fn is_script_entry(entry: &str) -> bool {
    entry
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension == "js" || extension == "mjs")
}

/// 面列表（上游 `validateContributions` 的 surfaces 段）。
pub(crate) fn validate_surfaces(surfaces: &[Surface]) -> Result<(), ManifestError> {
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
pub(crate) fn validate_hook(
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
pub(crate) fn validate_events(
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
pub(crate) fn validate_schedule(
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
pub(crate) fn validate_hook_transport(
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
pub(crate) fn validate_resources(resources: &[Resource]) -> Result<(), ManifestError> {
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
pub(crate) fn validate_plugin_key(key: &str) -> Result<(), String> {
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
pub(crate) fn is_plugin_key_segment(segment: &str) -> bool {
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
pub(crate) fn is_contribution_key(key: &str) -> bool {
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
pub(crate) fn validate_display_text(
    field: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), String> {
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
pub(crate) fn validate_relative_path(field: &str, value: &str) -> Result<(), ManifestError> {
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
pub(crate) fn is_relative_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 明文 HTTPS URL（上游 `validateHTTPSURL`）：≤2048 字节、scheme `https`、有 host、
/// **无** userinfo、**无** fragment。返回原始 host（未小写）。
pub(crate) fn parse_https_host(field: &str, value: &str) -> Result<String, ManifestError> {
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
pub(crate) fn validate_https_url(field: &str, value: &str) -> Result<(), ManifestError> {
    parse_https_host(field, value).map(|_| ())
}

/// 去掉 `authority` 里的端口（IPv6 字面量带方括号），返回 host 本身。
pub(crate) fn host_without_port(authority: &str) -> Option<&str> {
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
pub(crate) fn normalise_host(host: &str) -> String {
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
pub(crate) fn is_semver_number(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'))
}

/// `[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*`
pub(crate) fn is_semver_identifier_list(value: &str) -> bool {
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
