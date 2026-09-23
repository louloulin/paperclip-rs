//! 5 字段 cron 解析器 —— 上游 `service/cron.go`(138) 的移植（M5-1）。
//!
//! 归属：`docs/44-M5-PLAN.md` §4.2 把 `service/cron.go`(138) 判给 **M5-1**（「cron 基座」，
//! 与 `cron-preview` 路由同片），并且 §6.2 把「5 字段语义 + 无下次触发」列为本片**专属测试**。
//! 上游 `cron.NewParser(Minute|Hour|Dom|Month|Dow)` 是**5 字段、无秒**的标准 cron。
//!
//! # 落地位置（与 `src/lib.rs` 锚点文档的一处偏离，必须记）
//!
//! `src/lib.rs` 的 M5-0 anchor 原文写「`service/cron.go` 的移植落在 `src/trigger.rs`（M5-3）」，
//! 但 §3.2 的写集矩阵把 `mc-autopilot/src/{trigger,credential}.rs` 独占给 **M5-3**（一格一写者），
//! 而 §4.2/§6.2 又把 cron 解析与它的专属测试判给 **M5-1** —— 两者不可同时成立。
//! 本片的解法：把解析器落在**新文件 `src/cron.rs`**（§3.2 矩阵里没有这一行，不与任何切片抢文件），
//! 由 `src/lib.rs`（同样是 M5-1 的写者）声明为 `pub mod cron`，并在 `docs/46` 记录偏离。
//! ⇒ **M5-3 的 `trigger.rs` 与 M5-7 的 `plan_time` 都调用 `mc_autopilot::cron`**，
//! 不要再各写一份（`crates/mc-scheduler/src/jobs/autopilot.rs` 的注释指向 `trigger` 是 anchor 的旧话，
//! M5-8 落地时以本文件为准）。
//!
//! # 为什么手写（`docs/44` §5.4 第 1 项，anchor 实测）
//!
//! `cron` crate 0.12.1 被实测否掉：① 拒 5 字段；② 星期编号 `1=SUN..7=SAT`（robfig 是 `0=SUN..6=SAT`）；
//! ③ `dom` 与 `dow` 同时受限时是 AND 而不是 Vixie 的 OR；④ 没有 `TZ=` 前缀；⑤ `0 0 0 30 2 *` 的
//! 空迭代行为虽然一致，但前四条已足以否掉它。
//!
//! # 逐条对齐的上游语义（每一条都有对应用例）
//!
//! | 语义 | 上游（robfig v3 `Minute|Hour|Dom|Month|Dow`） | 本地 |
//! | --- | --- | --- |
//! | 字段数 | 必须 5 | [`CronError::FieldCount`] |
//! | 星期编号 | `0=Sun … 6=Sat` | [`dow_number`]（`chrono` 是 `Mon=1..Sun=0`） |
//! | `dom`/`dow` 同时受限 | **OR**（Vixie）；一方为 `*` 时是 AND | [`CronSpec::day_matches`] |
//! | `*` 的星号位 | 只有基数是 `*`/`?` **且** `step <= 1` 才算 `*`（`*/2` 算受限） | [`Field::star`] |
//! | `N/step` | 等价 `N-max/step` | [`Field::parse`] |
//! | `?` | 等价 `*` | [`Field::parse`] |
//! | 月份/星期名 | 大小写不敏感三字母（`jan`…/`sun`…） | [`Field::parse`] |
//! | `TZ=`/`CRON_TZ=` 前缀 | 前缀时区**覆盖**调用方传入的时区；无空格时上游**panic** | [`split_timezone_prefix`] |
//! | 无下次触发 | 零值时间（**不是**错误） ⇒ 本地 `Option::None` | [`CronSpec::next_after_utc`] |
//! | 搜索视界 | 5 年 | [`SEARCH_HORIZON_YEARS`] |
//! | 时间基准 | 入参是**绝对时刻**，返回 UTC | [`next_occurrence_after_utc`] |
//!
//! 迭代方式与 robfig 一致：**按绝对分钟推进、用本地墙钟分量匹配**。于是 DST 两种情形自然对齐上游——
//! 春季跳变里不存在的本地分钟**不会**触发（跳过），秋季回拨里重复的本地分钟**会触发两次**。

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// robfig 的搜索视界：超过它还没匹配就认为「永不再触发」。
pub const SEARCH_HORIZON_YEARS: i32 = 5;

