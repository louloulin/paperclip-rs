//! daemon 协议事件常量 —— 上游 `server/pkg/protocol/events.go` **逐条冻结**。
//!
//! **冻结来源**：`multica` @ `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`，
//! `server/pkg/protocol/events.go`（**109 条**事件常量，L6–L217）。
//!
//! **命名规则**：Rust 常量名由**线上字符串**机械推导 —— 大写，并把所有非字母数字字符
//! 折成 `_`（`"issue_metadata:changed"` → `ISSUE_METADATA_CHANGED`）。不抄 Go 标识符
//! （`EventIssueMetadataChanged`）是为了让「常量名 ↔ 线上字符串」可机械复算，也避免
//! 驼峰切词的歧义（`VCSConnectionCreated` / `GitHubInstallationCreated` 这类）。
//! 与 Go 标识符的**逐条对照表**（含 events.go 行号）在
//! `docs/16-M3-DAEMON-PROTOCOL.md` §6，那是权威表；本文件每条常量也在文档注释里带了
//! 上游标识符与行号。复算命令见 `docs/16` §12。
//!
//! **未知 event 必须忽略**，这是上游的显式契约（`hub.go` `handleFrame` 的 `default`
//! 分支：`// Unknown app messages are intentionally ignored for forward compatibility`）。
//! 因此本模块只提供**谓词** `is_known_event`，不提供会把未知值判为解析错误的枚举 ——
//! 一个 `enum` 会让新事件的到达变成错误而不是忽略，与上游行为相反。
//!
//! **新增协议事件是增量的**：加常量、进 `KNOWN_EVENTS`，不改任何已有常量的字符串；
//! 改名 / 改字符串属于破坏性变更（走 `docs/16` §9 的冻结变更流程）。
//!
//! 上游 L34–L42 的 `task:*` 状态迁移注释（∅→queued→dispatched→running→…→cancelled）
//! 是 M3-3 状态机的**真值**，本文件逐条保留在对应常量的文档注释里。

/// `issue:created` — 上游 `EventIssueCreated`（events.go:6）。
pub const ISSUE_CREATED: &str = "issue:created";

/// `issue:updated` — 上游 `EventIssueUpdated`（events.go:7）。
pub const ISSUE_UPDATED: &str = "issue:updated";

/// `issue:deleted` — 上游 `EventIssueDeleted`（events.go:8）。
pub const ISSUE_DELETED: &str = "issue:deleted";

/// `issue_metadata:changed` — 上游 `EventIssueMetadataChanged`（events.go:9）。
pub const ISSUE_METADATA_CHANGED: &str = "issue_metadata:changed";

/// `issue_attachments:changed` — 上游 `EventIssueAttachmentsChanged`（events.go:10）。
pub const ISSUE_ATTACHMENTS_CHANGED: &str = "issue_attachments:changed";

/// `comment:created` — 上游 `EventCommentCreated`（events.go:13）。
pub const COMMENT_CREATED: &str = "comment:created";

/// `comment:updated` — 上游 `EventCommentUpdated`（events.go:14）。
pub const COMMENT_UPDATED: &str = "comment:updated";

/// `comment:deleted` — 上游 `EventCommentDeleted`（events.go:15）。
pub const COMMENT_DELETED: &str = "comment:deleted";

/// `comment:resolved` — 上游 `EventCommentResolved`（events.go:16）。
pub const COMMENT_RESOLVED: &str = "comment:resolved";

/// `comment:unresolved` — 上游 `EventCommentUnresolved`（events.go:17）。
pub const COMMENT_UNRESOLVED: &str = "comment:unresolved";

/// `reaction:added` — 上游 `EventReactionAdded`（events.go:18）。
pub const REACTION_ADDED: &str = "reaction:added";

/// `reaction:removed` — 上游 `EventReactionRemoved`（events.go:19）。
pub const REACTION_REMOVED: &str = "reaction:removed";

/// `issue_reaction:added` — 上游 `EventIssueReactionAdded`（events.go:20）。
pub const ISSUE_REACTION_ADDED: &str = "issue_reaction:added";

/// `issue_reaction:removed` — 上游 `EventIssueReactionRemoved`（events.go:21）。
pub const ISSUE_REACTION_REMOVED: &str = "issue_reaction:removed";

