//! 三态可选字段的解码助手：`Option<Option<T>>` 的 `null` vs 缺失。
//!
//! # 为什么需要它
//!
//! serde 对 `Option<T>` 的实现是这样调 `Deserializer::deserialize_option` 的：
//!
//! ```text
//! Option<Option<String>>  ←  "key": null
//!   外层 visitor.visit_none() → 直接返回 None
//! ```
//!
//! 也就是说**外层** `Option` 会吃掉落 JSON `null`，内层永远见不到它，于是
//! `Option<Option<T>>` 退化成 `Option<T>`：「显式 null」与「键缺失」不可区分。
//! 而上游的 `**string`（`ChatSessionUpdatedPayload.project_id`）恰恰靠这个区分：
//! 缺失 = 没提这件事（保持原值），null = 显式清空（移出项目）。
//!
//! [`deserialize`] 无条件按**内层** `Option<T>` 读一次，再包一层 `Some`，把三态保住：
//!
//! | 线上 | 结果 |
//! |------|------|
//! | 键缺失 | 容器 `#[serde(default)]` → `None` |
//! | `null` | `Some(None)` |
//! | 有值 | `Some(Some(value))` |
//!
//! 出站方向不需要助手：`Option<Option<T>>` 默认序列化就是「`None` 省略键 / `Some(None)`
//! 出 `null` / `Some(Some(v))` 出值」，配合 `skip_serializing_if = "omit::option"` 即与
//! Go 的 `**string` + `omitempty` 逐字一致。

use serde::{Deserialize, Deserializer};

/// 把「缺失 vs `null` vs 有值」三态读进 `Option<Option<T>>`。
///
/// # Errors
///
/// 内层 `T` 解析失败时返回 `D::Error`（与普通 `Option<T>` 字段一致）。
pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
