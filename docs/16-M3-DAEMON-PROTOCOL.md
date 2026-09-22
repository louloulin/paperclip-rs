# M3 daemon 协议冻结（`mc-daemon-proto`）

> **冻结声明**：本文件冻结 `crates/mc-daemon-proto` 的类型、帧契约、事件常量表、能力协商与 RPC method 表，
> 作为 M3 子波二/三（W3b/W3c）**全部 36 条 daemon 路由 + 8 条异步请求-应答路由**的请求/响应形状来源。
> **冻结 commit**：`0ed37b5`（`feat/multica-rs-m3a-daemon-proto` 的第一笔 commit，含全部类型/用例与本文首版；
> 该 sha 由本片第二笔 commit 回填，见 `git log 0ed37b5..HEAD`）。
> **上游权威**：`server/pkg/protocol/` @ `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`（与 `docs/fixtures/upstream-routes.tsv` 同源），
> 辅以 `server/internal/daemonws/hub.go`、`server/internal/daemon/client.go`、`server/internal/handler/{daemon,daemon_rpc,runtime_update,runtime_models,runtime_local_skills}.go`。
> **编制**：LUM-1407（M3-1）。基线 `feat/multica-rs-initial` @ `d7639f0`。
> **改动流程**：本文件声明冻结后，改协议**必须**走 `docs/15-M3-PLAN.md` §8.4 仲裁（「谁冻结谁签字」，§9）。
> **本片不做**：0 条路由注册、0 次落库、0 个新第三方依赖（§1）。

---

## 1. 冻结范围与不做清单

| 项 | 处置 |
| --- | --- |
| `pkg/protocol/messages.go` 的 29 个 payload 类型 | **冻结**（Rust 版在 `src/messages/`） |
| `pkg/protocol/events.go` 的 109 个事件常量 | **冻结**（`src/events.rs` + §4 表） |
| `Message` 帧信封 + RPC request/response 信封 | **冻结**（`src/messages/envelope.rs`） |
| `hub.go` 的帧参数（读上限 / 并发 RPC 上限 / ping-pong） | **冻结**（`src/rpc.rs`） |
| RPC method 名表（`hub.go` + `daemon_rpc.go` 的字符串） | **冻结**（`method::KNOWN`，1 条） |
| 能力串（`DaemonCapability*` / `AppCapability*`）与协商头 | **冻结**（`src/capabilities.rs`） |
| §1.5 的 36 条 daemon 路由 | **只冻结 body/帧形状**（契约表 §6.1），**不注册路由** |
| §1.2 的 8 条 `Initiate*`/`Get*Request` | **只冻结返回体**（契约表 §6.2），**不实现 handler** |
| ws 传输 / 连接管理 / hub / notifier / metrics | **不做**（M3-7；`mc-ws` 与 axum ws 归 M3-7） |
| `tokio-tungstenite` | **不引**（属 M3-7；`Cargo.lock` 已预锁，`Cargo.toml` 未动） |
| DB 迁移、SQL、`lib.rs`/`mod.rs`/`mount.rs`/`Cargo.lock` 的跨片改动 | **不做**（见 §11.1 的唯一例外） |

**为什么不是「新建 crate」**：`crates/mc-daemon-proto` 在 M0/M1 脚手架里已预置（17 行占位 `lib.rs`），本片只填充它。

---

## 2. 冻结面清单（上游 → 本地）

| 上游文件 | 行数 | 上游内容 | 本地落点 | 本地行数 |
| --- | --- | --- | --- | --- |
| `pkg/protocol/messages.go` | 445 | 29 结构体 + 21 常量串 | `src/messages/{mod,envelope,daemon,task,chat,omit,double_option}.rs` | 860 |
| `pkg/protocol/events.go` | 218 | 109 事件常量（L6–L217） | `src/events.rs` | 491 |
| `internal/daemonws/hub.go` | 1154 | 帧参数 + RPC 分发 + 状态映射 | `src/rpc.rs` | 88 |
| `internal/daemon/client.go` | — | 三套能力集（common/WS/HTTP） | `src/capabilities.rs` | 167 |
| `internal/handler/daemon.go` | — | 心跳 ack / 能力头解析 / claim body | 同上 + `src/messages/daemon.rs` | — |

> 本地 15 个文件合计 2684 行（含 `tests/` 1032 行）；**每个文件都 < 800 行**（门 ⑩，`docs/35` §7）。

### 2.1 类型清单（29 → Rust 名）

`messages.go` 实测 **29** 个 `type ... struct`（不是 `docs/15` 沿用的 42，§11.2）。逐条映射：

| # | 上游类型 @ 行 | Rust | 落点 |
| --- | --- | --- | --- |
| 1 | `ChatQuickAction` L77 | `ChatQuickAction` | `chat.rs` |
| 2 | `RPCRequestPayload` L87 | `RPCRequestPayload` | `envelope.rs` |
| 3 | `RPCResponsePayload` L104 | `RPCResponsePayload` | `envelope.rs` |
| 4 | `Message` L112 | `Message` | `envelope.rs` |
| 5 | `TaskDispatchPayload` L118 | `TaskDispatchPayload` | `task.rs` |
| 6 | `TaskAvailablePayload` L127 | `TaskAvailablePayload` | `task.rs` |
| 7 | `RuntimeProfilesChangedPayload` L136 | `RuntimeProfilesChangedPayload` | `daemon.rs` |
| 8 | `WorkspacesChangedPayload` L144 | `WorkspacesChangedPayload` | `daemon.rs` |
| 9 | `PendingWorkPayload` L162 | `PendingWorkPayload` | `daemon.rs` |
| 10 | `TaskProgressPayload` L168 | `TaskProgressPayload` | `task.rs` |
| 11 | `TaskCompletedPayload` L176 | `TaskCompletedPayload` | `task.rs` |
| 12 | `ChatQuickActionsPayload` L186 | `ChatQuickActionsPayload` | `chat.rs` |
| 13 | `TaskMessagePayload` L201 | `TaskMessagePayload` | `task.rs` |
| 14 | `DaemonRegisterPayload` L221 | `DaemonRegisterPayload` | `daemon.rs` |
| 15 | `RuntimeInfo` L228 | `RuntimeInfo` | `daemon.rs` |
| 16 | `ChatMessagePayload` L235 | `ChatMessagePayload` | `chat.rs` |
| 17 | `ChatDonePayload` L278 | `ChatDonePayload` | `chat.rs` |
| 18 | `ChatCancelFinalizedPayload` L313 | `ChatCancelFinalizedPayload` | `chat.rs` |
| 19 | `ChatSessionReadPayload` L334 | `ChatSessionReadPayload` | `chat.rs` |
| 20 | `ChatSessionCreatedPayload` L338 | `ChatSessionCreatedPayload` | `chat.rs` |
| 21 | `ChatSessionChannelSource` L348 | `ChatSessionChannelSource` | `chat.rs` |
| 22 | `ChatSessionDeletedPayload` L357 | `ChatSessionDeletedPayload` | `chat.rs` |
| 23 | `ChatSessionUpdatedPayload` L365 | `ChatSessionUpdatedPayload` | `chat.rs` |
| 24 | `DaemonHeartbeatRequestPayload` L384 | `DaemonHeartbeatRequestPayload` | `daemon.rs` |
| 25 | `DaemonHeartbeatAckPayload` L401 | `DaemonHeartbeatAckPayload` | `daemon.rs` |
| 26 | `DaemonHeartbeatPendingUpdate` L423 | `DaemonHeartbeatPendingUpdate` | `daemon.rs` |
| 27 | `DaemonHeartbeatPendingModelList` L430 | `DaemonHeartbeatPendingModelList` | `daemon.rs` |
| 28 | `DaemonHeartbeatPendingLocalSkills` L436 | `DaemonHeartbeatPendingLocalSkills` | `daemon.rs` |
| 29 | `DaemonHeartbeatPendingLocalSkillImport` L442 | `DaemonHeartbeatPendingLocalSkillImport` | `daemon.rs` |

