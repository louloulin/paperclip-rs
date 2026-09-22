//! daemon 协议载荷类型 —— 上游 `server/pkg/protocol/messages.go` 的 **29 个结构体**
//! 逐字冻结。
//!
//! # 上游 → Rust 的映射
//!
//! | 上游 `messages.go` 行段 | 本模块文件 | 内容 |
//! |-------------------------|------------|------|
//! | L87–L116 | [`envelope`] | 帧信封 `Message` + RPC 请求/响应信封 |
//! | L75–L81、L186–L200、L235–L382 | [`chat`] | 会话面 14 个类型 + kind/outcome 常量 |
//! | L118–L135、L168–L184、L201–L219 | [`task`] | 任务面 5 个类型 |
//! | L136–L167、L221–L232、L384–L445 | [`daemon`] | 注册/心跳/提示 11 个类型 |
//! | L3–L73 | [`crate::capabilities`] | 能力常量与协商 |
//!
//! 上游 `messages.go` 共 **29** 个 `type ... struct`（`grep -c '^type .* struct'`），
//! 本 crate 一个不多一个不少地覆盖它们；立项文档里写的「42 个 payload 类型」是估算，
//! 实测漂移已登记在 `docs/16-M3-DAEMON-PROTOCOL.md` §11。
//!
//! # 三条贯穿全模块的解码约定
//!
//! 1. **未知字段容忍**：所有结构体都是 `#[serde(default)]`，既忽略未知键，也把缺失字段
//!    填成 `Default`（等价于 Go 解码时落零值）。上游从不因为字段缺失/多余而报错，
//!    新增字段因此天然向后兼容。
//! 2. **`omitempty` 双向对齐**：出站用 [`omit`] 里的谓词复刻 Go 的「零值省略」，
//!    入站不需要 —— 缺失即默认值。
//! 3. **三态字段不塌缩**：`*bool` / `*string` / `**string` 一律保留三层含义
//!    （缺失 / null / 有值），见 [`chat::ChatSessionUpdatedPayload::project_id`] 与
//!    [`task::TaskMessagePayload::output_truncated`]。
//!
//! # 本模块**不**做的事
//!
//! 不解码分派（未知 `type` 的忽略判定在 [`crate::events::is_known_event`]）、不做连接
//! 管理、不落库、不认识任何 HTTP 路由 —— 那些分别属于 M3-7 的 `mc-daemon` 与 W3b/W3c
//! 的 handler 切片。本 crate 只回答「线上字节长什么样」。

pub mod chat;
pub mod daemon;
pub mod double_option;
pub mod envelope;
pub mod omit;
pub mod task;

pub use chat::{
    ChatCancelFinalizedPayload, ChatDonePayload, ChatMessagePayload, ChatQuickAction,
    ChatQuickActionsPayload, ChatSessionChannelSource, ChatSessionCreatedPayload,
    ChatSessionDeletedPayload, ChatSessionReadPayload, ChatSessionUpdatedPayload,
};
pub use daemon::{
    DaemonHeartbeatAckPayload, DaemonHeartbeatPendingLocalSkillImport,
    DaemonHeartbeatPendingLocalSkills, DaemonHeartbeatPendingModelList,
    DaemonHeartbeatPendingUpdate, DaemonHeartbeatRequestPayload, DaemonRegisterPayload,
    PendingWorkPayload, RuntimeInfo, RuntimeProfilesChangedPayload, WorkspacesChangedPayload,
    HEARTBEAT_STATUS_RUNTIME_GONE,
};
pub use envelope::{Message, RPCRequestPayload, RPCResponsePayload};
pub use task::{
    TaskAvailablePayload, TaskCompletedPayload, TaskDispatchPayload, TaskMessagePayload,
    TaskProgressPayload,
};