/// `agent:status` — 上游 `EventAgentStatus`（events.go:24）。
pub const AGENT_STATUS: &str = "agent:status";

/// `agent:created` — 上游 `EventAgentCreated`（events.go:25）。
pub const AGENT_CREATED: &str = "agent:created";

/// `agent:archived` — 上游 `EventAgentArchived`（events.go:26）。
pub const AGENT_ARCHIVED: &str = "agent:archived";

/// `agent:restored` — 上游 `EventAgentRestored`（events.go:27）。
pub const AGENT_RESTORED: &str = "agent:restored";

/// `task:queued` — 上游 `EventTaskQueued`（events.go:34）。
///
/// 上游行内注释：`∅ → queued (enqueue / retry create)`
pub const TASK_QUEUED: &str = "task:queued";

/// `task:dispatch` — 上游 `EventTaskDispatch`（events.go:35）。
///
/// 上游行内注释：`queued → dispatched (daemon claim)`
pub const TASK_DISPATCH: &str = "task:dispatch";

/// `task:running` — 上游 `EventTaskRunning`（events.go:36）。
///
/// 上游行内注释：`dispatched → running (daemon started)`
pub const TASK_RUNNING: &str = "task:running";

/// `task:waiting_local_directory` — 上游 `EventTaskWaitingLocalDirectory`（events.go:37）。
///
/// 上游行内注释：`dispatched → waiting_local_directory (daemon parked on a busy local_directory path)`
pub const TASK_WAITING_LOCAL_DIRECTORY: &str = "task:waiting_local_directory";

/// `task:progress` — 上游 `EventTaskProgress`（events.go:38）。
pub const TASK_PROGRESS: &str = "task:progress";

/// `task:completed` — 上游 `EventTaskCompleted`（events.go:39）。
///
/// 上游行内注释：`running → completed`
pub const TASK_COMPLETED: &str = "task:completed";

/// `task:failed` — 上游 `EventTaskFailed`（events.go:40）。
///
/// 上游行内注释：`running → failed`
pub const TASK_FAILED: &str = "task:failed";

/// `task:message` — 上游 `EventTaskMessage`（events.go:41）。
pub const TASK_MESSAGE: &str = "task:message";

/// `task:cancelled` — 上游 `EventTaskCancelled`（events.go:42）。
///
/// 上游行内注释：`* → cancelled`
pub const TASK_CANCELLED: &str = "task:cancelled";

/// `inbox:new` — 上游 `EventInboxNew`（events.go:45）。
pub const INBOX_NEW: &str = "inbox:new";

/// `inbox:read` — 上游 `EventInboxRead`（events.go:46）。
pub const INBOX_READ: &str = "inbox:read";

/// `inbox:unread` — 上游 `EventInboxUnread`（events.go:47）。
pub const INBOX_UNREAD: &str = "inbox:unread";

/// `inbox:archived` — 上游 `EventInboxArchived`（events.go:48）。
pub const INBOX_ARCHIVED: &str = "inbox:archived";

/// `inbox:unarchived` — 上游 `EventInboxUnarchived`（events.go:49）。
pub const INBOX_UNARCHIVED: &str = "inbox:unarchived";

/// `inbox:batch-read` — 上游 `EventInboxBatchRead`（events.go:50）。
pub const INBOX_BATCH_READ: &str = "inbox:batch-read";

/// `inbox:batch-archived` — 上游 `EventInboxBatchArchived`（events.go:51）。
pub const INBOX_BATCH_ARCHIVED: &str = "inbox:batch-archived";

/// `workspace:updated` — 上游 `EventWorkspaceUpdated`（events.go:54）。
pub const WORKSPACE_UPDATED: &str = "workspace:updated";

/// `workspace:deleted` — 上游 `EventWorkspaceDeleted`（events.go:55）。
pub const WORKSPACE_DELETED: &str = "workspace:deleted";

/// `member:added` — 上游 `EventMemberAdded`（events.go:58）。
pub const MEMBER_ADDED: &str = "member:added";

/// `member:updated` — 上游 `EventMemberUpdated`（events.go:59）。
pub const MEMBER_UPDATED: &str = "member:updated";

/// `member:removed` — 上游 `EventMemberRemoved`（events.go:60）。
pub const MEMBER_REMOVED: &str = "member:removed";

/// `subscriber:added` — 上游 `EventSubscriberAdded`（events.go:63）。
pub const SUBSCRIBER_ADDED: &str = "subscriber:added";

