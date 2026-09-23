//! `plugin_storage` 的读写（公开 API `/v1/*/storage` 与 bridge 的 storage 面**共用**）。
//!
//! - **写者**：M6-7（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_surface.go` 的 storage 段（`/v1` 与 `/api/plugin-bridge/v1`
//!   是同一组 handler 挂两处 —— `docs/57` §9.2 的「17 + bridge 20」就是这个意思）。
//! - **8 列**：`id, installation_id, scope_type, scope_id, key, value, created_at, updated_at`；
//!   `scope_type CHECK IN ('workspace','user')`；`key` ≤1024、`value` ≤102400（`octet_length`）。
//! - **三条硬语义**：
//!   1. `scope_id` 的取值取决于 `scope_type`：`workspace` ⇒ 工作区 id，`user` ⇒ **用户 id**
//!      （不是安装 id）—— 写错会出现「A 用户读到 B 用户的键」；
//!   2. **软配额**（1000 键 / 5 MiB/安装）**没有淘汰**：超了要返回明确错误，不能 LRU 掉别人的数据；
//!   3. `value` 是不透明的 JSONB（宿主不理解内容，只做大小与配额校验）。
//! - **本仓约定**：`(installation_id, scope_type, scope_id, key)` 是逻辑主键（表上没有唯一约束，
//!   靠 `ON CONFLICT` 不可用 ⇒ 走「先 UPDATE，影响 0 行再 INSERT」或 `MERGE` 风格；并发下要
//!   有重试），**别**假设有唯一索引。
//! - **不做什么**：不做加密（`plugin_secret` 才是密文面，本文件不碰它）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 240 行以内。
