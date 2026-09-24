//! 运行时（provider 原生）MCP 配置的读取、去敏 inventory、与 agent 级配置的本地合并。
//!
//! - **上游**：`internal/daemon/runtime_mcp.go`（578 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 为什么合并发生在 daemon 里
//!
//! agent 的 MCP 配置里可能有 runtime 侧的 URL、headers、command、env 值。上游把**合并**
//! 放在本机做：只有去敏后的摘要（名字 / 传输档 / 来源 / 开关）才上行。本仓照做 ——
//! [`RuntimeLocalMcpServerSummary`] 是唯一出口形状，它**没有**任何值字段。
//!
//! ## 三层语义
//!
//! 1. [`load_runtime_mcp_server_configs`]：读某个 provider **自己**的配置文件，返回**含
//!    密钥**的条目（只在本进程里用于合并，绝不上行/落日志）。
//! 2. [`merge_runtime_and_agent_mcp_config`]：runtime 层做底、agent 层同名覆盖。agent 侧
//!    配置为 `null` / 缺省时**原样返回**（让 provider 的原生继承路径继续生效）；只要 agent
//!    给了配置（哪怕是空 `mcpServers`），就切到「合并后的任务本地配置」。
//! 3. [`list_runtime_local_mcp_servers`]：UI 用的去敏清单。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - **TOML 未接**（`codex`）：需要 TOML 解析器，而本波三方依赖在 M6-0 冻结 ⇒ 走
//!   [`McpConfigError::TomlUnsupported`] 这条**可区分**的错误，而不是静默返回空表。
//! - **claude 的插件 MCP 段未接**：上游从 `claude_plugins.go` 取插件 manifest，该文件不在
//!   本 slice 写集 ⇒ 插件贡献的条目（`Claude Plugin · <name>` 来源）本 slice 不产出。
//! - `kimi` / `omp` 只在 **inventory** 里出现（上游 `loadRuntimeMcpServerConfigs` 故意不收
//!   它们：`kimi acp` 会把文件与 session/new 里发的 `mcpServers` 再合并一次，于是每个
//!   用户 server 会被拉起两遍）。这条分工本 slice 照抄。
//! - `codebuddy` 同样**只在 inventory** 里（上游注释：CodeBuddy 自己会加载 user/project/local
//!   三个作用域，daemon 再合一次只会重复，还会丢掉作用域优先级与批准闸）。
//! - JSONC 的 `strip` 是本模块自己实现的（上游 `stripJSONC` 逐行移植）；**不改**字符串里的
//!   内容、只把注释与「closer 前那一个逗号」**置空**（总长度不变 ⇒ 解析错位的偏移仍然指向
//!   用户写的那个字节）。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::skill::{env_or, non_empty_env, user_home};

/// 配置文件格式（上游 `unmarshalRuntimeMcpConfig` 的 `format` 参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigFormat {
    /// 严格 JSON。
    Json,
    /// JSON + 注释 + 尾逗号（CodeBuddy / `CodeArts` 用）。
    Jsonc,
    /// TOML（codex 用；本 slice 未实现，见模块文档）。
    Toml,
}

/// runtime MCP 读取过程中的失败。
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    #[error("runtime mcp: {op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime mcp: parse runtime MCP config: {0}")]
    Parse(String),
    #[error("runtime mcp: unterminated block comment")]
    UnterminatedBlockComment,
    #[error(
        "runtime mcp: TOML config is not supported in this build (codex `config.toml`); \
         see docs/32 §9.9"
    )]
    TomlUnsupported,
    #[error("runtime mcp: {0}")]
    Shape(String),
}