/// `subscriber:removed` — 上游 `EventSubscriberRemoved`（events.go:64）。
pub const SUBSCRIBER_REMOVED: &str = "subscriber:removed";

/// `activity:created` — 上游 `EventActivityCreated`（events.go:67）。
pub const ACTIVITY_CREATED: &str = "activity:created";

/// `skill:created` — 上游 `EventSkillCreated`（events.go:70）。
pub const SKILL_CREATED: &str = "skill:created";

/// `skill:updated` — 上游 `EventSkillUpdated`（events.go:71）。
pub const SKILL_UPDATED: &str = "skill:updated";

/// `skill:deleted` — 上游 `EventSkillDeleted`（events.go:72）。
pub const SKILL_DELETED: &str = "skill:deleted";

/// `chat:message` — 上游 `EventChatMessage`（events.go:75）。
pub const CHAT_MESSAGE: &str = "chat:message";

/// `chat:done` — 上游 `EventChatDone`（events.go:76）。
pub const CHAT_DONE: &str = "chat:done";

/// `chat:quick_actions` — 上游 `EventChatQuickActions`（events.go:81）。
pub const CHAT_QUICK_ACTIONS: &str = "chat:quick_actions";

/// `chat:cancel_finalized` — 上游 `EventChatCancelFinalized`（events.go:87）。
pub const CHAT_CANCEL_FINALIZED: &str = "chat:cancel_finalized";

/// `chat:session_created` — 上游 `EventChatSessionCreated`（events.go:88）。
pub const CHAT_SESSION_CREATED: &str = "chat:session_created";

/// `chat:session_read` — 上游 `EventChatSessionRead`（events.go:89）。
pub const CHAT_SESSION_READ: &str = "chat:session_read";

/// `chat:session_deleted` — 上游 `EventChatSessionDeleted`（events.go:90）。
pub const CHAT_SESSION_DELETED: &str = "chat:session_deleted";

/// `chat:session_updated` — 上游 `EventChatSessionUpdated`（events.go:91）。
pub const CHAT_SESSION_UPDATED: &str = "chat:session_updated";

/// `project:created` — 上游 `EventProjectCreated`（events.go:94）。
pub const PROJECT_CREATED: &str = "project:created";

/// `project:updated` — 上游 `EventProjectUpdated`（events.go:95）。
pub const PROJECT_UPDATED: &str = "project:updated";

/// `project:deleted` — 上游 `EventProjectDeleted`（events.go:96）。
pub const PROJECT_DELETED: &str = "project:deleted";

/// `project_resource:created` — 上游 `EventProjectResourceCreated`（events.go:97）。
pub const PROJECT_RESOURCE_CREATED: &str = "project_resource:created";

/// `project_resource:updated` — 上游 `EventProjectResourceUpdated`（events.go:98）。
pub const PROJECT_RESOURCE_UPDATED: &str = "project_resource:updated";

/// `project_resource:deleted` — 上游 `EventProjectResourceDeleted`（events.go:99）。
pub const PROJECT_RESOURCE_DELETED: &str = "project_resource:deleted";

/// `label:created` — 上游 `EventLabelCreated`（events.go:102）。
pub const LABEL_CREATED: &str = "label:created";

/// `label:updated` — 上游 `EventLabelUpdated`（events.go:103）。
pub const LABEL_UPDATED: &str = "label:updated";

/// `label:deleted` — 上游 `EventLabelDeleted`（events.go:104）。
pub const LABEL_DELETED: &str = "label:deleted";

/// `issue_labels:changed` — 上游 `EventIssueLabelsChanged`（events.go:105）。
pub const ISSUE_LABELS_CHANGED: &str = "issue_labels:changed";

/// `property:created` — 上游 `EventPropertyCreated`（events.go:109）。
pub const PROPERTY_CREATED: &str = "property:created";

/// `property:updated` — 上游 `EventPropertyUpdated`（events.go:110）。
pub const PROPERTY_UPDATED: &str = "property:updated";

/// `issue_properties:changed` — 上游 `EventIssuePropertiesChanged`（events.go:111）。
pub const ISSUE_PROPERTIES_CHANGED: &str = "issue_properties:changed";

