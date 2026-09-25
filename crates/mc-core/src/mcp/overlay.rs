//! MCP `mcp_config` 文档的**纯函数**层 —— M8-3（`LUM-1800`）落地
//! （`docs/61-M8-PLAN.md` §2.3 / §6.5 的 M8-3 行；上游 `mcp_overlay.go` 160 行 +
//! `workspace_mcp.go` 的 `ResolveAgentMcpConfig` 折叠段）。
//!
//! 两个函数，同一条**优先级链**（上游 `daemon.go:2466-2505` 的注释逐字：
//! `bound workspace servers  <  agent's own servers  <  per-task overlay`）：
//!
//! | 函数 | 上游 | 做什么 |
//! | --- | --- | --- |
//! | [`resolve_agent_mcp_config`] | `ResolveAgentMcpConfig`（`workspace_mcp.go:57`） | workspace 库里**绑定给该 agent 且启用**的条目 ← agent 自己的 `mcp_config`（agent 侧同名**胜出**） |
//! | [`merge_task_overlay`] | `mergeMCPOverlay`（`mcp_overlay.go:36`） | 上面那一层的结果 ← per-task overlay（overlay 同名**胜出**） |
//!
//! # 合并契约（两侧逐字照抄上游）
//!
//! 输入都是 Claude 风格 `{"mcpServers": {<name>: <object>}}`。**任何不在 `mcpServers`
//! 下的顶层键只从 agent 侧保留** —— overlay 今天只携带 server 条目，不得悄悄引入别的顶层键。
//!
//! - [`merge_task_overlay`] 按 server 名合并，**overlay 胜出**（它携带的是用户自己的实时
//!   session URL，例如 Composio 的 bearer；agent 侧同名条目多半是过期/管理员共享的占位）。
//! - [`resolve_agent_mcp_config`] 反过来：**agent 自己的条目胜出**（它是更具体的陈述，
//!   也是 agent owner 直接编辑的那一份），并且把 agent 的**遗留容器 `mcp`** 一并折进规范
//!   容器 `mcpServers` —— 这一步是**必须**的，不是美化：daemon 侧 `runtime_mcp.go` 的
//!   runtime×agent 合并只在 `mcpServers` **缺席**时才回落读 `mcp`，所以把绑定条目写进
//!   `mcpServers` 而把 agent 的遗留条目留在 `mcp`，会让 daemon 读到绑定集并**静默丢掉**
//!   agent 自己保存的 servers（上游 `workspace_mcp.go:47-53` 点名的 `OpenCode` 回归）。
//! - 两侧都为空 / `null` ⇒ 返回 `None`（让 daemon 的 `hasManagedCursorMcpConfig` 短路继续
//!   把 task 当作「完全无托管 MCP」）。
//!
//! # 失败模式（fail-soft：**绝不**因为 overlay 坏了就丢掉 agent 已保存的 servers）
//!
//! 非法输入 ⇒ `Err`，**调用侧必须回落到它自己手上那份 agent 配置**（本函数按引用收它，
//! 调用方仍然持有；这就是上游「原值 + error」双返回在 Rust 侧的等价形态）。
//!
//! | 上游的四种错误 | 本枚举的哪一支 |
//! | --- | --- |
//! | agent 文档不是 JSON 对象 / 解析失败 | [`McpOverlayError::AgentConfigNotObject`] |
//! | overlay 不是 JSON 对象 / 解析失败 | [`McpOverlayError::OverlayNotObject`] |
//! | 任一容器不是对象、条目名是空串、条目不是对象 | [`McpOverlayError::ServersNotObject`] |
//!
//! ⇒ 这是**登记过的收窄**：上游的 `unmarshalServerMap` 会把出错的 server 名写进错误文案
//! （`mcpServers.<name> must be a JSON object`），本片的三值枚举不带载荷（anchor 冻结的
//! 签名如此，`docs/32` §11.1）；四种情形的**调用侧动作完全相同**（保留 agent 配置 + 记一行
//! 日志），所以丢失的只是文案细节。见 `docs/32` §16 的偏离登记。
//!
//! # 与「列缺失」的对应
//!
//! 上游的 `hasManagedJSON` 同时判「非空字节」与「不等于字面量 `null`」。本仓的入参是
//! `&serde_json::Value`（不是原始字节）⇒ **列缺失与 JSON `null` 统一折叠为
//! [`Value::Null`]**；「空字节」那一支在 JSONB 列上不可达（`jsonb` 不接受空串）。
//!
//! # 与 daemon 侧既有语义的关系
//!
//! `mc-daemon/src/mcp/runtime.rs` 已实现「runtime 层做底、agent 层同名覆盖」的**本地**合并；
//! 本文件是它的**上游一层**（agent 已解析后的 `mcp_config` ← per-task overlay），
//! 且**不碰** daemon 文件（`docs/61` §2.3 的「不得重复实现」清单）。

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