**命名规则**：Rust 类型名与上游 **逐字相同**（不加 `Payload`/`Dto` 后缀、不改大小写），以便「上游 → Rust」的检索是恒等映射。
唯一的线上键名偏差（Rust 保留字 / 惯例）用 `#[serde(rename)]` 处理，**字段名不改**：

| Rust 字段 | 线上键 | 原因 |
| --- | --- | --- |
| `Message.kind` | `type` | `type` 是 Rust 关键字 |
| `RuntimeInfo.kind` | `type` | 同上 |
| `TaskMessagePayload.kind` | `type` | 同上 |

### 2.2 常量串清单（`messages.go` 的 21 条）

| 组 | 条数 | 上游行 | Rust |
| --- | --- | --- | --- |
| `DaemonCapability*` | 11 | L6/7/8/9/10/22/26/32/37/50/63 | `capabilities::DAEMON_CAPABILITY_*` |
| `AppCapabilityChatDraftRestoreV1` | 1 | L72 | `capabilities::APP_CAPABILITY_CHAT_DRAFT_RESTORE_V1` |
| `PendingWorkKind*` | 3 | L151-153 | `messages::daemon::pending_work_kind::*` |
| `ChatMessageKind*` | 4 | L248/252/257/262 | `messages::chat::message_kind::*` |
| `ChatCancelOutcome*` | 2 | L297/301 | `messages::chat::cancel_outcome::*` |

---

## 3. 帧契约

### 3.1 信封

```jsonc
{ "type": "<事件名>", "payload": { /* 各事件自己的 body */ } }
```

- `Message.kind`（线上 `type`）：必填，**无** `omitempty`；缺失 → 解码为空串（Go 零值）。
- `Message.payload`：Rust 是 `serde_json::Value`；**键缺失 → `Value::Null`**，与上游 `json.RawMessage` 的 nil 同义。
- 未知 `type` **不得**解码失败：`is_known_event()` 只是谓词，未知事件必须被忽略（`src/events.rs`，用例 `unknown_events_are_ignored_not_rejected`）。

### 3.2 RPC 信封（`rpc-v1`）

请求：`{"type":"daemon:rpc_request","payload":{"request_id","method","body"?,"timeout_ms"?}}`
响应：`{"type":"daemon:rpc_response","payload":{"request_id","status","body"?,"error"?}}`

- `timeout_ms` 缺省 → 0；`hub.go:1026` **只在 `> 0` 时**才施加服务端超时。
- `status` 恒为 HTTP 语义码（§3.3）；`error` 在 `status >= 400` 时是给人看的一句话。

### 3.3 状态码映射（`hub.go:1007-1040`）

| 情形 | 状态 | 备注 |
| --- | --- | --- |
| 正常 | handler 的 HTTP 状态（`tasks.claim` = 200） | `daemon_rpc.go:59` 把 WS 请求**重新派发**成同进程 HTTP 请求，故 WS 与 HTTP body **逐字节一致** |
| 未知 method | 404 | `daemon_rpc.go:54`，body `unknown rpc method %q` |
| 在途 RPC 已满（`>= 8`） | 429 | `hub.go:1013-1015` |
| handler 不可用 | 503 | `hub.go:1007` |
| handler 返回 `status < 400` | 500 | `hub.go:1035` 兜底（防「看似成功实则无状态」） |
| 其余 | 原样透传 | `hub.go:1040` |

### 3.4 帧参数（`src/rpc.rs` ↔ `hub.go`）

| 名称 | 值 | 上游 |
| --- | --- | --- |
| `MAX_IN_FLIGHT_RPC_PER_CLIENT` | 8 | `hub.go:300` |
| `RPC_READ_LIMIT_BYTES` | 65536（64 KiB） | `hub.go:944` |
| `WRITE_WAIT_MS` | 10000 | `hub.go:17` |
| `PONG_WAIT_MS` | 60000 | `hub.go:18` |
| `PING_PERIOD_MS` | 54000（= `pongWait × 9 / 10`） | `hub.go:18` |

### 3.5 成功但**空体**的 daemon 路由

上游有两个 handler 走「只写状态码、不写 body」：`POST /api/daemon/tasks/{taskId}/fail` 与
`POST /api/daemon/tasks/{taskId}/session`。**M3-7 必须复刻**——不要「顺手补一个 `{"status":"ok"}`」，
那是**协议变更**（要按 §9 走仲裁）。判定方式：读 handler 末尾有无 `writeJSON`/`writeMeasuredJSON`。

---

## 4. 事件契约

- 权威：`pkg/protocol/events.go` L6–L217，**109** 条常量（`docs/15` §2.3 的 8 条 task 迁移注释在 L34–L42，属 M3-3）。
- 本地：`src/events.rs` 定义 109 个 `&str` 常量、`KNOWN_EVENTS: [&str; 109]`、`KNOWN_EVENT_COUNT = 109`、`is_known_event()`。
- **Rust 常量名 = 线上串的所有非字母数字折成 `_` 再大写**（`"issue_metadata:changed"` → `ISSUE_METADATA_CHANGED`）。
  该规则由用例 `event_table_is_complete_and_consistent` + `event_wire_names_match_upstream` 机械校验
（用 `stringify!` 反推常量名，避免手抄错；后者是 11 条重点事件的逐条对照）。
- **不定义 enum**：未知事件必须能被忽略（前向兼容），enum 会逼出 `#[serde(other)]` 的语义陷阱。

### 4.1 事件表（109 条，逐条可核对）