/// `issue_status:changed` — 上游 `EventIssueStatusChanged`（events.go:120）。
pub const ISSUE_STATUS_CHANGED: &str = "issue_status:changed";

/// `pin:created` — 上游 `EventPinCreated`（events.go:123）。
pub const PIN_CREATED: &str = "pin:created";

/// `pin:deleted` — 上游 `EventPinDeleted`（events.go:124）。
pub const PIN_DELETED: &str = "pin:deleted";

/// `pin:reordered` — 上游 `EventPinReordered`（events.go:125）。
pub const PIN_REORDERED: &str = "pin:reordered";

/// `invitation:created` — 上游 `EventInvitationCreated`（events.go:128）。
pub const INVITATION_CREATED: &str = "invitation:created";

/// `invitation:accepted` — 上游 `EventInvitationAccepted`（events.go:129）。
pub const INVITATION_ACCEPTED: &str = "invitation:accepted";

/// `invitation:declined` — 上游 `EventInvitationDeclined`（events.go:130）。
pub const INVITATION_DECLINED: &str = "invitation:declined";

/// `invitation:revoked` — 上游 `EventInvitationRevoked`（events.go:131）。
pub const INVITATION_REVOKED: &str = "invitation:revoked";

/// `autopilot:created` — 上游 `EventAutopilotCreated`（events.go:134）。
pub const AUTOPILOT_CREATED: &str = "autopilot:created";

/// `autopilot:updated` — 上游 `EventAutopilotUpdated`（events.go:135）。
pub const AUTOPILOT_UPDATED: &str = "autopilot:updated";

/// `autopilot:deleted` — 上游 `EventAutopilotDeleted`（events.go:136）。
pub const AUTOPILOT_DELETED: &str = "autopilot:deleted";

/// `autopilot:run_start` — 上游 `EventAutopilotRunStart`（events.go:137）。
pub const AUTOPILOT_RUN_START: &str = "autopilot:run_start";

/// `autopilot:run_done` — 上游 `EventAutopilotRunDone`（events.go:138）。
pub const AUTOPILOT_RUN_DONE: &str = "autopilot:run_done";

/// `squad:created` — 上游 `EventSquadCreated`（events.go:141）。
pub const SQUAD_CREATED: &str = "squad:created";

/// `squad:updated` — 上游 `EventSquadUpdated`（events.go:142）。
pub const SQUAD_UPDATED: &str = "squad:updated";

/// `squad:deleted` — 上游 `EventSquadDeleted`（events.go:143）。
pub const SQUAD_DELETED: &str = "squad:deleted";

/// `daemon:heartbeat` — 上游 `EventDaemonHeartbeat`（events.go:146）。
pub const DAEMON_HEARTBEAT: &str = "daemon:heartbeat";

/// `daemon:heartbeat_ack` — 上游 `EventDaemonHeartbeatAck`（events.go:147）。
pub const DAEMON_HEARTBEAT_ACK: &str = "daemon:heartbeat_ack";

/// `daemon:register` — 上游 `EventDaemonRegister`（events.go:148）。
pub const DAEMON_REGISTER: &str = "daemon:register";

/// `daemon:task_available` — 上游 `EventDaemonTaskAvailable`（events.go:149）。
pub const DAEMON_TASK_AVAILABLE: &str = "daemon:task_available";

/// `daemon:runtime_profiles_changed` — 上游 `EventDaemonRuntimeProfilesChanged`（events.go:150）。
pub const DAEMON_RUNTIME_PROFILES_CHANGED: &str = "daemon:runtime_profiles_changed";

/// `daemon:workspaces_changed` — 上游 `EventDaemonWorkspacesChanged`（events.go:151）。
pub const DAEMON_WORKSPACES_CHANGED: &str = "daemon:workspaces_changed";

/// `daemon:pending_work` — 上游 `EventDaemonPendingWork`（events.go:160）。
pub const DAEMON_PENDING_WORK: &str = "daemon:pending_work";

/// `daemon:rpc_request` — 上游 `EventDaemonRPCRequest`（events.go:166）。
pub const DAEMON_RPC_REQUEST: &str = "daemon:rpc_request";

/// `daemon:rpc_response` — 上游 `EventDaemonRPCResponse`（events.go:167）。
pub const DAEMON_RPC_RESPONSE: &str = "daemon:rpc_response";