impl McpConfigError {
    fn io(op: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// 本模块的结果别名。
pub type Result<T, E = McpConfigError> = std::result::Result<T, E>;

/// UI 用的去敏 inventory 行（上游 `runtimeLocalMcpServerSummary`）。
///
/// ⚠️ 这个类型会离开用户机器。**不要**往里加 command 参数、URL、headers 或 env 值。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeLocalMcpServerSummary {
    /// server 名。
    pub name: String,
    /// 传输档（`stdio` / `http` / `sse` / `unknown`；空则省略）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transport: String,
    /// 来源标签（`User config` / `Claude Plugin · <name>`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// 是否启用（`false` 用 `enabled: false` 表达，**不省略**）。
    pub enabled: bool,
}

/// 一个 provider 的 runtime 配置面：不支持与「支持但没读到」必须分得开。
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderMcpConfig {
    /// 该 provider 没有 runtime MCP 配置面（上游 `return map, false, nil`）。
    Unsupported,
    /// 支持；`Map` 可能为空。
    Supported(Map<String, Value>),
}

/// `CodeArts` 的用户配置文件（上游 `codeArtsUserConfigPath`）：`.json` 优先于 `.jsonc`。
#[must_use]
pub fn code_arts_user_config_path(home: &Path) -> PathBuf {
    let config_dir = home.join(".codeartsdoer");
    let candidates = [
        config_dir.join("codearts_cli.json"),
        config_dir.join("codearts_cli.jsonc"),
    ];
    for candidate in &candidates {
        if candidate.is_file() {
            return candidate.clone();
        }
    }
    candidates[0].clone()
}

/// `CodeBuddy` 的用户级 MCP 文件（上游 `codebuddyUserMcpConfigPath`）。
///
/// 配置目录 = `$CODEBUDDY_CONFIG_DIR`（缺省 `~/.codebuddy`）；候选是**回落链**而不是合并：
/// `<dir>/.mcp.json` → `<dir>/mcp.json` → `~/.codebuddy.json`。都不存在时返回第一个候选，
/// 让调用方那次读以 `NotFound` 结束（与别的 provider 的「没有 runtime server」一致）。
#[must_use]
pub fn codebuddy_user_mcp_config_path(home: &Path) -> PathBuf {
    let config_dir = non_empty_env("CODEBUDDY_CONFIG_DIR")
        .map_or_else(|| home.join(".codebuddy"), PathBuf::from);
    let candidates = [
        config_dir.join(".mcp.json"),
        config_dir.join("mcp.json"),
        home.join(".codebuddy.json"),
    ];
    for candidate in &candidates {
        if candidate.is_file() {
            return candidate.clone();
        }
    }
    candidates[0].clone()
}

/// 解析一份 runtime MCP 配置（上游 `unmarshalRuntimeMcpConfig`）。
pub fn unmarshal_runtime_mcp_config(
    raw: &[u8],
    format: McpConfigFormat,
) -> Result<Map<String, Value>> {
    match format {
        McpConfigFormat::Toml => Err(McpConfigError::TomlUnsupported),
        McpConfigFormat::Jsonc => {
            let stripped = strip_jsonc(raw)?;
            serde_json::from_slice::<Map<String, Value>>(&stripped)
                .map_err(|err| McpConfigError::Parse(err.to_string()))
        }
        McpConfigFormat::Json => serde_json::from_slice::<Map<String, Value>>(raw)
            .map_err(|err| McpConfigError::Parse(err.to_string())),
    }
}

/// 把 JSONC 改写成严格 JSON（上游 `stripJSONC` 的逐行移植）。
///
/// 三条性质必须保持（上游专门写了注释，因为 `CodeBuddy` 的 MCP 文件依赖它们）：
/// 1. **字符串字面量原样拷贝** —— 命令行参数里的 `//` 或逗号不会被当注释/分隔符；
/// 2. **输出与输入等长** —— 注释与被丢掉的逗号是**置空**而不是删除，于是解析错误的偏移
///    仍然指向用户实际写的那个字节；
/// 3. **只清掉 closer 之前的那一个逗号** —— `[1,,,]` 依旧是非法 JSON，不会被「修好」。
///
/// 未闭合的 `/*` **报错**（而不是一路置空到文件尾）：否则 Agent > MCP 页会列出 `CodeBuddy`
/// 自己都拒收的文件里的 server。
pub fn strip_jsonc(raw: &[u8]) -> Result<Vec<u8>, McpConfigError> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    // `out` 里唯一那个「还可以被删掉」的逗号下标；任何值 token 都会把它重置。
    let mut last_comma: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;