| 线上串 | Rust 常量 | 上游 Go 标识 | events.go |
| --- | --- | --- | --- |
| `issue:created` | `ISSUE_CREATED` | `EventIssueCreated` | events.go:6 |
| `issue:updated` | `ISSUE_UPDATED` | `EventIssueUpdated` | events.go:7 |
| `issue:deleted` | `ISSUE_DELETED` | `EventIssueDeleted` | events.go:8 |
| `issue_metadata:changed` | `ISSUE_METADATA_CHANGED` | `EventIssueMetadataChanged` | events.go:9 |
| `issue_attachments:changed` | `ISSUE_ATTACHMENTS_CHANGED` | `EventIssueAttachmentsChanged` | events.go:10 |
| `comment:created` | `COMMENT_CREATED` | `EventCommentCreated` | events.go:13 |
| `comment:updated` | `COMMENT_UPDATED` | `EventCommentUpdated` | events.go:14 |
| `comment:deleted` | `COMMENT_DELETED` | `EventCommentDeleted` | events.go:15 |
| `comment:resolved` | `COMMENT_RESOLVED` | `EventCommentResolved` | events.go:16 |
| `comment:unresolved` | `COMMENT_UNRESOLVED` | `EventCommentUnresolved` | events.go:17 |
| `reaction:added` | `REACTION_ADDED` | `EventReactionAdded` | events.go:18 |
| `reaction:removed` | `REACTION_REMOVED` | `EventReactionRemoved` | events.go:19 |
| `issue_reaction:added` | `ISSUE_REACTION_ADDED` | `EventIssueReactionAdded` | events.go:20 |
| `issue_reaction:removed` | `ISSUE_REACTION_REMOVED` | `EventIssueReactionRemoved` | events.go:21 |
| `agent:status` | `AGENT_STATUS` | `EventAgentStatus` | events.go:24 |
| `agent:created` | `AGENT_CREATED` | `EventAgentCreated` | events.go:25 |
| `agent:archived` | `AGENT_ARCHIVED` | `EventAgentArchived` | events.go:26 |
| `agent:restored` | `AGENT_RESTORED` | `EventAgentRestored` | events.go:27 |
| `task:queued` | `TASK_QUEUED` | `EventTaskQueued` | events.go:34 |
| `task:dispatch` | `TASK_DISPATCH` | `EventTaskDispatch` | events.go:35 |
| `task:running` | `TASK_RUNNING` | `EventTaskRunning` | events.go:36 |
| `task:waiting_local_directory` | `TASK_WAITING_LOCAL_DIRECTORY` | `EventTaskWaitingLocalDirectory` | events.go:37 |
| `task:progress` | `TASK_PROGRESS` | `EventTaskProgress` | events.go:38 |
| `task:completed` | `TASK_COMPLETED` | `EventTaskCompleted` | events.go:39 |
| `task:failed` | `TASK_FAILED` | `EventTaskFailed` | events.go:40 |
| `task:message` | `TASK_MESSAGE` | `EventTaskMessage` | events.go:41 |
| `task:cancelled` | `TASK_CANCELLED` | `EventTaskCancelled` | events.go:42 |
| `inbox:new` | `INBOX_NEW` | `EventInboxNew` | events.go:45 |
| `inbox:read` | `INBOX_READ` | `EventInboxRead` | events.go:46 |
| `inbox:unread` | `INBOX_UNREAD` | `EventInboxUnread` | events.go:47 |
| `inbox:archived` | `INBOX_ARCHIVED` | `EventInboxArchived` | events.go:48 |
| `inbox:unarchived` | `INBOX_UNARCHIVED` | `EventInboxUnarchived` | events.go:49 |
| `inbox:batch-read` | `INBOX_BATCH_READ` | `EventInboxBatchRead` | events.go:50 |
| `inbox:batch-archived` | `INBOX_BATCH_ARCHIVED` | `EventInboxBatchArchived` | events.go:51 |
| `workspace:updated` | `WORKSPACE_UPDATED` | `EventWorkspaceUpdated` | events.go:54 |
| `workspace:deleted` | `WORKSPACE_DELETED` | `EventWorkspaceDeleted` | events.go:55 |
| `member:added` | `MEMBER_ADDED` | `EventMemberAdded` | events.go:58 |
| `member:updated` | `MEMBER_UPDATED` | `EventMemberUpdated` | events.go:59 |
| `member:removed` | `MEMBER_REMOVED` | `EventMemberRemoved` | events.go:60 |
| `subscriber:added` | `SUBSCRIBER_ADDED` | `EventSubscriberAdded` | events.go:63 |
| `subscriber:removed` | `SUBSCRIBER_REMOVED` | `EventSubscriberRemoved` | events.go:64 |
| `activity:created` | `ACTIVITY_CREATED` | `EventActivityCreated` | events.go:67 |
| `skill:created` | `SKILL_CREATED` | `EventSkillCreated` | events.go:70 |
| `skill:updated` | `SKILL_UPDATED` | `EventSkillUpdated` | events.go:71 |
| `skill:deleted` | `SKILL_DELETED` | `EventSkillDeleted` | events.go:72 |
| `chat:message` | `CHAT_MESSAGE` | `EventChatMessage` | events.go:75 |
| `chat:done` | `CHAT_DONE` | `EventChatDone` | events.go:76 |
| `chat:quick_actions` | `CHAT_QUICK_ACTIONS` | `EventChatQuickActions` | events.go:81 |
| `chat:cancel_finalized` | `CHAT_CANCEL_FINALIZED` | `EventChatCancelFinalized` | events.go:87 |
| `chat:session_created` | `CHAT_SESSION_CREATED` | `EventChatSessionCreated` | events.go:88 |
| `chat:session_read` | `CHAT_SESSION_READ` | `EventChatSessionRead` | events.go:89 |
| `chat:session_deleted` | `CHAT_SESSION_DELETED` | `EventChatSessionDeleted` | events.go:90 |
| `chat:session_updated` | `CHAT_SESSION_UPDATED` | `EventChatSessionUpdated` | events.go:91 |
| `project:created` | `PROJECT_CREATED` | `EventProjectCreated` | events.go:94 |
| `project:updated` | `PROJECT_UPDATED` | `EventProjectUpdated` | events.go:95 |
| `project:deleted` | `PROJECT_DELETED` | `EventProjectDeleted` | events.go:96 |
| `project_resource:created` | `PROJECT_RESOURCE_CREATED` | `EventProjectResourceCreated` | events.go:97 |
| `project_resource:updated` | `PROJECT_RESOURCE_UPDATED` | `EventProjectResourceUpdated` | events.go:98 |
| `project_resource:deleted` | `PROJECT_RESOURCE_DELETED` | `EventProjectResourceDeleted` | events.go:99 |
| `label:created` | `LABEL_CREATED` | `EventLabelCreated` | events.go:102 |
| `label:updated` | `LABEL_UPDATED` | `EventLabelUpdated` | events.go:103 |
| `label:deleted` | `LABEL_DELETED` | `EventLabelDeleted` | events.go:104 |
| `issue_labels:changed` | `ISSUE_LABELS_CHANGED` | `EventIssueLabelsChanged` | events.go:105 |
| `property:created` | `PROPERTY_CREATED` | `EventPropertyCreated` | events.go:109 |
| `property:updated` | `PROPERTY_UPDATED` | `EventPropertyUpdated` | events.go:110 |
| `issue_properties:changed` | `ISSUE_PROPERTIES_CHANGED` | `EventIssuePropertiesChanged` | events.go:111 |
| `issue_status:changed` | `ISSUE_STATUS_CHANGED` | `EventIssueStatusChanged` | events.go:120 |
| `pin:created` | `PIN_CREATED` | `EventPinCreated` | events.go:123 |
| `pin:deleted` | `PIN_DELETED` | `EventPinDeleted` | events.go:124 |
| `pin:reordered` | `PIN_REORDERED` | `EventPinReordered` | events.go:125 |
| `invitation:created` | `INVITATION_CREATED` | `EventInvitationCreated` | events.go:128 |
| `invitation:accepted` | `INVITATION_ACCEPTED` | `EventInvitationAccepted` | events.go:129 |
| `invitation:declined` | `INVITATION_DECLINED` | `EventInvitationDeclined` | events.go:130 |
| `invitation:revoked` | `INVITATION_REVOKED` | `EventInvitationRevoked` | events.go:131 |
| `autopilot:created` | `AUTOPILOT_CREATED` | `EventAutopilotCreated` | events.go:134 |
| `autopilot:updated` | `AUTOPILOT_UPDATED` | `EventAutopilotUpdated` | events.go:135 |
| `autopilot:deleted` | `AUTOPILOT_DELETED` | `EventAutopilotDeleted` | events.go:136 |
| `autopilot:run_start` | `AUTOPILOT_RUN_START` | `EventAutopilotRunStart` | events.go:137 |
| `autopilot:run_done` | `AUTOPILOT_RUN_DONE` | `EventAutopilotRunDone` | events.go:138 |
| `squad:created` | `SQUAD_CREATED` | `EventSquadCreated` | events.go:141 |
| `squad:updated` | `SQUAD_UPDATED` | `EventSquadUpdated` | events.go:142 |
| `squad:deleted` | `SQUAD_DELETED` | `EventSquadDeleted` | events.go:143 |
| `daemon:heartbeat` | `DAEMON_HEARTBEAT` | `EventDaemonHeartbeat` | events.go:146 |
| `daemon:heartbeat_ack` | `DAEMON_HEARTBEAT_ACK` | `EventDaemonHeartbeatAck` | events.go:147 |
| `daemon:register` | `DAEMON_REGISTER` | `EventDaemonRegister` | events.go:148 |
| `daemon:task_available` | `DAEMON_TASK_AVAILABLE` | `EventDaemonTaskAvailable` | events.go:149 |
| `daemon:runtime_profiles_changed` | `DAEMON_RUNTIME_PROFILES_CHANGED` | `EventDaemonRuntimeProfilesChanged` | events.go:150 |
| `daemon:workspaces_changed` | `DAEMON_WORKSPACES_CHANGED` | `EventDaemonWorkspacesChanged` | events.go:151 |
| `daemon:pending_work` | `DAEMON_PENDING_WORK` | `EventDaemonPendingWork` | events.go:160 |
| `daemon:rpc_request` | `DAEMON_RPC_REQUEST` | `EventDaemonRPCRequest` | events.go:166 |
| `daemon:rpc_response` | `DAEMON_RPC_RESPONSE` | `EventDaemonRPCResponse` | events.go:167 |
| `github_installation:created` | `GITHUB_INSTALLATION_CREATED` | `EventGitHubInstallationCreated` | events.go:170 |
| `github_installation:deleted` | `GITHUB_INSTALLATION_DELETED` | `EventGitHubInstallationDeleted` | events.go:171 |
| `pull_request:linked` | `PULL_REQUEST_LINKED` | `EventPullRequestLinked` | events.go:172 |
| `pull_request:updated` | `PULL_REQUEST_UPDATED` | `EventPullRequestUpdated` | events.go:173 |
| `pull_request:unlinked` | `PULL_REQUEST_UNLINKED` | `EventPullRequestUnlinked` | events.go:174 |
| `vcs_connection:created` | `VCS_CONNECTION_CREATED` | `EventVCSConnectionCreated` | events.go:177 |
| `vcs_connection:deleted` | `VCS_CONNECTION_DELETED` | `EventVCSConnectionDeleted` | events.go:178 |
| `lark_installation:created` | `LARK_INSTALLATION_CREATED` | `EventLarkInstallationCreated` | events.go:186 |
| `lark_installation:revoked` | `LARK_INSTALLATION_REVOKED` | `EventLarkInstallationRevoked` | events.go:187 |
| `slack_installation:created` | `SLACK_INSTALLATION_CREATED` | `EventSlackInstallationCreated` | events.go:194 |
| `slack_installation:revoked` | `SLACK_INSTALLATION_REVOKED` | `EventSlackInstallationRevoked` | events.go:195 |
| `dingtalk_installation:created` | `DINGTALK_INSTALLATION_CREATED` | `EventDingTalkInstallationCreated` | events.go:201 |
| `dingtalk_installation:revoked` | `DINGTALK_INSTALLATION_REVOKED` | `EventDingTalkInstallationRevoked` | events.go:202 |
| `dingtalk_installation:binding_updated` | `DINGTALK_INSTALLATION_BINDING_UPDATED` | `EventDingTalkAccountBindingUpdated` | events.go:203 |
| `wecom_installation:created` | `WECOM_INSTALLATION_CREATED` | `EventWecomInstallationCreated` | events.go:211 |
| `wecom_installation:revoked` | `WECOM_INSTALLATION_REVOKED` | `EventWecomInstallationRevoked` | events.go:212 |
| `telegram_installation:created` | `TELEGRAM_INSTALLATION_CREATED` | `EventTelegramInstallationCreated` | events.go:216 |
| `telegram_installation:revoked` | `TELEGRAM_INSTALLATION_REVOKED` | `EventTelegramInstallationRevoked` | events.go:217 |