/// `github_installation:created` — 上游 `EventGitHubInstallationCreated`（events.go:170）。
pub const GITHUB_INSTALLATION_CREATED: &str = "github_installation:created";

/// `github_installation:deleted` — 上游 `EventGitHubInstallationDeleted`（events.go:171）。
pub const GITHUB_INSTALLATION_DELETED: &str = "github_installation:deleted";

/// `pull_request:linked` — 上游 `EventPullRequestLinked`（events.go:172）。
pub const PULL_REQUEST_LINKED: &str = "pull_request:linked";

/// `pull_request:updated` — 上游 `EventPullRequestUpdated`（events.go:173）。
pub const PULL_REQUEST_UPDATED: &str = "pull_request:updated";

/// `pull_request:unlinked` — 上游 `EventPullRequestUnlinked`（events.go:174）。
pub const PULL_REQUEST_UNLINKED: &str = "pull_request:unlinked";

/// `vcs_connection:created` — 上游 `EventVCSConnectionCreated`（events.go:177）。
pub const VCS_CONNECTION_CREATED: &str = "vcs_connection:created";

/// `vcs_connection:deleted` — 上游 `EventVCSConnectionDeleted`（events.go:178）。
pub const VCS_CONNECTION_DELETED: &str = "vcs_connection:deleted";

/// `lark_installation:created` — 上游 `EventLarkInstallationCreated`（events.go:186）。
pub const LARK_INSTALLATION_CREATED: &str = "lark_installation:created";

/// `lark_installation:revoked` — 上游 `EventLarkInstallationRevoked`（events.go:187）。
pub const LARK_INSTALLATION_REVOKED: &str = "lark_installation:revoked";

/// `slack_installation:created` — 上游 `EventSlackInstallationCreated`（events.go:194）。
pub const SLACK_INSTALLATION_CREATED: &str = "slack_installation:created";

/// `slack_installation:revoked` — 上游 `EventSlackInstallationRevoked`（events.go:195）。
pub const SLACK_INSTALLATION_REVOKED: &str = "slack_installation:revoked";

/// `dingtalk_installation:created` — 上游 `EventDingTalkInstallationCreated`（events.go:201）。
pub const DINGTALK_INSTALLATION_CREATED: &str = "dingtalk_installation:created";

/// `dingtalk_installation:revoked` — 上游 `EventDingTalkInstallationRevoked`（events.go:202）。
pub const DINGTALK_INSTALLATION_REVOKED: &str = "dingtalk_installation:revoked";

/// `dingtalk_installation:binding_updated` — 上游 `EventDingTalkAccountBindingUpdated`（events.go:203）。
pub const DINGTALK_INSTALLATION_BINDING_UPDATED: &str = "dingtalk_installation:binding_updated";

/// `wecom_installation:created` — 上游 `EventWecomInstallationCreated`（events.go:211）。
pub const WECOM_INSTALLATION_CREATED: &str = "wecom_installation:created";

/// `wecom_installation:revoked` — 上游 `EventWecomInstallationRevoked`（events.go:212）。
pub const WECOM_INSTALLATION_REVOKED: &str = "wecom_installation:revoked";

/// `telegram_installation:created` — 上游 `EventTelegramInstallationCreated`（events.go:216）。
pub const TELEGRAM_INSTALLATION_CREATED: &str = "telegram_installation:created";

/// `telegram_installation:revoked` — 上游 `EventTelegramInstallationRevoked`（events.go:217）。
pub const TELEGRAM_INSTALLATION_REVOKED: &str = "telegram_installation:revoked";