/// agent 文档里可能承载 server 的**两个**顶层容器（上游 `mcpServerContainers` 逐字）。
///
/// `mcpServers` 是每个 runtime 都消费的 Claude 风格容器；`mcp` 是少数 agent 仍然带着、
/// 且 agent 设置界面会**原地编辑**的遗留拼法（上游 `mcp-config-model.ts`）。
/// 两者都读；只有 `mcpServers` 会被写出。
pub const MCP_SERVER_CONTAINERS: [&str; 2] = ["mcpServers", "mcp"];

/// 规范容器名（写出口只有它一个）。
const CANONICAL_CONTAINER: &str = "mcpServers";

/// 折叠 agent 自己条目时的**遍历序**（上游 `ResolveAgentMcpConfig` 里那句
/// `[...]string{"mcp", "mcpServers"}` 逐字）：**后遍历者覆盖先遍历者** ⇒ 同名时
/// 规范容器胜出。它是**优先级**，不是「忽略顺序也无所谓」的容器清单 ——
/// 顺序反了会让遗留容器悄悄盖掉 agent 设置界面显示的那一份
/// （`resolve_prefers_the_canonical_container_over_the_legacy_one` 钉住这一点）。
const MCP_SERVER_FOLD_ORDER: [&str; 2] = ["mcp", CANONICAL_CONTAINER];

/// overlay 合并错误。**不**携带输入片段：条目里常规地嵌着 API token，
/// 错误文案被写进日志/响应时不得回显它们（上游在 `validateWorkspaceMcpServerEntry`
/// 的注释里为同一理由也刻意不包裹底层错误）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpOverlayError {
    #[error("agent mcp_config is not a JSON object")]
    AgentConfigNotObject,
    #[error("overlay is not a JSON object")]
    OverlayNotObject,
    #[error("mcpServers is not a JSON object")]
    ServersNotObject,
}

/// workspace 库里的一个条目 + 它挂载用的名字（上游 `WorkspaceMcpBinding`）。
///
/// `Debug` **手写**（不派生）：`config` 常规地带着第三方 token。
#[derive(Clone, PartialEq)]
pub struct WorkspaceMcpBinding {
    /// 条目在 `mcpServers` 下挂载的名字。
    pub name: String,
    /// 条目本体（含第三方凭证 ⇒ **手写 `Debug` 只列键名**）。
    pub config: Value,
}

impl std::fmt::Debug for WorkspaceMcpBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceMcpBinding")
            .field("name", &self.name)
            .field("config", &"<redacted, write-only>")
            .finish()
    }
}

/// 把 per-task overlay 叠加到 agent 的 `mcp_config` 上。
///
/// 语义逐条见模块头；`None` 表示**两侧都没有托管内容**（调用侧应把 task 当作「无托管
/// MCP」）。失败时调用侧**必须**回落到它传进来的 `agent_mcp_config`。
///
/// # Errors
///
/// 见模块头的四格对照表。
pub fn merge_task_overlay(
    agent_mcp_config: &Value,
    overlay: &Value,
) -> Result<Option<Value>, McpOverlayError> {
    if !has_managed_json(overlay) {
        return Ok(passthrough(agent_mcp_config));
    }
    if !has_managed_json(agent_mcp_config) {
        // 只回 overlay，并把它的顶层键**原样**带上（上游注释：overlay 可能是 Postgres
        // JSONB 任意空白写下的，重新 marshal 让 daemon 拿到确定的规范形状）。
        let doc = overlay
            .as_object()
            .ok_or(McpOverlayError::OverlayNotObject)?;
        return Ok(Some(Value::Object(doc.clone())));
    }

    // 顺序照上游：**先** agent 后 overlay（两侧都坏时上游报的是 agent 那一支）。
    let agent_doc = agent_mcp_config
        .as_object()
        .ok_or(McpOverlayError::AgentConfigNotObject)?;
    let overlay_doc = overlay
        .as_object()
        .ok_or(McpOverlayError::OverlayNotObject)?;

    let mut merged = server_map(agent_doc.get(CANONICAL_CONTAINER))?;
    // 同名时 overlay 胜出。
    for (name, entry) in server_map(overlay_doc.get(CANONICAL_CONTAINER))? {
        merged.insert(name, entry);
    }

    // 重建：非 `mcpServers` 的顶层键**只**从 agent 侧保留，合并结果写回规范容器。
    let mut out = Map::new();
    for (key, value) in agent_doc {
        if key != CANONICAL_CONTAINER {
            out.insert(key.clone(), value.clone());
        }
    }
    if !merged.is_empty() {
        out.insert(CANONICAL_CONTAINER.to_string(), servers_value(merged));
    }
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(out)))
}