---

## 5. 能力协商与版本策略

### 5.1 三套能力集（`daemon/client.go`）

| 集合 | 条数 | 组成 | 上游 |
| --- | --- | --- | --- |
| common | 10 | skill-bundles-v1, coalesced-comments-v1, execution-manifest-v1, agent-skill-v1, remote-mcp-v1, local-worktree-v1, source_context_quick_create_v1, rpc-v1, platform-skill-v1, checkout-keeps-work-v1 | `client.go:203` |
| WS = common + 1 | 11 | 追加 `claim-poll-hints-v1` | `client.go:186-192` |
| HTTP | 10 | 与 common 同集 | `client.go:196` |

Rust：`daemon_common_capabilities()` / `daemon_ws_capabilities()` / `daemon_http_capabilities()`（`src/capabilities.rs`），
顺序、条数、内容由用例 `capability_negotiation` 逐条断言（含「无空白字符」「无重复」）。

### 5.2 服务端能力回执

- 头名：`X-Client-Capabilities`（`CLIENT_CAPABILITIES_HEADER`）；值 = `,` 分隔，逐项 `TrimSpace` 后**丢弃空项**；头缺失 → **nil**（老 daemon）。
- 心跳 ack 的 `server_capabilities` **恒为** `["rpc-v1"]`（`DaemonCapabilityRPCV1`，`daemon.go:1379`），Rust 侧 `SERVER_HEARTBEAT_CAPABILITIES`。
- 判定 `request_has_client_capability` 用**精确相等**（不做子串匹配，用例已锁）。
- `runtime_has_capability(Option<&[u8]>, &str)`：解析 `agent_runtime.capabilities` JSON 数组，**失败即 false**（fail-closed，对齐 `daemon.go:1594`）。

### 5.3 版本策略（冻结）

1. **只增不改**：字段可加（新增字段必须可选或带零值语义）、**不可**改名/改类型/改必填性；删字段视为破坏性变更。
2. **未知容忍**：所有 payload 结构体在**容器级**加 `#[serde(default)]`，与 Go 的零值解码一致——缺字段永不报错，必填性由 handler 判定（上游同款）。
3. **未知事件/未知 method 忽略**：事件表是**谓词**不是闭环；RPC 未知 method 回 404 而非断连。
4. **能力门**：新行为必须挂在一个 `*_v1` 能力串后面，且仅当对端 `X-Client-Capabilities` 里出现该串时才启用；服务端不回执能力串 = 老 daemon = 走降级路径。
5. **`rpc-v1` 是传输层的唯一开关**：没有 `rpc-v1` 的 daemon 只能走 HTTP 面（§6.1 全部 36 条）。
6. 帧参数（§3.4）**是契约**：改 `8`/`64 KiB`/ping 周期属协议变更。

---

## 6. 冻结契约表

### 6.1 daemon 面 36 条（§1.5）