/// 上游 `events.go` 全部事件字符串，顺序与上游 const 块一致。
///
/// 这个数组是「已知事件」的唯一真值面：`is_known_event` 只查它。
pub const KNOWN_EVENTS: [&str; 109] = [
    ISSUE_CREATED,
    ISSUE_UPDATED,
    ISSUE_DELETED,
    ISSUE_METADATA_CHANGED,
    ISSUE_ATTACHMENTS_CHANGED,
    COMMENT_CREATED,
    COMMENT_UPDATED,
    COMMENT_DELETED,
    COMMENT_RESOLVED,
    COMMENT_UNRESOLVED,
    REACTION_ADDED,
    REACTION_REMOVED,
    ISSUE_REACTION_ADDED,
    ISSUE_REACTION_REMOVED,
    AGENT_STATUS,
    AGENT_CREATED,
    AGENT_ARCHIVED,
    AGENT_RESTORED,
    TASK_QUEUED,
    TASK_DISPATCH,
    TASK_RUNNING,
    TASK_WAITING_LOCAL_DIRECTORY,
    TASK_PROGRESS,
    TASK_COMPLETED,
    TASK_FAILED,
    TASK_MESSAGE,
    TASK_CANCELLED,
    INBOX_NEW,
    INBOX_READ,
    INBOX_UNREAD,
    INBOX_ARCHIVED,
    INBOX_UNARCHIVED,
    INBOX_BATCH_READ,
    INBOX_BATCH_ARCHIVED,
    WORKSPACE_UPDATED,
    WORKSPACE_DELETED,
    MEMBER_ADDED,
    MEMBER_UPDATED,
    MEMBER_REMOVED,
    SUBSCRIBER_ADDED,
    SUBSCRIBER_REMOVED,
    ACTIVITY_CREATED,
    SKILL_CREATED,
    SKILL_UPDATED,
    SKILL_DELETED,
    CHAT_MESSAGE,
    CHAT_DONE,
    CHAT_QUICK_ACTIONS,
    CHAT_CANCEL_FINALIZED,
    CHAT_SESSION_CREATED,
    CHAT_SESSION_READ,
    CHAT_SESSION_DELETED,
    CHAT_SESSION_UPDATED,
    PROJECT_CREATED,
    PROJECT_UPDATED,
    PROJECT_DELETED,
    PROJECT_RESOURCE_CREATED,
    PROJECT_RESOURCE_UPDATED,
    PROJECT_RESOURCE_DELETED,
    LABEL_CREATED,
    LABEL_UPDATED,
    LABEL_DELETED,
    ISSUE_LABELS_CHANGED,
    PROPERTY_CREATED,
    PROPERTY_UPDATED,
    ISSUE_PROPERTIES_CHANGED,
    ISSUE_STATUS_CHANGED,
    PIN_CREATED,
    PIN_DELETED,
    PIN_REORDERED,
    INVITATION_CREATED,
    INVITATION_ACCEPTED,
    INVITATION_DECLINED,
    INVITATION_REVOKED,
    AUTOPILOT_CREATED,
    AUTOPILOT_UPDATED,
    AUTOPILOT_DELETED,
    AUTOPILOT_RUN_START,
    AUTOPILOT_RUN_DONE,
    SQUAD_CREATED,
    SQUAD_UPDATED,
    SQUAD_DELETED,
    DAEMON_HEARTBEAT,
    DAEMON_HEARTBEAT_ACK,
    DAEMON_REGISTER,
    DAEMON_TASK_AVAILABLE,
    DAEMON_RUNTIME_PROFILES_CHANGED,
    DAEMON_WORKSPACES_CHANGED,
    DAEMON_PENDING_WORK,
    DAEMON_RPC_REQUEST,
    DAEMON_RPC_RESPONSE,
    GITHUB_INSTALLATION_CREATED,
    GITHUB_INSTALLATION_DELETED,
    PULL_REQUEST_LINKED,
    PULL_REQUEST_UPDATED,
    PULL_REQUEST_UNLINKED,
    VCS_CONNECTION_CREATED,
    VCS_CONNECTION_DELETED,
    LARK_INSTALLATION_CREATED,
    LARK_INSTALLATION_REVOKED,
    SLACK_INSTALLATION_CREATED,
    SLACK_INSTALLATION_REVOKED,
    DINGTALK_INSTALLATION_CREATED,
    DINGTALK_INSTALLATION_REVOKED,
    DINGTALK_INSTALLATION_BINDING_UPDATED,
    WECOM_INSTALLATION_CREATED,
    WECOM_INSTALLATION_REVOKED,
    TELEGRAM_INSTALLATION_CREATED,
    TELEGRAM_INSTALLATION_REVOKED,
];

/// 上游事件常量条数（`events.go` 实测）。
pub const KNOWN_EVENT_COUNT: usize = 109;

/// 未知 event 判定：只认 `KNOWN_EVENTS` 里的字符串，其余一律忽略（**不报错**）。
///
/// 与上游 `switch msg.Type` + `default:` 忽略的行为等价。
#[must_use]
pub fn is_known_event(event_type: &str) -> bool {
    KNOWN_EVENTS.contains(&event_type)
}
