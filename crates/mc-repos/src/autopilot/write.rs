//! autopilot / collaborator / subscriber 的**写**查询。
//!
//! - **写者**：M5-2（**W**；`docs/44` §3.2）。其它切片不得在此加查询。
//! - **上游 SQL**：`db/queries/autopilot.sql` 的写侧查询。
//! - **要落的写点**：`autopilot` 的 create/update/delete、`writeAutopilotCollaborators`、
//!   `autopilot_collaborator` / `autopilot_subscriber` 的增删、`autopilot_rule_version` 的 append。
//! - **事务边界**：`lockAndValidateAutopilotSubscribers`39 要求**同事务**内加锁校验
//!   （上游 `FOR SHARE` / `FOR UPDATE`）⇒ 需要接受调用方传入的 `&mut Transaction`，
//!   不要自己开事务（否则与路由层的校验竞态）。
//! - **append-only**：`autopilot_rule_version`（`186`）只允许 INSERT，**没有** UPDATE/DELETE 查询。