| method | path | handler | router.go | 请求体 | 200 响应 | 幂等 | 错误码 | 备注 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| POST | `/api/daemon/register` | DaemonRegister | L1523 | `DaemonRegisterRequest` | `{"runtimes":…,"repos":…,"repos_version":…,"settings":…}` | LW | 400 `invalid request body`; 400 `daemon_id is required`; 400 `workspace_id is required`; 400 `at least one runtime or failed profile is required`; 404 `workspace not found`; 400 `invalid profile_id`; 400 `invalid workspace_id` | 按 runtime 集合 upsert；`runtime_id` 由服务端生成 |
| POST | `/api/daemon/deregister` | DaemonDeregister | L1524 | `inline{runtime_ids,offline_reasons}` | `{"status":"ok"}` | LW | `G_ws²`; 400 `invalid request body`; 400 `runtime_ids is required`; 500 `failed to load runtimes` | 把 `runtime_ids` 置离线（`offline_reasons` 记因） |
| POST | `/api/daemon/heartbeat` | DaemonHeartbeat | L1525 | `DaemonHeartbeatRequest` | HTTP `{"status":…,"pending_*":…}` ／ WS `protocol.DaemonHeartbeatAckPayload` | LW | `G_ws`; 400 `invalid request body`; 400 `runtime_id is required`; 404 `runtime not found`; 500 `failed to load runtime`; 500 `heartbeat failed`; 400 `invalid runtime_id` | **HTTP/WS 两种 ack 形状不同**，见 §6.4 |
| GET | `/api/daemon/ws` | DaemonWebSocket | L1526 | — | 帧流（§3） | WS | 400 `runtime_ids or user identity required` | 长连接，帧契约见 §3 |
| GET | `/api/daemon/workspaces` | ListDaemonWorkspaces | L1527 | — | `[]DaemonWorkspaceResponse`（`daemon_workspace.go:16`） | R | 500 `failed to list daemon workspaces`; 401 `daemon workspace identity required`; 404 `workspace not found` |  |
| GET | `/api/daemon/workspaces/{workspaceId}/repos` | GetDaemonWorkspaceRepos | L1528 | — | `daemonWorkspaceReposResponse`（`daemon.go:278`） | R | `G_ws`; 404 `workspace not found` | 带 `repos_version`/`settings` |
| GET | `/api/daemon/workspaces/{workspaceId}/runtime-profiles` | DaemonListRuntimeProfiles | L1529 | — | `{"workspace_id":…,"runtime_profiles":…}` | R | `G_ws`; 500 `failed to list runtime profiles`; 400 `invalid workspace id` |  |
| POST | `/api/daemon/tasks/{id}/plugin-hooks` | InvokeAgentPluginHook | L1534 | `invokeAgentHookRequest` | {"status":…,"error":…}; hook 返回值（`result` 透传） | AP | `G_task`; `G_plug`; 400 `invalid request body`; 400 `installation_id and hook_key are required` | 通道属 M3、**内容属 W6**；无 plugin host → 403 `plugin_api_disabled`；重复调用重复执行 hook |
| GET | `/api/daemon/tasks/{id}/plugin-mcp/{contributionId}/credential` | ResolvePluginMCPCredential | L1537 | — | `{"credential_header":…,"credential":…}` | R | `G_task`; `G_plug`; 400 `malformed contribution id` | 同属 W6 内容 |
| POST | `/api/daemon/runtimes/{runtimeId}/tasks/claim` | ClaimTaskByRuntime | L1539 | — | `{"task": AgentTaskResponse 或 null}` | POLL | `G_rt`; 500 `runtime owner required to mint task token`; 500 `failed to mint task token`; 500 `failed to mint Remote MCP daemon token`; 500 `failed to finalize task claim` | 无任务时 200 `{"task":null}`，**不是** 404 |
| POST | `/api/daemon/tasks/claim` | ClaimTasksByRuntime | L1543 | `inline{daemon_id,runtime_ids,max_tasks}` | `{tasks:[]AgentTaskResponse, claim_poll_hint_supported, next_deferred_task_after_ms}` | POLL | `G_ws²`; 400 `invalid request body`; 400 `daemon_id is required`; 403 `daemon_id does not match token`; 400 `max_tasks must not be negative`; 500 `failed to load runtimes` | `max_tasks=0` → 200 空列表；>cap 截断 |
| POST | `/api/daemon/claim` | ClaimTasksByRuntime | L1544 | `inline{daemon_id,runtime_ids,max_tasks}` | 同上 | POLL | `G_ws²`; 400 `invalid request body`; 400 `daemon_id is required`; 403 `daemon_id does not match token`; 400 `max_tasks must not be negative`; 500 `failed to load runtimes` | 与上一行同 handler、同 body（历史别名） |
| POST | `/api/daemon/runtimes/{runtimeId}/tasks/{taskId}/prepare-lease` | ExtendTaskPrepareLease | L1545 | — | `AgentTaskResponse`（`agent.go:359`） | LW | `G_rt`; `G_task`; 404 `task not found` | 续 `prepare` 租约 |
| POST | `/api/daemon/runtimes/{runtimeId}/tasks/{taskId}/skill-bundles/resolve` | ResolveTaskSkillBundles | L1546 | `resolveSkillBundlesRequest` | `{"bundles:[]service.AgentSkillData":…}` | R | `G_rt`; `G_task`; 404 `task not found`; 409 `task is not preparing`; 400 `invalid request body`; 400 `invalid skill ref`; 500 `failed to load skill bundles`; 404 `skill bundle not found`; 409 `pinned plugin skill bundle hash mismatch` | pin bundle 哈希；任务非 `preparing` → 409 |
| GET | `/api/daemon/runtimes/{runtimeId}/tasks/pending` | ListPendingTasksByRuntime | L1547 | — | `[]AgentTaskResponse` | R | `G_rt`; 500 `failed to list pending tasks` | 裸数组，无信封 |
| POST | `/api/daemon/runtimes/{runtimeId}/update/{updateId}/result` | ReportUpdateResult | L1548 | `inline{status,output,error}` | `{"status":"ok"}` | LW | `G_rt`; 404 `update not found`; 400 `invalid request body`; 500 `failed to persist completion`; 500 `failed to persist failure` | 终态由 daemon 给，服务端不再改 |
| POST | `/api/daemon/runtimes/{runtimeId}/models/{requestId}/result` | ReportModelListResult | L1549 | 内联 `{}` | `{"status":"ok"}` | LW | `G_rt`; 404 `request not found`; 400 `invalid request body`; 500 `failed to persist completion`; 500 `failed to persist failure` |  |
| POST | `/api/daemon/runtimes/{runtimeId}/local-skills/{requestId}/result` | ReportLocalSkillListResult | L1550 | 内联 `{}` | `{"status":"ok"}` | LW | `G_rt`; 404 `request not found`; 400 `invalid request body`; 500 `failed to persist completion`; 500 `failed to persist failure` |  |
| POST | `/api/daemon/runtimes/{runtimeId}/local-skills/import/{requestId}/result` | ReportLocalSkillImportResult | L1551 | 内联 `{}` | `{"status":"ok"}` | LW | `G_rt`; 404 `request not found`; 400 `invalid request body`; 500 `failed to persist import completion` |  |
| GET | `/api/daemon/tasks/{taskId}/status` | GetTaskStatus | L1553 | — | `{"status":"ok"}` | R | `G_ws`; 404 `task not found`; 500 `failed to load task`; 400 `invalid task_id` | 只回 `status` |
| POST | `/api/daemon/tasks/{taskId}/start` | StartTask | L1554 | — | `AgentTaskResponse` | TR | `G_task` |  |
| POST | `/api/daemon/tasks/{taskId}/wait-local-directory` | MarkTaskWaitingLocalDirectory | L1555 | `TaskWaitLocalDirectoryRequest` | `AgentTaskResponse` | TR | `G_task`; 400 `invalid request body` |  |
| POST | `/api/daemon/tasks/{taskId}/progress` | ReportTaskProgress | L1556 | `TaskProgressRequest` | `{"status":"ok"}` | LW | `G_task`; 400 `invalid request body` | **覆盖**语义（step/total/summary），不累积 |
| POST | `/api/daemon/tasks/{taskId}/complete` | CompleteTask | L1557 | `TaskCompleteRequest` | `AgentTaskResponse` | TR | `G_task`; 400 `invalid request body` |  |
| POST | `/api/daemon/tasks/{taskId}/fail` | FailTask | L1558 | `TaskFailRequest` | **空体** | TR | `G_task`; 400 `invalid request body` | **成功无响应体**（上游无 `writeJSON`）—— M3-7 不要“顺手补 `{status:ok}`” |
| POST | `/api/daemon/tasks/{taskId}/usage` | ReportTaskUsage | L1559 | `inline{usage}` | `{"status":"ok"}` | LW | `G_task`; 400 `invalid request body` | 逐条 upsert；**单条失败只记日志不中断**，整体仍 200 |
| POST | `/api/daemon/tasks/{taskId}/messages` | ReportTaskMessages | L1560 | `TaskMessageBatchRequest` | `{"status":"ok"}` | AP | `G_task`; 400 `invalid request body`; 500 `failed to persist task message` | 批量**追加**；无幂等键 ⇒ 重发会重复入库 |
| GET | `/api/daemon/tasks/{taskId}/messages` | ListTaskMessages | L1561 | — | `[]protocol.TaskMessagePayload` | R | `G_task`; 400 `invalid since parameter`; 500 `failed to list task messages` | `since` 非法 → 400 `invalid since parameter` |
| POST | `/api/daemon/tasks/{taskId}/cancel-ack` | AckTaskCancelled | L1562 | `TaskCancelAckRequest` | `{"status":"ok"}` | LW | `G_task`; 500 `failed to record durable work directory`; 500 `failed to record branch name`; 500 `failed to record task error` | 落盘 branch/durable_work_dir/错误信息 |
| POST | `/api/daemon/workspaces/{workspaceId}/issues/gc-check` | BatchIssueGCCheck | L1564 | `batchIssueGCCheckRequest` | `{"issues":…}` | R | `G_ws`; 400 `invalid request body`; 400 `too many issue_ids`; 400 `invalid issue_id`; 500 `failed to check issues`; 400 `invalid workspace_id` | POST 但纯读；`issue_ids` 有上限 |
| GET | `/api/daemon/issues/{issueId}/gc-check` | GetIssueGCCheck | L1565 | — | `{"status":…,"category":…,"updated_at":…}` | R | `G_ws`; 404 `issue not found`; 400 `invalid issue_id` |  |
| GET | `/api/daemon/chat-sessions/{sessionId}/gc-check` | GetChatSessionGCCheck | L1566 | — | `{"status":…,"updated_at":…}` | R | `G_ws`; 404 `chat session not found`; 400 `invalid session_id` |  |
| GET | `/api/daemon/autopilot-runs/{runId}/gc-check` | GetAutopilotRunGCCheck | L1567 | — | `{"status":…,"completed_at":…}` | R | `G_ws`; 404 `autopilot run not found`; 400 `invalid run_id` |  |
| GET | `/api/daemon/tasks/{taskId}/gc-check` | GetTaskGCCheck | L1568 | — | `{"status":…,"completed_at":…}` | R | `G_task` |  |
| POST | `/api/daemon/runtimes/{runtimeId}/recover-orphans` | RecoverOrphanedTasks | L1570 | — | `{"orphaned":…,"retried":…}` | TR | `G_rt`; 500 `recover orphans failed` | 按租约过期判孤儿，走统一失败管线（可能自动重试） |
| POST | `/api/daemon/tasks/{taskId}/session` | PinTaskSession | L1571 | `PinTaskSessionRequest` | **空体**（handler 末尾无 `writeJSON`，与 `fail` 同款） | LW | `G_task`; 400 `invalid request body`; 400 `session_id or work_dir required`; 500 `pin session failed` | pin `session_id`/`work_dir`；两者皆空 → 400 |