/// 把 workspace 库中**绑定给该 agent**的条目折进 agent 自己的 `mcp_config`。
///
/// 这是优先级链里 `bound < agent < overlay` 的前两段。`bound` 由调用侧从
/// `agent_mcp_server JOIN workspace_mcp_server`（**只取 `enabled = TRUE`**）读出：
/// 一个 workspace 库条目**不绑定就不生效** —— 建它不给任何人，这正是这套形状的全部意义。
///
/// `agent_mcp_config` 传 [`Value::Null`] 表示该 agent 自己没有配置（列缺失）。
/// 返回 `None` = 什么都没有（保持 daemon 的「无托管 MCP」短路）。
///
/// # Errors
///
/// agent 文档不是对象、或某个容器/条目形状非法 ⇒ [`McpOverlayError`]
/// （调用侧回落到 agent 原值 —— 绑定一个共享 server **绝不**能拿走 agent 今天在跑的 servers）。
pub fn resolve_agent_mcp_config(
    bound: &[WorkspaceMcpBinding],
    agent_mcp_config: &Value,
) -> Result<Option<Value>, McpOverlayError> {
    if bound.is_empty() {
        return Ok(passthrough(agent_mcp_config));
    }

    // 名字为空或条目缺失的绑定被跳过（上游同判）；同名**后者胜出**（Go map 赋值序）。
    let mut shared: BTreeMap<String, Value> = BTreeMap::new();
    for server in bound {
        if server.name.is_empty() || !has_managed_json(&server.config) {
            continue;
        }
        shared.insert(server.name.clone(), server.config.clone());
    }
    if shared.is_empty() {
        return Ok(passthrough(agent_mcp_config));
    }

    if !has_managed_json(agent_mcp_config) {
        // agent 自己没有声明任何东西：它就跑「被给到的那几个」。
        let mut out = Map::new();
        out.insert(CANONICAL_CONTAINER.to_string(), servers_value(shared));
        return Ok(Some(Value::Object(out)));
    }

    let agent_doc = agent_mcp_config
        .as_object()
        .ok_or(McpOverlayError::AgentConfigNotObject)?;

    // agent 的**两个**容器一起读：名字集合要跨遗留拼法，否则 `mcp` 里的 `linear`
    // 挡不住绑定进来的同名条目。折叠顺序 `mcp` → `mcpServers` ⇒ 同名时规范容器胜出
    // （与 agent 设置界面显示的一致）。
    let mut declared: HashSet<String> = HashSet::new();
    let mut own: BTreeMap<String, Value> = BTreeMap::new();
    for container in MCP_SERVER_FOLD_ORDER {
        for (name, entry) in server_map(agent_doc.get(container))? {
            declared.insert(name.clone());
            own.insert(name, entry);
        }
    }

    let mut merged: BTreeMap<String, Value> = BTreeMap::new();
    for (name, config) in &shared {
        if !declared.contains(name) {
            merged.insert(name.clone(), config.clone());
        }
    }
    for (name, entry) in own {
        merged.insert(name, entry);
    }

    // 两个容器都被上面消费掉了：把遗留键留在原地会交给 daemon 第二份**过期副本**。
    let mut out = Map::new();
    for (key, value) in agent_doc {
        if !MCP_SERVER_CONTAINERS.contains(&key.as_str()) {
            out.insert(key.clone(), value.clone());
        }
    }
    out.insert(CANONICAL_CONTAINER.to_string(), servers_value(merged));
    Ok(Some(Value::Object(out)))
}