/// 上游 `NextOccurrencesUTC` 的硬上限（防「每秒」表达式在长 catch-up 窗口里爆量）。
pub const MAX_BETWEEN_OCCURRENCES: usize = 1024;

/// 上游 `cron.NewParser(Minute | Hour | Dom | Month | Dow)` 的字段数。
pub const FIELD_COUNT: usize = 5;

/// `cron-preview` 的拒绝码（上游 `autopilot_cron_preview.go` 的两个常量）。
pub const CODE_INVALID_CRON: &str = "invalid_cron";
/// 时区不认识 ≠ cron 语法错：编辑器要能指出是哪个输入框错了。
pub const CODE_INVALID_TIMEZONE: &str = "invalid_timezone";

/// 解析/时区错误。文案不必逐字等于 robfig，但**分类**必须能区分 `invalid_cron` 与
/// `invalid_timezone`（见 [`CronError::code`]）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    /// 字段数不是 5。
    #[error("expected exactly {expected} fields, found {found}: {expr}")]
    FieldCount {
        /// 期望字段数（恒为 5）。
        expected: usize,
        /// 实际字段数。
        found: usize,
        /// 原始表达式。
        expr: String,
    },
    /// 字段里的记号既不是合法数字也不是合法名字。
    #[error("failed to parse int from {field}: {value}")]
    ParseInt {
        /// 字段名（`minute` / `dom` …）。
        field: String,
        /// 出错的记号。
        value: String,
    },
    /// 取值超上界。
    #[error("end of range ({value}) above maximum ({max}): {field}")]
    AboveMax {
        /// 字段名。
        field: String,
        /// 实际取值。
        value: u32,
        /// 上界。
        max: u32,
    },
    /// 取值低于下界。
    #[error("beginning of range ({value}) below minimum ({min}): {field}")]
    BelowMin {
        /// 字段名。
        field: String,
        /// 实际取值。
        value: u32,
        /// 下界。
        min: u32,
    },
    /// `a-b` 起止倒置。
    #[error("beginning of range ({value}) beyond end of range ({end}): {field}")]
    InvertedRange {
        /// 字段名。
        field: String,
        /// 起点。
        value: u32,
        /// 终点。
        end: u32,
    },
    /// `*/0` 这类非法步长。
    #[error("step of range should be a positive number: {field}")]
    ZeroStep {
        /// 字段名。
        field: String,
    },
    /// `TZ=`/`CRON_TZ=` 后面没有排程。上游 robfig 在这里**panic**（`parser.go:99` 的 `slice[:-1]`），
    /// 而 `cron-preview` 直接把用户原文喂进来 ⇒ 必须变成 400 而不是 500。
    #[error("missing schedule after timezone prefix: {expr}")]
    MissingScheduleAfterTimezonePrefix {
        /// 原始表达式。
        expr: String,
    },
    /// 未知 IANA 时区。
    #[error("invalid timezone {timezone:?}")]
    InvalidTimezone {
        /// 时区名。
        timezone: String,
    },
}

impl CronError {
    /// HTTP 拒绝码（`invalid_cron` / `invalid_timezone`），`cron-preview` 直接用它。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidTimezone { .. } => CODE_INVALID_TIMEZONE,
            _ => CODE_INVALID_CRON,
        }
    }
}

/// 一个 cron 字段：位图 + 「基数是不是 `*`」。
///
/// `star` 必须单独存：robfig 用它决定 `dom`/`dow` 的 AND/OR 分支，而 `*/2` 的位图与
/// 显式列举无法区分（`*/2` 在分钟上是 `0,2,…,58`，与 `0-58/2` 完全相同，但前者**不算** `*`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    bits: u64,
    star: bool,
}