### 6.2 异步请求-应答 8 条（§1.2）

闭环三段：**用户面 `POST Initiate*`（C=创建）→ daemon 面 `POST …/result`（§6.1，LW）→ 用户面 `GET Get*Request`（R=轮询）**。
下表即用户面这 8 条；`…/result` 侧见 §6.1 对应行（3 个 `local-skills` 变体 + update + models）。

| method | path | handler | router.go | 请求体 | 200 响应 | 幂等 | 错误码 | 备注 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| POST | `/api/runtimes/{runtimeId}/update` | `h.InitiateUpdate` | L2273 | `inline{target_version}` | `UpdateRequest`（`runtime_update.go:31`） | C | `G_rr`; 403 `only runtime owners and workspace admins can update runtimes`; 400 `invalid request body`; 400 `target_version is required`; 500 `failed to start the update` | `UpdateRequest` 落 `pending`；`HasPending` 去重 |
| GET | `/api/runtimes/{runtimeId}/update/{updateId}` | `h.GetUpdate` | L2274 | — | `UpdateRequest` | R | `G_rr`; 404 `update not found`; 403 `only runtime owners, workspace admins, and the update initiator can view this update` | `pending|running|completed|failed|timeout` |
| POST | `/api/runtimes/{runtimeId}/models` | `h.InitiateListModels` | L2275 | — | `ModelListRequest`（`runtime_models.go:56`） | C | `G_rr`; 503 `runtime is offline` | `ModelListRequest` 落 `pending`；runtime 离线 → 503 |
| GET | `/api/runtimes/{runtimeId}/models/{requestId}` | `h.GetModelListRequest` | L2276 | — | `ModelListRequest` | R | `G_rr`; 404 `request not found` | 缓存命中时 `cached/cached_at` 有值 |
| POST | `/api/runtimes/{runtimeId}/local-skills` | `h.InitiateListLocalSkills` | L2277 | — | `RuntimeLocalSkillListRequest`（`runtime_local_skills.go:203`） | C | `G_rc`; 503 `runtime is offline` | 清单请求；runtime 离线 → 503 |
| GET | `/api/runtimes/{runtimeId}/local-skills/{requestId}` | `h.GetLocalSkillListRequest` | L2278 | — | `RuntimeLocalSkillListRequest` | R | `G_rc`; 404 `request not found` |  |
| POST | `/api/runtimes/{runtimeId}/local-skills/import` | `h.InitiateImportLocalSkill` | L2279 | `CreateRuntimeLocalSkillImportRequest` | `RuntimeLocalSkillImportRequest`（`runtime_local_skills.go:217`） | C | `G_ls`; 503 `runtime is offline`; 400 `invalid request body`; 400 `skill_key is required`; 400 `invalid action`; 400 `invalid target_skill_id` | **owner-only**（`G_ls`） |
| GET | `/api/runtimes/{runtimeId}/local-skills/import/{requestId}` | `h.GetLocalSkillImportRequest` | L2280 | — | `RuntimeLocalSkillImportRequest` | R | `G_ls`; 404 `request not found` |  |

**状态枚举（冻结值集）**

| 请求体 | 状态字段值集 | 上游 |
| --- | --- | --- |
| `UpdateRequest.status` | `pending`、`running`、`completed`、`failed`、`timeout` | `runtime_update.go:22-27` |
| `ModelListRequest.status` | `pending`、`running`、`completed`、`failed`、`timeout` | `runtime_models.go:40-46` |
| `RuntimeLocalSkillListRequest.status` | `pending`、`running`、`completed`、`failed`、`timeout` | `runtime_local_skills.go:25-35` |
| `RuntimeLocalSkillImportRequest.status` | 同上 **+ `conflict`（终态，且不是错误）** | `runtime_local_skills.go:29-35` |

- 超时由**服务端**判定并写成 `timeout`：update 待办 120s、running 150s（`runtime_update.go:44-68`）。
- `UpdateRequest.RunStartedAt` / `ModelListRequest.RunStartedAt` / `RuntimeLocalSkillImportRequest.CreatorID` 是 `json:"-"` ⇒ **不出现在线上**（Rust 侧不建模）。
- **`conflict` 是新契约（MUL-2800）**：同名的全新导入会以终态 `conflict` 结束，并在 `conflict` 字段里带结构化信息（可否 overwrite 等），
  供桌面端/CLI 提示「覆盖 / 改名 / 跳过」，而不是静默失败。是否启用由**发起时**的 `supports_conflict` 记录决定。
  `LocalSkillImportAction` 只有两个取值：`""`（默认 = create）与 `"overwrite"`（按 `target_skill_id` 覆盖，仅创建者可覆盖）。