/// 上游 `hasManagedJSON`：真的带了托管载荷（非 `null`）。见模块头的折叠说明。
fn has_managed_json(value: &Value) -> bool {
    !value.is_null()
}

/// 上游 `passthroughAgentMcpConfig`：agent 配置原样返回，缺失时 `None`。
fn passthrough(agent_mcp_config: &Value) -> Option<Value> {
    if has_managed_json(agent_mcp_config) {
        Some(agent_mcp_config.clone())
    } else {
        None
    }
}

/// 上游 `unmarshalServerMap`：把 `mcpServers` 子对象解成**按名字**索引的表。
///
/// 缺失 / `null` ⇒ 空表（**不是**错误）；非对象、空名、条目不是对象 ⇒
/// [`McpOverlayError::ServersNotObject`]（每个 runtime 都要求内层是对象，否则 sidecar
/// 生成器会 500）。
fn server_map(container: Option<&Value>) -> Result<BTreeMap<String, Value>, McpOverlayError> {
    let Some(container) = container else {
        return Ok(BTreeMap::new());
    };
    if !has_managed_json(container) {
        return Ok(BTreeMap::new());
    }
    let object = container
        .as_object()
        .ok_or(McpOverlayError::ServersNotObject)?;
    let mut out = BTreeMap::new();
    for (name, entry) in object {
        if name.is_empty() || !entry.is_object() {
            return Err(McpOverlayError::ServersNotObject);
        }
        out.insert(name.clone(), entry.clone());
    }
    Ok(out)
}