impl Field {
    fn parse(raw: &str, field: &'static str, min: u32, max: u32, names: &[(&str, u32)]) -> Result<Self, CronError> {
        let mut bits = 0_u64;
        let mut star = false;
        for item in raw.split(',') {
            let (range_and_step, step) = match item.split_once('/') {
                Some((range, step)) => (range, parse_uint(step, field)?),
                None => (item, 1),
            };
            if step == 0 {
                return Err(CronError::ZeroStep {
                    field: field.to_string(),
                });
            }
            let single = !range_and_step.contains('-');
            let start;
            let mut end;
            let is_star = range_and_step == "*" || range_and_step == "?";
            if is_star {
                start = min;
                end = max;
                // robfig：`*` 先记星号位，`/step>1` 再把它清掉（`getRange` 的 `if step > 1 { extra = 0 }`）。
                if step == 1 {
                    star = true;
                }
            } else {
                let (low, high) = match range_and_step.split_once('-') {
                    Some((low, high)) => (low, Some(high)),
                    None => (range_and_step, None),
                };
                start = parse_value(low, field, min, max, names)?;
                end = match high {
                    Some(high) => parse_value(high, field, min, max, names)?,
                    None => start,
                };
                // robfig：`N/step`（无 `-`）等价 `N-max/step`。
                if single && step > 1 {
                    end = max;
                }
            }
            if start > end {
                return Err(CronError::InvertedRange {
                    field: field.to_string(),
                    value: start,
                    end,
                });
            }
            let mut value = start;
            while value <= end {
                bits |= 1_u64 << value;
                value += step;
            }
        }
        Ok(Self { bits, star })
    }

    fn matches(self, value: u32) -> bool {
        self.bits & (1_u64 << value) != 0
    }
}

fn parse_uint(raw: &str, field: &'static str) -> Result<u32, CronError> {
    raw.parse::<u32>().map_err(|_| CronError::ParseInt {
        field: field.to_string(),
        value: raw.to_string(),
    })
}

fn parse_value(
    raw: &str,
    field: &'static str,
    min: u32,
    max: u32,
    names: &[(&str, u32)],
) -> Result<u32, CronError> {
    let lowered = raw.to_ascii_lowercase();
    let value = match names.iter().find(|(name, _)| *name == lowered) {
        Some((_, value)) => *value,
        None => parse_uint(raw, field)?,
    };
    if value > max {
        return Err(CronError::AboveMax {
            field: field.to_string(),
            value,
            max,
        });
    }
    if value < min {
        return Err(CronError::BelowMin {
            field: field.to_string(),
            value,
            min,
        });
    }
    Ok(value)
}