- M3-7 实现这三个变体时必须保留 `conflict` 的**非错误**语义（`status=conflict` 时 `error` 为空）。

### 6.3 幂等标签定义

标签是**按上游写语义人工判定**的（上游没有声明字段），判定规则如下，M3-7 用它决定「重试/重发是否安全」：

| 标签 | 含义 | 重发是否安全 |
| --- | --- | --- |
| `R` | 纯读（含 POST 的 `gc-check`、`resolve`） | 是 |
| `LW` | 最后写胜：同一 body 重发得到同一状态 | 是 |
| `TR` | 状态迁移守卫：由 handler 内部状态判断，重发多半返回守卫错误或幂等成功 | 需读 §6.1 备注 |
| `AP` | 追加/重复执行：重发会改变结果（重复入库、重复执行 hook） | **否**，需业务去重键 |
| `POLL` | 轮询式领取：重发会领到**下一批**（不会重复持有同一任务） | 是（语义是「再领一次」） |
| `WS` | 长连接，非请求-应答 | — |
| `C` | 创建待办请求（用户面发起） | 由 `HasPending` 去重后安全 |

### 6.4 两处「同名不同形」的实测事实（易踩）

1. **心跳 ack**：HTTP 面是 `{status} ∪ pending_*`（`daemon.go:1186-1202`，**故意不回** `runtime_id` 与 `server_capabilities`，注释写明「redundant noise on the HTTP path」）；WS 面是完整 `DaemonHeartbeatAckPayload`（`hub.go:800`）。Rust 里冻结的是**协议类型**（= WS 形），HTTP 形在 §6.1 单独标注。
2. **`claim` 三条路由**：`POST /api/daemon/tasks/claim` 与 `POST /api/daemon/claim` 同 handler；单 runtime 版 `POST /api/daemon/runtimes/{runtimeId}/tasks/claim` 是无任务时 `{"task":null}` 的**单条**形状。WS 的 `tasks.claim` RPC（§3.2）**只**对应批量版。

### 6.5 WS RPC method 表（1 条）

| method | 上游分发点 | 等价 HTTP | 请求体 | 响应体 |
| --- | --- | --- | --- | --- |
| `tasks.claim` | `daemon_rpc.go:51-54`（switch） | `POST /api/daemon/tasks/claim` | `{daemon_id, runtime_ids, max_tasks}` | `{tasks:[AgentTaskResponse…], claim_poll_hint_supported, next_deferred_task_after_ms}` |

Rust：`rpc::method::{TASKS_CLAIM, KNOWN, is_known}`；用例 `rpc_method_table_matches_upstream` 内联抄了上游字符串与行号，
逐条比对（改了上游就会红，逼人回来核对）。

---

## 7. 鉴权守卫与错误码别名

### 7.1 守卫（§6 表的 `G_*` 别名展开）

| 别名 | 上游 | 产生的响应 |
| --- | --- | --- |
| `G_ws` | `requireDaemonWorkspaceAccess`（`daemon.go:56`） | 404 `not found`（工作区空 / token 工作区不匹配 / 非成员） |
| `G_ws²` | `verifyDaemonWorkspaceAccess`（`daemon.go:170`） | **不写响应**，仅返回 bool（循环里静默跳过该项） |
| `G_rt` | `requireDaemonRuntimeAccess`（`daemon.go:92`） | 400 `invalid runtime_id`；404 `runtime not found`（仅 `pgx.ErrNoRows`）；404 `not found`（工作区不通）；500 `failed to load runtime` |
| `G_task` | `requireDaemonTaskAccessWithWorkspace`（`daemon.go:126`） | 400 `invalid task_id`；404 `task not found`；404 `not found`；500 `failed to load task` |
| `G_rr` | `requireRuntimeReadAccess`（`runtime.go` 族） | 404 / 403（runtime 不可读 or 非成员） |
| `G_rc` | `requireRuntimeCapabilityReadAccess`（`runtime_local_skills.go:552`） | 同 `G_rr` |
| `G_ls` | `requireRuntimeLocalSkillAccess`（`runtime_local_skills.go:572`） | 同 `G_rc` + 403 `insufficient permissions`（**owner-only**） |
| `G_plug` | `requirePluginsV1`（`plugin.go:20`） | 403 `code=plugin_api_disabled`，`msg=Plugin management is not enabled` |

**两条必须照抄的语义**（上游注释里写明，且已有专门的回归）：

- `G_rt` / `G_task` 只在**确认查无此行**（`isNotFound`）时才 404。daemon 收到 404 会**杀掉正在跑的 agent** 并从内存表里删掉该 runtime，
  所以瞬时 DB 故障必须落 500——把「我不知道」说成「已删除」会造成自伤（`daemon.go:87-91`、`daemon.go:145-150`，MUL-7259/GH#8272）。
- 工作区不匹配一律 **404 `not found`**（不是 403），避免泄漏「这个 id 存在」。

### 7.2 UUID 参数

路径/body 里的 UUID 一律走 `parseUUIDOrBadRequest(w, s, fieldName)`（`handler.go:669`）→ 失败 `400 "invalid "+fieldName`。
§6.1 表的错误码列已把每条路由的 `fieldName` 展开（`runtime_id` / `task_id` / `issue_id` / `session_id` / `run_id` / `workspace_id`）。

### 7.3 通用响应写出

| 函数 | 用法 | 备注 |
| --- | --- | --- |
| `writeJSON(w, status, v)` | 常规 | |
| `writeMeasuredJSON(w, status, v)` | 需要量 body 字节数的热路径（claim / heartbeat） | 返回值是字节数，**语义上等价**于 `writeJSON` |
| `writeError(w, status, msg)` | 单条错误 | body 形状：`{"error": msg}` |
| `writeErrorCode(w, status, code, msg)` | 带机读码（如 `plugin_api_disabled`） | body 多一个 `code` |

---

## 8. 变更请求的登记处

本片**没有**发现任何需要偏离上游的点（无 golden fixture 未决项）。冻结后新增的偏离一律先写进本表，再按 §9 仲裁：

| # | 日期 | 变更 | 提出方 | 状态 |
| --- | --- | --- | --- | --- |
| — | — | （空） | — | — |

---

## 9. 变更流程（冻结后）

1. **触发**：任何 W3b/W3c 切片发现上游协议与 Rust 类型不符（字段名、可选性、状态码、帧形状）。
2. **权威顺序**（`docs/15` §8.4.1 / §8.4.3）：**golden fixture（W0-C 抽取器产物）> 本文件 §6 的冻结表 > 消费侧实现**；
   未覆盖时 `docs/plan1.md` > `docs/01-PLAN.md`，上游 `router.go`/`migrations/upstream/` > 其 Rust 等价物，且**上游事实 > 本仓既有实现**。
3. **取证**：给出上游 `file:line` 原文 + 一份最小 JSON（或 `hub.go` 状态码行号）。
4. **落地**：改 `crates/mc-daemon-proto` + 本文件 + 受影响用例；`tests/contract.rs` 的上游字符串/行号必须同步更新（红就是提醒）。
5. **禁止**：在**消费侧**（`mc-http` / `mc-ws` / `mc-task`）就地打补丁绕过冻结类型——那会让两处形状分叉。
6. **schema 分歧**不走本流程：一律回 W0-B2（`docs/15` §8.4.4，本片不新增迁移）；范围分歧走 §8.4.5（不得擅自把上游路由塞进 M3）。
7. **接入点**（`docs/15` §8.5）：W0-C 抽取器落地后，本片 `tests/golden.rs` 的手推 golden 应改为**读抽取器产物**；
   在此之前本文件的 §11.4 是已知风险（见该节）。

