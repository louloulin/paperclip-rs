//! `pc-cron` — Paperclip cron 表达式解析与下次触发时间计算
//!
//! 对齐 Node `cron.ts`：
//! - 支持标准 5 字段 cron 表达式（minute / hour / day-of-month / month / day-of-week）
//! - 每个字段支持 `*` / `N` / `N-M` / `N/S` / `*/S` / `N-M/S` / `N,M,...` 语法
//! - 提供 `parse_cron` / `validate_cron` / `next_tick` / `next_tick_from_expression` 四个稳定入口
//! - 纯函数无副作用，方便单测
//!
//! 模块拆分（高内聚低耦合）：
//! - `cron::parse` — 表达式解析（token 切分 + 字段解析 + 边界校验）
//! - `cron::tick` — 下次触发时间计算（按粒度跳跃，含搜索窗口保护）
//! - `cron::tests` — 模块私有规则单测

pub mod cron;

pub use cron::{
    next_tick, next_tick_from_expression, parse_cron, validate_cron, CronError, FieldSpec,
    ParsedCron,
};