    while index < raw.len() {
        let byte = raw[index];

        if in_string {
            out.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }

        match byte {
            b'"' => {
                in_string = true;
                last_comma = None;
                out.push(byte);
                index += 1;
            }
            b'/' if raw.get(index + 1) == Some(&b'/') => {
                // 行注释：整行置空，保留换行本身。
                while index < raw.len() && raw[index] != b'\n' {
                    out.push(b' ');
                    index += 1;
                }
            }
            b'/' if raw.get(index + 1) == Some(&b'*') => {
                // 先吃掉开头的 `/*`，于是 `/*/` 不会拿自己那个 `*` 当收尾。
                out.push(b' ');
                out.push(b' ');
                index += 2;
                let mut closed = false;
                while index < raw.len() {
                    if raw[index] == b'*' && raw.get(index + 1) == Some(&b'/') {
                        out.push(b' ');
                        out.push(b' ');
                        // 吃掉 `*`；循环末尾的 `index += 1` 吃掉 `/`。
                        index += 1;
                        closed = true;
                        break;
                    }
                    out.push(if raw[index] == b'\n' { b'\n' } else { b' ' });
                    index += 1;
                }
                if !closed {
                    return Err(McpConfigError::UnterminatedBlockComment);
                }
                index += 1;
            }
            b',' => {
                last_comma = Some(out.len());
                out.push(byte);
                index += 1;
            }
            b'}' | b']' => {
                if let Some(position) = last_comma.take() {
                    out[position] = b' ';
                }
                out.push(byte);
                index += 1;
            }
            b' ' | b'\t' | b'\n' | b'\r' => {
                out.push(byte);
                index += 1;
            }
            _ => {
                last_comma = None;
                out.push(byte);
                index += 1;
            }
        }
    }
    Ok(out)
}

