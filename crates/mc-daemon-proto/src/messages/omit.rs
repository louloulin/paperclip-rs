//! `omitempty` 谓词 —— 让 serde 的出站序列化与上游 Go 的 `json:"x,omitempty"` 对齐。
//!
//! Go 的 `omitempty` 对**非指针**标量也有意义：`false` / `0` / `""` / 空切片 / 空 map
//! 都会被省略。serde 只内建了 `Option::is_none`，其余要靠 `skip_serializing_if` 指到
//! 一个 `fn(&T) -> bool`，所以这里放那批谓词，供各 payload 结构体引用。
//!
//! 入站方向不依赖本模块：所有 payload 都是 `#[serde(default)]`，缺失字段落 `Default`
//! （与 Go 解码缺失字段落零值一致）。
//!
//! `#[allow(clippy::trivially_copy_pass_by_ref)]`：serde 的 `skip_serializing_if` 只接受
//! `fn(&T) -> bool`，因此按引用取小整数/bool 是接口要求，不是笔误。
#![allow(clippy::trivially_copy_pass_by_ref)]

/// `Option<T>` 是否为空（Go 的 `omitempty` 对指针/接口的语义）。
#[must_use]
pub const fn option<T>(value: &Option<T>) -> bool {
    value.is_none()
}

/// 字符串是否为空。
///
/// `#[allow(clippy::ptr_arg)]`：签名必须是 `fn(&String) -> bool`（字段类型就是
/// `String`），不能收窄成 `&str`，否则 serde 生成代码无法调用。
#[must_use]
#[allow(clippy::ptr_arg)]
pub fn string(value: &String) -> bool {
    value.is_empty()
}

/// 切片是否为空（Go `omitempty` 对 nil 与长度 0 的切片都不输出）。
///
/// `#[allow(clippy::ptr_arg)]` 同 [`string`]：签名受 serde 约束。
#[must_use]
#[allow(clippy::ptr_arg)]
pub fn vec_is_empty<T>(value: &Vec<T>) -> bool {
    value.is_empty()
}

/// `bool` 是否为 `false`（Go `omitempty` 会省略 false）。
#[must_use]
pub const fn boolean(value: &bool) -> bool {
    !*value
}

/// `i32` 是否为 0。
#[must_use]
pub const fn i32(value: &i32) -> bool {
    *value == 0
}

/// `i64` 是否为 0。
#[must_use]
pub const fn i64(value: &i64) -> bool {
    *value == 0
}

/// `serde_json::Map` 是否为空。
#[must_use]
pub fn map(value: &serde_json::Map<String, serde_json::Value>) -> bool {
    value.is_empty()
}
