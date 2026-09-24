//! 桥面 handler（`POST /api/plugin-bridge/v1/hooks/:key` 的实现体）。
//!
//! 从 `hooks_job.rs` 拆出来是门 ⑩ 的 800 行硬上限。

use super::{
    decode, find_hook, invoke_hook, plugin_issue_for_caller, policy, AppState, Arc, Bytes,
    Deserialize, HeaderMap, HookActor, HookInvocation, HookRuntime, HookTrigger, Id, IntoResponse,
    Path, PluginError, PluginResult, Response, State, StatusCode, Value,
};

// ---------------------------------------------------------------------------
// bridge 路由的 handler（注册在 `routes/plugin_bridge/hooks.rs`）
// ---------------------------------------------------------------------------

/// 上游 `InvokePluginHook` 的请求体（`trigger` 只允许 `ui` / `manual`）。
#[derive(Debug, Deserialize)]
struct BridgeHookBody {
    trigger: String,
    #[serde(default)]
    issue_id: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

/// `POST /api/plugin-bridge/v1/hooks/:key` —— 人在界面里按按钮 / 命令面板触发的 hook。
///
/// 两件事是**故意**的：
///
/// 1. 触发器只收 `ui` / `manual`：`event` 由宿主派发、`agent` 走 MCP，从浏览器收下它们等于
///    让一个客户端挑一个本该带别种身份的调用点；
/// 2. 认证走 `policy::resolve_caller`（会话中继面），安装来自 `x-multica-plugin-installation`
///    头 ⇒ 插件令牌**不认**（那是公开 `/v1` 面的凭据）。
///
/// 本函数注册在 `routes/plugin_bridge/hooks.rs`（那里是前缀归位点），实现留在本文件是因为
/// 引擎、错误映射与 `PluginError` 全在 `routes::plugins` 下（见文件头）。
pub(crate) async fn invoke_bridge_hook(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let runtime = HookRuntime::from_state(&state);
    let inner = async {
        let caller = policy::resolve_caller(&state, &headers, "")
            .await
            .map_err(bridge_error)?;
        // 「插件自己的服务器调这个端点」= 求宿主回调自己 = 一个没有人的循环（上游注释）。
        let actor_id = caller.require_member().map_err(bridge_error)?;
        let request: BridgeHookBody = decode(&body)?;
        let trigger = parse_bridge_trigger(&request.trigger)?;
        let hook = find_hook(&caller.installation, &key)?;

        // `issue_id` 给了就要过第三步授权：范围外的 issue 是 404（插件不能借这个端点确认
        // 一个它读不到的 id 存在）。
        let issue_id = match request.issue_id.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(reference) => {
                let issue = plugin_issue_for_caller(&state, &caller, reference)
                    .await
                    .map_err(bridge_error)?;
                Some(Id::from(issue.id))
            }
        };

        let result = invoke_hook(
            &runtime,
            HookInvocation {
                installation: caller.installation.clone(),
                hook,
                trigger,
                event_type: None,
                actor: HookActor {
                    kind: mc_plugin_host::token::ActorKind::Member,
                    id: actor_id,
                },
                issue_id,
                input: request.input,
                delivery_id: None,
                planned_at: None,
                attempt: 1,
            },
        )
        .await?;
        Ok::<Value, PluginError>(result.to_json())
    };

    match inner.await {
        Ok(payload) => (StatusCode::OK, axum::Json(payload)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `ui` / `manual` 之外的触发器一律 400（上游 `trigger must be ui or manual`）。
pub(super) fn parse_bridge_trigger(raw: &str) -> PluginResult<HookTrigger> {
    match HookTrigger::parse(raw) {
        Some(HookTrigger::Ui) => Ok(HookTrigger::Ui),
        Some(HookTrigger::Manual) => Ok(HookTrigger::Manual),
        _ => Err(PluginError::invalid("trigger must be ui or manual")),
    }
}

/// `ActionError`（凭据/成员门）折成插件面的错误信封。
///
/// 桥面其余路由走 `ActionError` 自己的 problem 体；hook 是插件面唯一条没有 `/v1` 孪生的键，
/// 上游用的是 `writePluginError` ⇒ 这里也用它，好让码集与 M6-5/M6-6 的插件面一致。
fn bridge_error(error: policy::ActionError) -> PluginError {
    PluginError::new(error.status, error.code, error.detail)
}