/// 读某个 provider 自己的 runtime MCP 配置（上游 `loadRuntimeMcpServerConfigs`）。
///
/// 返回**含密钥**的条目；调用方只允许把它用于本进程内的合并。
pub fn load_runtime_mcp_server_configs(provider: &str) -> Result<ProviderMcpConfig> {
    let home = user_home().map_err(|_| {
        McpConfigError::Shape("cannot resolve user home for runtime MCP config".to_string())
    })?;
    let Some((path, key, format)) = runtime_config_source(provider, &home, false)? else {
        return Ok(ProviderMcpConfig::Unsupported);
    };

    let mut servers = Map::new();
    match fs::read(&path) {
        Ok(raw) => {
            let config = unmarshal_runtime_mcp_config(&raw, format)?;
            if let Some(configured) = nested_runtime_mcp_map(&config, &key) {
                for (name, entry) in configured {
                    servers.insert(
                        name.clone(),
                        normalize_runtime_mcp_entry(provider, entry.clone()),
                    );
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(McpConfigError::io("read runtime MCP config", &path, err)),
    }

    // ⚠️ 上游这里还会把 `claude` 的插件 MCP 条目补进空位（用户配置优先）；本 slice 不接
    // （`claude_plugins.go` 不在写集），见模块文档。
    Ok(ProviderMcpConfig::Supported(servers))
}

/// provider → (路径, 取值的键, 格式)；`Ok(None)` = 该 provider 没有 runtime MCP 面。
///
/// `inventory_only=false` 时**只列**真正参与合并的 provider；`true` 时用 inventory 的那张
/// 表（多出 `codebuddy` / `kimi` / `omp`，见模块文档）。
fn runtime_config_source(
    provider: &str,
    home: &Path,
    inventory_only: bool,
) -> Result<Option<(PathBuf, String, McpConfigFormat)>> {
    let source = if inventory_only {
        match provider {
            "claude" => (
                home.join(".claude.json"),
                "mcpServers".to_string(),
                McpConfigFormat::Json,
            ),
            "codearts" => (
                code_arts_user_config_path(home),
                "mcp".to_string(),
                McpConfigFormat::Jsonc,
            ),
            "codebuddy" => (
                codebuddy_user_mcp_config_path(home),
                "mcpServers".to_string(),
                McpConfigFormat::Jsonc,
            ),
            // 仅 inventory：`kimi acp` 会把这份文件与 session/new 里发的 `mcpServers`
            // 再合并一次 ⇒ 这里再合一次会让每个用户 server 被拉起两遍。
            "kimi" => {
                let kimi_home = env_or("KIMI_CODE_HOME", || home.join(".kimi-code"));
                (
                    kimi_home.join("mcp.json"),
                    "mcpServers".to_string(),
                    McpConfigFormat::Json,
                )
            }
            // 仅 inventory：omp 的发现是多级优先链，这里只读用户作用域入口。
            "omp" => (
                home.join(".omp").join("agent").join("mcp.json"),
                "mcpServers".to_string(),
                McpConfigFormat::Json,
            ),
            "codex" => (
                env_or("CODEX_HOME", || home.join(".codex")).join("config.toml"),
                "mcp_servers".to_string(),
                McpConfigFormat::Toml,
            ),
            "cursor" => (
                home.join(".cursor").join("mcp.json"),
                "mcpServers".to_string(),
                McpConfigFormat::Json,
            ),
            "opencode" => {
                let config_home = env_or("XDG_CONFIG_HOME", || home.join(".config"));
                (
                    config_home.join("opencode").join("opencode.json"),
                    "mcp".to_string(),
                    McpConfigFormat::Json,
                )
            }
            "openclaw" => (
                openclaw_config_path(home),
                "mcp.servers".to_string(),
                McpConfigFormat::Json,
            ),
            _ => return Ok(None),
        }
    } else {
        match provider {
            "claude" => (
                home.join(".claude.json"),
                "mcpServers".to_string(),
                McpConfigFormat::Json,
            ),
            "codearts" => (
                code_arts_user_config_path(home),
                "mcp".to_string(),
                McpConfigFormat::Jsonc,
            ),
            // `codebuddy` 故意缺席：它自己会加载三个作用域，daemon 再合一次只会重复，
            // 还会丢掉作用域优先级与项目作用域的批准闸。
            "codex" => (
                env_or("CODEX_HOME", || home.join(".codex")).join("config.toml"),
                "mcp_servers".to_string(),
                McpConfigFormat::Toml,
            ),
            "cursor" => (
                home.join(".cursor").join("mcp.json"),
                "mcpServers".to_string(),
                McpConfigFormat::Json,
            ),
            "opencode" => {
                let config_home = env_or("XDG_CONFIG_HOME", || home.join(".config"));
                (
                    config_home.join("opencode").join("opencode.json"),
                    "mcp".to_string(),
                    McpConfigFormat::Json,
                )
            }
            "openclaw" => (
                openclaw_config_path(home),
                "mcp.servers".to_string(),
                McpConfigFormat::Json,
            ),
            _ => return Ok(None),
        }
    };
    Ok(Some(source))
}

/// `OpenClaw` 的配置路径：`$CLAWDBOT_CONFIG_PATH` 优先，否则 `$OPENCLAW_STATE_DIR`（缺省
/// `~/.openclaw`）下的 `openclaw.json`。
fn openclaw_config_path(home: &Path) -> PathBuf {
    if let Some(path) = non_empty_env("CLAWDBOT_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    let state_dir = env_or("OPENCLAW_STATE_DIR", || home.join(".openclaw"));
    state_dir.join("openclaw.json")
}

/// Codex 的 `http_headers` → `headers` 归一（上游 `normalizeRuntimeMcpEntry`）。
///
/// 保留原键，让 Codex 自己的少见设置在渲染回去时不丢。
#[must_use]
pub fn normalize_runtime_mcp_entry(provider: &str, value: Value) -> Value {
    let Value::Object(mut entry) = value else {
        return value;
    };
    if provider != "codex" {
        return Value::Object(entry);
    }
    if let Some(headers) = entry.get("http_headers").cloned() {
        entry.entry("headers".to_string()).or_insert(headers);
    }
    if entry.contains_key("url") {
        entry
            .entry("type".to_string())
            .or_insert_with(|| Value::String("http".to_string()));
    }
    Value::Object(entry)
}

/// UI 用的去敏清单（上游 `listRuntimeLocalMcpServers`）。
pub fn list_runtime_local_mcp_servers(
    provider: &str,
) -> Result<Option<Vec<RuntimeLocalMcpServerSummary>>> {
    let home = user_home().map_err(|_| {
        McpConfigError::Shape("cannot resolve user home for runtime MCP inventory".to_string())
    })?;
    let Some((path, key, format)) = runtime_config_source(provider, &home, true)? else {
        return Ok(None);
    };

    let mut out: Vec<RuntimeLocalMcpServerSummary> = Vec::new();
    match fs::read(&path) {
        Ok(raw) => {
            let config = unmarshal_runtime_mcp_config(&raw, format)?;
            if let Some(servers) = nested_runtime_mcp_map(&config, &key) {
                out.extend(runtime_mcp_summaries(servers, "User config"));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(McpConfigError::io("read runtime MCP config", &path, err)),
    }

    // ⚠️ 上游这里追加 claude 插件贡献的 server；本 slice 不接（见模块文档）。

    // 用户配置在重名时胜出；插件条目只填空位。这里做一次保序去重。
    let mut deduped: Vec<RuntimeLocalMcpServerSummary> = Vec::with_capacity(out.len());
    for server in out {
        if deduped.iter().any(|seen| seen.name == server.name) {
            continue;
        }
        deduped.push(server);
    }
    deduped.sort_by_key(|server| server.name.to_ascii_lowercase());
    Ok(Some(deduped))
}

/// 条目 → 摘要（上游 `runtimeMcpSummaries`）。
#[must_use]
pub fn runtime_mcp_summaries(
    servers: &Map<String, Value>,
    source: &str,
) -> Vec<RuntimeLocalMcpServerSummary> {
    let mut out = Vec::with_capacity(servers.len());
    for (name, value) in servers {
        let Value::Object(entry) = value else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        let mut enabled = true;
        if let Some(Value::Bool(value)) = entry.get("enabled") {
            enabled = *value;
        }
        if let Some(Value::Bool(true)) = entry.get("disabled") {
            enabled = false;
        }
        out.push(RuntimeLocalMcpServerSummary {
            name: name.clone(),
            transport: runtime_mcp_transport(entry),
            source: source.to_string(),
            enabled,
        });
    }
    out
}

/// `"a.b"` 形式的嵌套查找（上游 `nestedRuntimeMcpMap`）。
///
/// 任一层不是对象就返回 `None`（与上游同：`map[string]any` 断言失败即放弃整条路径）。
#[must_use]
pub fn nested_runtime_mcp_map<'a>(
    config: &'a Map<String, Value>,
    path: &str,
) -> Option<&'a Map<String, Value>> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = config;
    for (index, part) in parts.iter().enumerate() {
        let value = current.get(*part)?;
        let Value::Object(mapped) = value else {
            return None;
        };
        if index == parts.len() - 1 {
            return Some(mapped);
        }
        current = mapped;
    }
    None
}

/// 传输档归类（上游 `runtimeMcpTransport`）。
#[must_use]
pub fn runtime_mcp_transport(entry: &Map<String, Value>) -> String {
    if let Some(Value::String(kind)) = entry.get("type") {
        match kind.to_ascii_lowercase().as_str() {
            "local" | "stdio" => return "stdio".to_string(),
            "remote" | "http" | "streamable-http" => return "http".to_string(),
            "sse" => return "sse".to_string(),
            _ => {}
        }
    }
    if entry.contains_key("command") {
        return "stdio".to_string();
    }
    if entry.contains_key("url") {
        return "http".to_string();
    }
    "unknown".to_string()
}

/// runtime 层 + agent 层的本地合并（上游 `mergeRuntimeAndAgentMcpConfig`）。
///
/// `agent_config` 为 `None` / `null` ⇒ **原样返回**（provider 的原生继承路径不动）。
/// 只要有配置（哪怕 `mcpServers` 是空表），就切到合并结果 —— 于是「加一个托管 server」
/// 不会顺手把与它无关的 runtime server 关掉。
pub fn merge_runtime_and_agent_mcp_config(
    provider: &str,
    agent_config: Option<&Value>,
) -> Result<Option<Value>> {
    let Some(agent_config) = agent_config else {
        return Ok(None);
    };
    let trimmed = match agent_config {
        Value::Null => return Ok(Some(Value::Null)),
        Value::Object(map) if map.is_empty() => return Ok(Some(agent_config.clone())),
        other => other.clone(),
    };

    let ProviderMcpConfig::Supported(runtime_servers) = load_runtime_mcp_server_configs(provider)?
    else {
        return Ok(Some(agent_config.clone()));
    };

    let Value::Object(agent_document) = trimmed else {
        return Err(McpConfigError::Shape(
            "parse agent MCP config: not a JSON object".to_string(),
        ));
    };
    let agent_servers = if let Some(servers) = nested_runtime_mcp_map(&agent_document, "mcpServers")
    {
        servers.clone()
    } else if provider == "opencode" || provider == "codearts" {
        // 旧 OpenCode agent 可能存的是 provider 原生的顶层 `mcp`；放进规范信封后这些条目
        // 仍然能走既有的 OpenCode 适配器。
        nested_runtime_mcp_map(&agent_document, "mcp")
            .cloned()
            .unwrap_or_default()
    } else {
        Map::new()
    };

    let mut merged: BTreeMap<String, Value> = BTreeMap::new();
    for (name, entry) in runtime_servers {
        merged.insert(name, entry);
    }
    for (name, entry) in agent_servers {
        merged.insert(name, entry);
    }
    let servers: Map<String, Value> = merged.into_iter().collect();
    Ok(Some(crate::mcp::mcp_servers_document(servers)))
}

#[cfg(test)]
mod tests;