---

## 10. 复算命令（自证）

```bash
# 0) 前置：完整 blob 浅克隆（不要用 --filter=blob:none，见 docs/20）
#    /tmp/up1371/full @ f41fae6b08fb734afcbd13205c0b3203dd0bc9c6
cd /tmp/up1371/full

# 1) payload 类型数量（应为 29；最后一个 `WorkspacesChangedPayload struct{}` 无空格，
#    所以模式不能写成 '^type .* struct {'——那会数出 28）
grep -cE '^type .*struct' server/pkg/protocol/messages.go
grep -cE '^\s+(DaemonCapability|AppCapability|PendingWorkKind|ChatMessageKind|ChatCancelOutcome)' server/pkg/protocol/messages.go

# 2) 事件常量数量（应为 109；注意 const 块是等号对齐的，模式里 '=' 前是 ' *= *'）
grep -cE '^\s+[A-Za-z]+ *= *"' server/pkg/protocol/events.go

# 3) 协议文件行数（445 / 218 / 1154）
wc -l server/pkg/protocol/messages.go server/pkg/protocol/events.go server/internal/daemonws/hub.go

# 4) 唯一 RPC method（应为 1 条分支 + 404 default）
grep -n 'case "' server/internal/handler/daemon_rpc.go

# 5) 契约表与 §1.5/§1.2 行号逐条比对（本文件 §6 的表即由此生成）
#    上游 handler 体内的请求/响应/错误码为机械抽取，见 §10.1；行号来自 docs/15 §1.5/§1.2

# 6) 本地侧自证
cd <repo>
PATH="$HOME/.cargo/bin:$PATH" cargo test -p mc-daemon-proto      # 16 用例（7 contract + 4 golden + 5 roundtrip）
PATH="$HOME/.cargo/bin:$PATH" cargo clippy -p mc-daemon-proto --all-targets -- -D warnings
bash scripts/gates.sh --with-db                                   # 10 道门
python3 scripts/file_size_check.py --quiet                        # 门 ⑩
```

### 10.1 契约表生成口径

§6 的表是**机械抽取**的（上游 handler 体内）：请求体取 `Decode(&req)` 的目标类型 / 内联 `struct` 的 `json` 标签；
响应取 `writeJSON|writeMeasuredJSON` 的第二参（类型名或 `map` 的字面键集）；错误码取该 handler 体内**所有**
`writeError(w, status, "msg")` 字面量 + `parseUUIDOrBadRequest` 的 fieldName + 守卫别名。
**行号（`router.go` L####）来自 `docs/15` §1.5/§1.2**，与 `docs/fixtures/upstream-routes.tsv` 同源。
幂等列（§6.3）是唯一**人工**列。

---

## 11. 偏差登记

### 11.1 与「不修改 `lib.rs`」约束的唯一例外

`docs/15` §7.1 的「同文件单写者」把 `lib.rs`/`mod.rs`/`mount.rs`/`Cargo.lock` 列为跨片冲突面。本片修改了
`crates/mc-daemon-proto/src/lib.rs`——它是**脚手架为该 crate 自己预置的声明文件**（17 行纯文档占位），
且 M3 计划明确要求「本片填充它」。修改内容仅两处：①追加 `pub mod` 声明与 `pub use` 门面；
②把两处「本片尚未定义任何类型」的陈述改成实测事实（含 42→29 的更正指针）。
**没有**触碰 `Cargo.toml` / `Cargo.lock` / 任何其它 crate 的 `lib.rs`。

### 11.2 42 → **29** 个 payload 类型

`docs/15` §4 M3-1 沿用「42 个 payload 类型」，实测 `messages.go` 是 **445 行 / 29 个 `struct`**（§2.1 逐条列全）。
差异来源不可考（疑似把 `const` 组与 struct 混计）。本片以**实测 29** 为准，`lib.rs` 与本节同时登记。

### 11.3 类型映射的 4 处有意偏差

| # | Go | Rust | 影响 | 判据 |
| --- | --- | --- | --- | --- |
| D1 | `json.RawMessage` / `*json.RawMessage` | `serde_json::Value`（`Option<Value>`） | **不保留**键序与原始字节；`"x":null` 与缺键都落 `Null`。入站语义一致 | scaffold 未开 `serde_json/raw_value`，本片不许改依赖 |
| D2 | nil slice → `null` | `Vec` 空 → `[]`（`omitempty` 时省略） | **出站**形状可能差一个空数组；入站（`null`/`[]`/缺失）三态等价 | Go 的 nil 与 empty 在 `omitempty` 下不可区分；要精确复刻需再加一层 `Option<Vec<_>>`（无消费者时属过度设计） |
| D3 | `mustMarshalRaw` 失败即 panic | `Result` 返回 | 更安全；调用方必须处理错误 | Rust 不允许在库边界 panic（且 `unsafe_code=forbid` 风格下同理） |
| D4 | `**string` 用「缺键 vs `null`」区分「不提」与「清空」 | `Option<Option<String>>` + `super::double_option::deserialize` | **必须**用自定义解码器：serde 默认会把 `null` 吃在外层，三态塌成两态（`double_option.rs` 文档有完整推导） | 「显式 null = 移出项目」是 `ChatSessionUpdatedPayload.project_id` 的既有语义 |

### 11.4 golden JSON 是**手工推导**的（无 Go 工具链）

本机 `which go` 为空，无法用 Go 生成 golden。`tests/golden.rs` 的 4 组 golden 按三条规则从 Go 结构体标签手推：
① 键名 = `json:"..."` 字面量；② 键序 = 字段声明顺序；③ 键是否出现 = `omitempty` 的零值规则。
该事实写在 `tests/golden.rs` 头部。**风险**：手推可能与真实 Go 编码有出入；`docs/15` §8.5 已把
「M3-1 的 golden 改为读 W0-C 抽取器产物」登记为后续接入点，届时以 fixture 覆盖本文件的 §11.4。

### 11.5 出站键序与 `omitempty` 的实现方式

- Go 用 `encoding/json` 的零值判定；Rust 用 `skip_serializing_if = "..."` 的谓词（`src/messages/omit.rs`）：
  `option` / `string` / `vec_is_empty` / `boolean` / `i32` / `i64` / `map`。
- serde 只接受 `fn(&T) -> bool`，故该模块有两处**必要的** `allow`：`trivially_copy_pass_by_ref`（接口要求）、
  `ptr_arg`（签名由字段类型固定）。已在模块文档说明，避免后续被「优化」掉。
- `serde_json::Value` 内部是 `BTreeMap` ⇒ 经 `Value` 往返的 body 键序会变成字典序（D1 的直接后果）。

---

## 12. 交棒 M3-7（daemon 面 + ws 服务端）的未冻结项

1. ws 传输、连接管理、hub、notifier、metrics —— **不在本片**，但 §3 的帧参数是它们的契约。
2. daemon 的**客户端**侧（`mc-daemon`）—— 本片只冻结线上形状，不含重连/退避/探活策略。
3. `runtime_profile` / `runtime` 的**绑定与探测**（`profile_id` 落库、版本探测）—— 属 M3-2/M3-7。
4. plugin/skill 的**内容**（hook 执行、MCP 凭据的签发语义）—— 属 W6；本片只冻结通道与错误码（§7.1 `G_plug`）。
5. `fail` / `session` 的**空体**语义须在 M3-7 的 e2e 里显式断言（§3.5）。