/// `BTreeMap<String, Value>` → JSON 对象（键序确定：`serde_json::Map` 默认是 `BTreeMap`，
/// 与 Go `json.Marshal` 对 map 的字典序一致 ⇒ 两侧产出**逐字节同形**）。
fn servers_value(servers: BTreeMap<String, Value>) -> Value {
    Value::Object(servers.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn servers_of(doc: &Value) -> &Map<String, Value> {
        doc.get(CANONICAL_CONTAINER)
            .and_then(Value::as_object)
            .expect("mcpServers 对象")
    }

    fn binding(name: &str, config: Value) -> WorkspaceMcpBinding {
        WorkspaceMcpBinding {
            name: name.into(),
            config,
        }
    }

    // -----------------------------------------------------------------------
    // merge_task_overlay（上游 mcp_overlay_test.go 的逐条移植）
    // -----------------------------------------------------------------------

    /// 「哪边都没有托管 MCP」⇒ `None`（daemon 的短路分支）。
    ///
    /// 上游这里有四个等价输入（`nil` / `null` / 空字节两两组合）；本仓的入参是已解析的
    /// `Value` ⇒ 只剩 [`Value::Null`] 这一种可表达形态（模块头「与列缺失的对应」）。
    #[test]
    fn merge_returns_none_when_neither_side_has_content() {
        assert_eq!(merge_task_overlay(&Value::Null, &Value::Null), Ok(None));
    }

    /// 「该 task 没有 overlay」⇒ agent 配置**逐字**透传（值相等：入参已是解析后的 JSON）。
    #[test]
    fn merge_passes_the_agent_config_through_untouched() {
        let agent = json!({"mcpServers":{"fetch":{"command":"uvx","args":["mcp-server-fetch"]}}});
        assert_eq!(
            merge_task_overlay(&agent, &Value::Null),
            Ok(Some(agent.clone()))
        );
    }

    /// 「agent 自己没有配置」⇒ 只回 overlay（重新规范化成对象）。
    #[test]
    fn merge_returns_the_overlay_alone_when_the_agent_has_none() {
        let overlay =
            json!({"mcpServers":{"composio":{"type":"http","url":"https://mcp.example/s/abc"}}});
        let got = merge_task_overlay(&Value::Null, &overlay)
            .expect("merge")
            .expect("some");
        assert_eq!(servers_of(&got).len(), 1);
        assert!(servers_of(&got).contains_key("composio"));
    }

    /// 两侧都有 ⇒ agent 的 servers 必须**全部存活**，overlay 的条目**并列出现**。
    #[test]
    fn merge_keeps_both_sides() {
        let agent = json!({"mcpServers":{"fetch":{"command":"uvx"},"github":{"command":"npx"}}});
        let overlay =
            json!({"mcpServers":{"composio":{"type":"http","url":"https://mcp.example"}}});
        let got = merge_task_overlay(&agent, &overlay)
            .expect("merge")
            .expect("some");
        for want in ["fetch", "github", "composio"] {
            assert!(servers_of(&got).contains_key(want), "缺 {want}");
        }
    }

    /// 同名冲突：**overlay 胜出**（它带的是实时 session URL）。
    #[test]
    fn merge_lets_the_overlay_win_on_collisions() {
        let agent = json!({"mcpServers":{"composio":{"url":"https://placeholder.example/old"}}});
        let overlay = json!({"mcpServers":{"composio":{"url":"https://mcp.example/new"}}});
        let got = merge_task_overlay(&agent, &overlay)
            .expect("merge")
            .expect("some");
        assert_eq!(
            servers_of(&got)["composio"]["url"],
            json!("https://mcp.example/new")
        );
    }

    /// `mcpServers` 之外的顶层键**只从 agent 侧**保留。
    #[test]
    fn merge_preserves_the_agent_non_server_keys() {
        let agent = json!({"mcpServers":{"fetch":{"command":"uvx"}},"experimental":{"foo":"bar"}});
        let overlay = json!({"mcpServers":{"composio":{"type":"http"}}});
        let got = merge_task_overlay(&agent, &overlay)
            .expect("merge")
            .expect("some");
        assert_eq!(got.get("experimental"), Some(&json!({"foo":"bar"})));
    }

    /// 坏 overlay ⇒ `Err`（调用侧回落到 agent 原值，**不**静默丢掉 agent 的 servers）。
    #[test]
    fn merge_rejects_a_malformed_overlay() {
        let agent = json!({"mcpServers":{"fetch":{"command":"uvx"}}});
        assert_eq!(
            merge_task_overlay(&agent, &json!("not-an-object")),
            Err(McpOverlayError::OverlayNotObject)
        );
    }

    /// 坏 agent ⇒ `Err`（对称守卫：不能 panic，也不能给出半合并结果）。
    #[test]
    fn merge_rejects_a_malformed_agent_config() {
        let overlay = json!({"mcpServers":{"composio":{"type":"http"}}});
        assert_eq!(
            merge_task_overlay(&json!(["not", "an", "object"]), &overlay),
            Err(McpOverlayError::AgentConfigNotObject)
        );
    }

    /// 条目必须是对象：`{"composio":"https://…"}`（字符串）不得穿过合并再去炸 sidecar 生成器。
    #[test]
    fn merge_rejects_a_non_object_server_entry() {
        let agent = json!({"mcpServers":{"fetch":{"command":"uvx"}}});
        let overlay = json!({"mcpServers":{"composio":"not-an-object"}});
        assert_eq!(
            merge_task_overlay(&agent, &overlay),
            Err(McpOverlayError::ServersNotObject)
        );
    }

    /// `mcpServers` 本身不是对象 ⇒ 同判（含空名条目）。
    #[test]
    fn merge_rejects_a_non_object_container_and_an_empty_name() {
        let agent = json!({"mcpServers":[]});
        assert_eq!(
            merge_task_overlay(&agent, &json!({"mcpServers":{"a":{}}})),
            Err(McpOverlayError::ServersNotObject)
        );
        let agent = json!({"mcpServers":{"":{"command":"x"}}});
        assert_eq!(
            merge_task_overlay(&agent, &json!({"mcpServers":{"a":{}}})),
            Err(McpOverlayError::ServersNotObject)
        );
    }

    /// 两侧都坏 ⇒ 报的是 **agent** 那一支（上游先解析 agent）。
    #[test]
    fn merge_reports_the_agent_side_first_when_both_are_malformed() {
        assert_eq!(
            merge_task_overlay(&json!("bad"), &json!("also-bad")),
            Err(McpOverlayError::AgentConfigNotObject)
        );
    }

    /// 合并结果里 `mcpServers` 之外没有别的键、且只有 agent 那份 `mcpServers` 被消费。
    #[test]
    fn merge_drops_an_empty_container() {
        let got = merge_task_overlay(&json!({"experimental":1}), &json!({"mcpServers":{}}))
            .expect("merge")
            .expect("some");
        assert!(got.get(CANONICAL_CONTAINER).is_none());
        assert_eq!(got, json!({"experimental":1}));
    }

    // -----------------------------------------------------------------------
    // resolve_agent_mcp_config（上游 workspace_mcp_test.go 的逐条移植）
    // -----------------------------------------------------------------------

    /// 绑定模型的定义性质：agent 就跑「被给到的那几个」，不多不少。
    #[test]
    fn resolve_gives_an_unconfigured_agent_exactly_its_bindings() {
        let bound = [binding("shared", json!({"url":"https://shared.example"}))];
        let got = resolve_agent_mcp_config(&bound, &Value::Null)
            .expect("resolve")
            .expect("some");
        assert_eq!(
            got,
            json!({"mcpServers":{"shared":{"url":"https://shared.example"}}})
        );
    }

    /// 绑定与 agent 自己的条目**并集**。
    #[test]
    fn resolve_unions_bindings_with_the_agents_own_servers() {
        let bound = [binding("shared", json!({"url":"https://shared.example"}))];
        let agent = json!({"mcpServers":{"private":{"url":"https://private.example"}}});
        let got = resolve_agent_mcp_config(&bound, &agent)
            .expect("resolve")
            .expect("some");
        assert_eq!(servers_of(&got).len(), 2);
    }

    /// 同名时 **agent 自己的条目胜出**。
    #[test]
    fn resolve_lets_the_agents_own_entry_win_on_collisions() {
        let bound = [binding(
            "linear",
            json!({"url":"https://ws-linear.example"}),
        )];
        let agent = json!({"mcpServers":{"linear":{"url":"https://agent-linear.example"}}});
        let got = resolve_agent_mcp_config(&bound, &agent)
            .expect("resolve")
            .expect("some");
        assert_eq!(
            servers_of(&got)["linear"]["url"],
            json!("https://agent-linear.example")
        );
    }

    /// 遗留容器 `mcp` 里的同名条目**既挡住**绑定条目、本身也**存活**（折进规范容器）。
    #[test]
    fn resolve_folds_the_legacy_container_into_the_canonical_one() {
        let bound = [
            binding("linear", json!({"url":"https://ws-linear.example"})),
            binding("shared", json!({"url":"https://shared.example"})),
        ];
        let agent = json!({"mcp":{"linear":{"url":"https://legacy-linear.example"}}});
        let got = resolve_agent_mcp_config(&bound, &agent)
            .expect("resolve")
            .expect("some");
        // 遗留键被消费掉，不是留在 `mcpServers` 旁边。
        assert!(got.get("mcp").is_none());
        assert_eq!(
            servers_of(&got)["linear"]["url"],
            json!("https://legacy-linear.example")
        );
        assert!(servers_of(&got).contains_key("shared"));
    }

    /// 非 server 的顶层键（`inputs` 之类）必须留下。
    #[test]
    fn resolve_keeps_the_agents_non_server_keys() {
        let bound = [binding("shared", json!({"url":"https://shared.example"}))];
        let agent = json!({"mcp":{"legacy":{"command":"legacy-server"}},"inputs":[{"id":"token"}]});
        let got = resolve_agent_mcp_config(&bound, &agent)
            .expect("resolve")
            .expect("some");
        assert_eq!(got.get("inputs"), Some(&json!([{"id":"token"}])));
        assert!(got.get("mcp").is_none());
        assert_eq!(servers_of(&got).len(), 2);
    }

    /// 两个容器都声明同名 ⇒ 规范容器胜出（agent 设置界面显示的就是它）。
    #[test]
    fn resolve_prefers_the_canonical_container_over_the_legacy_one() {
        let bound = [binding("shared", json!({"url":"https://shared.example"}))];
        let agent = json!({
            "mcpServers":{"dup":{"url":"https://canonical.example"}},
            "mcp":{"dup":{"url":"https://legacy.example"}},
        });
        let got = resolve_agent_mcp_config(&bound, &agent)
            .expect("resolve")
            .expect("some");
        assert_eq!(
            servers_of(&got)["dup"]["url"],
            json!("https://canonical.example")
        );
    }

    /// 没有绑定 ⇒ agent 原样（库条目**不**隐式生效）。
    #[test]
    fn resolve_leaves_the_agent_alone_without_bindings() {
        let agent = json!({"mcpServers":{"private":{"url":"https://private.example"}}});
        assert_eq!(
            resolve_agent_mcp_config(&[], &agent),
            Ok(Some(agent.clone()))
        );
        assert_eq!(resolve_agent_mcp_config(&[], &Value::Null), Ok(None));
    }

    /// 托管但**空**的 agent 文档仍然收得到绑定（「没有自己的 server」不是退出信号）。
    #[test]
    fn resolve_still_binds_into_a_managed_but_empty_agent_config() {
        let bound = [binding("shared", json!({"url":"https://shared.example"}))];
        let got = resolve_agent_mcp_config(&bound, &json!({}))
            .expect("resolve")
            .expect("some");
        assert_eq!(servers_of(&got).len(), 1);
    }

    /// 名字为空 / 条目缺失的绑定被跳过；全被跳过 ⇒ 等同没有绑定。
    #[test]
    fn resolve_skips_unusable_bindings() {
        let bound = [
            binding("", json!({"url":"https://x.example"})),
            binding("null-config", Value::Null),
        ];
        assert_eq!(resolve_agent_mcp_config(&bound, &Value::Null), Ok(None));
    }

    /// fail-soft：agent 文档坏 ⇒ `Err`，且**不得**吞掉 agent 的 servers（调用侧拿到错误后
    /// 继续用自己手上那份）。
    #[test]
    fn resolve_rejects_a_malformed_agent_document() {
        let bound = [binding("shared", json!({"url":"https://shared.example"}))];
        assert_eq!(
            resolve_agent_mcp_config(&bound, &json!(["not-an-object"])),
            Err(McpOverlayError::AgentConfigNotObject)
        );
        assert_eq!(
            resolve_agent_mcp_config(&bound, &json!({"mcpServers":[]})),
            Err(McpOverlayError::ServersNotObject)
        );
        assert_eq!(
            resolve_agent_mcp_config(&bound, &json!({"mcpServers":{"broken":"not-an-object"}})),
            Err(McpOverlayError::ServersNotObject)
        );
    }

    /// 折叠序与容器清单**同集合**（防两处漂移：漏一个容器 = agent 的某份 servers 被静默丢掉）。
    #[test]
    fn fold_order_covers_exactly_the_known_containers() {
        let mut declared = MCP_SERVER_CONTAINERS.map(str::to_string).to_vec();
        let mut folded = MCP_SERVER_FOLD_ORDER.map(str::to_string).to_vec();
        declared.sort();
        folded.sort();
        assert_eq!(declared, folded);
        assert_eq!(MCP_SERVER_FOLD_ORDER[1], CANONICAL_CONTAINER);
    }

    /// 两层**组合**：bound < agent < overlay（上游 `TestResolveAgentMcpConfig_ComposesWithTaskOverlay`）。
    #[test]
    fn resolve_composes_with_the_task_overlay() {
        let bound = [
            binding("shared", json!({"url":"https://shared.example"})),
            binding("composio", json!({"url":"https://ws-composio.example"})),
        ];
        let agent = json!({"mcpServers":{"private":{"url":"https://private.example"}}});
        let overlay = json!({"mcpServers":{"composio":{"url":"https://session-composio.example"}}});

        let resolved = resolve_agent_mcp_config(&bound, &agent)
            .expect("resolve")
            .expect("some");
        let merged = merge_task_overlay(&resolved, &overlay)
            .expect("merge")
            .expect("some");
        assert_eq!(servers_of(&merged).len(), 3);
        assert_eq!(
            servers_of(&merged)["composio"]["url"],
            json!("https://session-composio.example")
        );
    }

    /// **凭据纪律**：`WorkspaceMcpBinding` 的手写 `Debug` 不吐条目内容。
    #[test]
    fn binding_debug_redacts_the_entry() {
        let rendered = format!(
            "{:?}",
            binding(
                "linear",
                json!({"url":"https://secret.example","headers":{"Authorization":"Bearer sk-live-do-not-log"}})
            )
        );
        assert!(!rendered.contains("sk-live-do-not-log"), "{rendered}");
        assert!(!rendered.contains("secret.example"), "{rendered}");
        assert!(rendered.contains("<redacted, write-only>"));
        assert!(rendered.contains("linear"));
    }
}