const MONTH_NAMES: [(&str, u32); 12] = [
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

const DOW_NAMES: [(&str, u32); 7] = [
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

/// 解析后的 5 字段排程（不含时区：时区由调用点按 [`Tz`] 传入或由 `TZ=` 前缀决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CronSpec {
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

impl CronSpec {
    /// 解析 5 字段表达式（**不含** `TZ=` 前缀；带前缀请用 [`parse_with_timezone`]）。
    ///
    /// # Errors
    ///
    /// 字段数不是 5、记号非法、取值越界、区间倒置、步长为 0 ⇒ [`CronError`]。
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != FIELD_COUNT {
            return Err(CronError::FieldCount {
                expected: FIELD_COUNT,
                found: fields.len(),
                expr: expr.to_string(),
            });
        }
        Ok(Self {
            minute: Field::parse(fields[0], "minute", 0, 59, &[])?,
            hour: Field::parse(fields[1], "hour", 0, 23, &[])?,
            dom: Field::parse(fields[2], "dom", 1, 31, &[])?,
            month: Field::parse(fields[3], "month", 1, 12, &MONTH_NAMES)?,
            dow: Field::parse(fields[4], "dow", 0, 6, &DOW_NAMES)?,
        })
    }

    /// `dom` / `dow` 的匹配，逐字对齐 robfig `SpecSchedule.dayMatches`：
    /// 一方为 `*` ⇒ AND；**两边都受限 ⇒ OR**（Vixie 语义）。
    fn day_matches(self, local: &DateTime<Tz>) -> bool {
        let dom_match = self.dom.star || self.dom.matches(local.day());
        let dow_match = self.dow.star || self.dow.matches(dow_number(local.weekday()));
        if self.dom.star || self.dow.star {
            dom_match && dow_match
        } else {
            dom_match || dow_match
        }
    }

    /// 严格晚于 `after` 的下一次触发（UTC）。`None` = 5 年视界内不再触发（上游的零值时间）。
    ///
    /// `after` 按**绝对时刻**解释；结果恒为 UTC。调度判定的入参应是 DB 时间（`SELECT now()`），
    /// 这样两个时钟偏移的实例会给出同一个 `plan_time`（上游 `NextOccurrenceAfterUTC` 的注释契约）。
    #[must_use]
    pub fn next_after_utc(self, after: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        let start = after
            .with_second(0)
            .and_then(|value| value.with_nanosecond(0))
            .unwrap_or(after)
            + Duration::minutes(1);
        let horizon = start + Duration::days(i64::from(SEARCH_HORIZON_YEARS) * 366);
        let mut instant = start;
        while instant <= horizon {
            let local = instant.with_timezone(&tz);
            if !self.month.matches(local.month()) {
                instant = jump(instant, next_month_start(&local, tz));
                continue;
            }
            if !self.day_matches(&local) {
                instant = jump(instant, next_day_start(&local, tz));
                continue;
            }
            if !self.hour.matches(local.hour()) {
                instant = jump(instant, next_hour_start(&local, tz));
                continue;
            }
            if !self.minute.matches(local.minute()) {
                instant = jump(instant, next_minute_start(&local, tz));
                continue;
            }
            return Some(instant);
        }
        None
    }
}

/// `chrono` 的 `Weekday` → cron 的 `0=Sun … 6=Sat`。
fn dow_number(weekday: chrono::Weekday) -> u32 {
    weekday.num_days_from_sunday()
}

/// 跳转必须**严格前进**：DST 的跳变会让本地时间戳落在过去（秋季回拨那一小时），
/// 直接采用会导致死循环 ⇒ 不满足时退回「绝对 +1 分钟」，与 robfig 的逐分钟推进等价。
fn jump(instant: DateTime<Utc>, target: Option<DateTime<Utc>>) -> DateTime<Utc> {
    match target {
        Some(target) if target > instant => target,
        _ => instant + Duration::minutes(1),
    }
}

fn local_to_utc(tz: Tz, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|value| value.to_utc())
}

fn next_month_start(local: &DateTime<Tz>, tz: Tz) -> Option<DateTime<Utc>> {
    let (year, month) = if local.month() == 12 {
        (local.year() + 1, 1)
    } else {
        (local.year(), local.month() + 1)
    };
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(0, 0, 0)?;
    local_to_utc(tz, naive)
}

fn next_day_start(local: &DateTime<Tz>, tz: Tz) -> Option<DateTime<Utc>> {
    let naive = local.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
    local_to_utc(tz, naive)
}

fn next_hour_start(local: &DateTime<Tz>, tz: Tz) -> Option<DateTime<Utc>> {
    let naive = wall_clock(local).checked_add_signed(Duration::hours(1))?;
    local_to_utc(tz, naive.with_minute(0)?.with_second(0)?)
}

fn next_minute_start(local: &DateTime<Tz>, tz: Tz) -> Option<DateTime<Utc>> {
    let naive = wall_clock(local).checked_add_signed(Duration::minutes(1))?;
    local_to_utc(tz, naive.with_second(0)?)
}

fn wall_clock(local: &DateTime<Tz>) -> NaiveDateTime {
    chrono::NaiveDateTime::new(local.date_naive(), local.time())
}

/// 把 `expr` 拆成（可选的 `TZ=`/`CRON_TZ=` 前缀时区，排程文本）。
///
/// 上游 `parseCronSchedule` 的护栏逐字搬过来：前缀后**缺空格**的形态会被 robfig **panic**
/// （`parser.go:99` 的 `slice[:-1]`），而 `cron-preview` 喂的是用户原文 ⇒ 变成错误而不是 500。
fn split_timezone_prefix(expr: &str) -> Result<(Option<&str>, &str), CronError> {
    for prefix in ["TZ=", "CRON_TZ="] {
        if let Some(rest) = expr.strip_prefix(prefix) {
            let Some((timezone, schedule)) = rest.split_once(' ') else {
                return Err(CronError::MissingScheduleAfterTimezonePrefix {
                    expr: expr.to_string(),
                });
            };
            return Ok((Some(timezone.trim()), schedule));
        }
    }
    Ok((None, expr))
}

/// 未知时区 → [`CronError::InvalidTimezone`]（上游 `service.ValidateTimezone`）。
///
/// # Errors
///
/// `tz` 不是 chrono-tz 认识的 IANA 名。
pub fn resolve_timezone(timezone: &str) -> Result<Tz, CronError> {
    timezone
        .trim()
        .parse::<Tz>()
        .map_err(|_| CronError::InvalidTimezone {
            timezone: timezone.to_string(),
        })
}

/// 解析「表达式 + 时区」：`TZ=` 前缀**覆盖** `fallback_timezone`（robfig 的行为），
/// 前缀存在但时区非法 / 排程非法都按 [`CronError`] 返回。
///
/// # Errors
///
/// 见 [`CronSpec::parse`] / [`resolve_timezone`]。
pub fn parse_with_timezone(expr: &str, fallback_timezone: &str) -> Result<(CronSpec, Tz), CronError> {
    let (prefix_timezone, schedule) = split_timezone_prefix(expr)?;
    let tz = match prefix_timezone {
        Some(timezone) => resolve_timezone(timezone)?,
        None => resolve_timezone(fallback_timezone)?,
    };
    Ok((CronSpec::parse(schedule)?, tz))
}

/// 上游 `NextOccurrenceAfterUTC`：`after` 之后的下一次触发（UTC），`None` = 不再触发。
///
/// # Errors
///
/// 表达式或时区非法。
pub fn next_occurrence_after_utc(
    expr: &str,
    timezone: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, CronError> {
    let (spec, tz) = parse_with_timezone(expr, timezone)?;
    Ok(spec.next_after_utc(after, tz))
}

/// 上游 `NextOccurrencesAfterUTC`：`after` 之后的下 `count` 个触发，升序、UTC。
/// 视界内不足 `count` 个就**返回短切片**（不是错误）。
///
/// # Errors
///
/// 表达式或时区非法。
pub fn next_occurrences_after_utc(
    expr: &str,
    timezone: &str,
    after: DateTime<Utc>,
    count: usize,
) -> Result<Vec<DateTime<Utc>>, CronError> {
    let (spec, tz) = parse_with_timezone(expr, timezone)?;
    let mut out = Vec::with_capacity(count);
    let mut cursor = after;
    for _ in 0..count {
        match spec.next_after_utc(cursor, tz) {
            Some(next) => {
                out.push(next);
                cursor = next;
            }
            None => break,
        }
    }
    Ok(out)
}

/// 上游 `NextOccurrencesUTC`：半开区间 `(after, until]` 内的**每一个**触发（UTC 升序）。
///
/// 硬上限 [`MAX_BETWEEN_OCCURRENCES`]，防「每分钟」表达式在长 catch-up 窗口里爆量
/// （上游还有一层 `JobSpec.MaxPlansPerTick`，那是 M5-7 的事）。
///
/// # Errors
///
/// 表达式或时区非法。
pub fn next_occurrences_between_utc(
    expr: &str,
    timezone: &str,
    after: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, CronError> {
    let (spec, tz) = parse_with_timezone(expr, timezone)?;
    let mut out = Vec::new();
    let mut cursor = after;
    while out.len() < MAX_BETWEEN_OCCURRENCES {
        match spec.next_after_utc(cursor, tz) {
            Some(next) if next <= until => {
                out.push(next);
                cursor = next;
            }
            _ => break,
        }
    }
    Ok(out)
}

/// 上游 `ComputeNextRun`：以**本地当前时刻**为锚算 `autopilot_trigger.next_run_at`
/// 这个**纯展示**列。
///
/// ⚠️ 调度判定**不得**用它 —— 派发必须走 [`next_occurrence_after_utc`] /
/// [`next_occurrences_between_utc`] 并锚在 DB 时间上（上游 `service/cron.go` 的注释契约）。
///
/// # Errors
///
/// 表达式或时区非法。
pub fn compute_next_run(expr: &str, timezone: &str) -> Result<Option<DateTime<Utc>>, CronError> {
    next_occurrence_after_utc(expr, timezone, Utc::now())
}

#[cfg(test)]
mod tests;
