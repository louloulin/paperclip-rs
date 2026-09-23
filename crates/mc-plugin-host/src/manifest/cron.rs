//! 计划型 hook 的 cron 引擎：五字段解析 + 「不得高于每五分钟一次」的下限校验。
//!
//! 这是 `github.com/robfig/cron/v3` v3.0.1 `Minute|Hour|Dom|Month|Dow` 解析器的
//! 逐字等价实现（无 `Descriptor`、无秒字段），差别只在：
//!
//! - 错误消息是英文自由文本的**近似**（判据是「拒没拒」而不是「消息逐字相同」）；
//! - Go 的 `time.Location` 在这里不存在（本 crate 无 tz 依赖）：调度匹配一律按 **UTC**
//!   计算，`timezone` 字段只做结构校验。见 `manifest.rs` 文件头注的差异 2。
//!
//! 上游 `pkg/plugincontract/manifest.go:710` `validateHookSchedule` 的语义：
//! 在 `2024-01-01T00:00Z` 起的 **400 天**窗口里逐对比较相邻两次触发时刻，任一对间隔
//! 小于 5 分钟即拒（高频表达式在头两次就返回）。窗口里 0 次或 1 次触发的表达式
//! （每月/每年级）按定义合法。

use std::fmt;

use super::MINIMUM_SCHEDULE_INTERVAL_SECS;

/// robfig 的 `starBit`：只有 base 是 `*` / `?` 且步长 ≤ 1 时才置位。
///
/// 它是 day-of-month 与 day-of-week 的 **AND/OR 开关**：任一方带 starBit 就取交集，
/// 双方都是「受限表达式」才取并集（见 [`CronSchedule::matches_date`]）。
const STAR_BIT: u64 = 1 << 63;

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_DAY: u64 = 86_400;

/// 扫描上限：robfig 的 `yearLimit = t.Year() + 5`，找不到就返回零值。
const SEARCH_DAYS_CEILING: u64 = 5 * 366;

/// 下限校验窗口的起点：2024-01-01T00:00:00Z（上游 `time.Date(2024, ...)`）。
const WINDOW_START_EPOCH: u64 = 1_704_067_200;

/// 下限校验窗口长度（上游 `start.AddDate(0, 0, 400)`）。
const WINDOW_DAYS: u64 = 400;

/// 解析/校验失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CronError {
    /// 表达式本身不合法（消息是 robfig 风格的近似）。
    Parse(String),
    /// 相邻两次触发间隔小于 5 分钟（上游 `MinimumScheduleInterval`）。
    TooFrequent,
}

impl fmt::Display for CronError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => formatter.write_str(message),
            Self::TooFrequent => {
                formatter.write_str("must not run more often than every five minutes")
            }
        }
    }
}

/// 一个字段的取值域 + 三字母名称表（robfig 的 `bounds`）。
struct Bounds {
    min: u32,
    max: u32,
    names: &'static [(&'static str, u32)],
}

const MINUTE: Bounds = Bounds {
    min: 0,
    max: 59,
    names: &[],
};
const HOUR: Bounds = Bounds {
    min: 0,
    max: 23,
    names: &[],
};
const DAY_OF_MONTH: Bounds = Bounds {
    min: 1,
    max: 31,
    names: &[],
};
const MONTH: Bounds = Bounds {
    min: 1,
    max: 12,
    names: &[
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
    ],
};
const DAY_OF_WEEK: Bounds = Bounds {
    min: 0,
    max: 6,
    names: &[
        ("sun", 0),
        ("mon", 1),
        ("tue", 2),
        ("wed", 3),
        ("thu", 4),
        ("fri", 5),
        ("sat", 6),
    ],
};

/// 解析后的五字段计划（robfig 的 `SpecSchedule`，不含秒字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CronSchedule {
    minute: u64,
    hour: u64,
    day_of_month: u64,
    month: u64,
    day_of_week: u64,
}

/// 解析一个五字段 cron 表达式。
///
/// # Errors
///
/// [`CronError::Parse`]：字段数不对、数值越界、range 反向、步长为 0、含 `@` 描述符等。
pub(super) fn parse(expression: &str) -> Result<CronSchedule, CronError> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Err(CronError::Parse("empty spec string".to_owned()));
    }
    // 上游的解析器**不含** `Descriptor` 选项。
    if expression.starts_with('@') {
        return Err(CronError::Parse(format!(
            "parser does not accept descriptors: {expression}"
        )));
    }
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(CronError::Parse(format!(
            "expected exactly 5 fields, found {}: [{}]",
            fields.len(),
            fields.join(" ")
        )));
    }
    Ok(CronSchedule {
        minute: parse_field(fields[0], &MINUTE)?,
        hour: parse_field(fields[1], &HOUR)?,
        day_of_month: parse_field(fields[2], &DAY_OF_MONTH)?,
        month: parse_field(fields[3], &MONTH)?,
        day_of_week: parse_field(fields[4], &DAY_OF_WEEK)?,
    })
}

/// 一个字段 = 逗号分隔的若干 range 的并集。
///
/// 空项被**丢弃**（robfig 用 `strings.FieldsFunc`），因此 `",,"` 得到一个空位集 —— 一个
/// 合法但永不触发的字段。镜像这个行为是为了让「上游接受 ⇒ 本仓接受」成立。
fn parse_field(field: &str, bounds: &Bounds) -> Result<u64, CronError> {
    let mut bits = 0u64;
    for expression in field.split(',').filter(|item| !item.is_empty()) {
        bits |= parse_range(expression, bounds)?;
    }
    Ok(bits)
}

/// `N` / `N-M` / `N/S` / `N-M/S` / `*` / `?`（+ 三字母名称）。
fn parse_range(expression: &str, bounds: &Bounds) -> Result<u64, CronError> {
    let mut slash_parts = expression.split('/');
    let base = slash_parts.next().unwrap_or_default();
    let mut step = 1u64;
    let mut has_step = false;
    if let Some(raw_step) = slash_parts.next() {
        if slash_parts.next().is_some() {
            return Err(CronError::Parse(format!("too many slashes: {expression}")));
        }
        step = parse_number(raw_step)?;
        has_step = true;
    }
    let mut hyphen_parts = base.split('-');
    let low = hyphen_parts.next().unwrap_or_default();
    let high = hyphen_parts.next();
    if hyphen_parts.next().is_some() {
        return Err(CronError::Parse(format!("too many hyphens: {expression}")));
    }
    let is_star = low == "*" || low == "?";
    let (start, mut end, mut extra) = if is_star {
        // `*-3` 这类畸形写法也落在 `*` 上：robfig 只看 `lowAndHigh[0]`。
        (u64::from(bounds.min), u64::from(bounds.max), STAR_BIT)
    } else {
        let start = parse_value(low, bounds)?;
        let end = match high {
            Some(raw) => parse_value(raw, bounds)?,
            None => start,
        };
        (start, end, 0)
    };
    if has_step {
        // "N/step" 意为 "N-max/step"。
        if high.is_none() {
            end = u64::from(bounds.max);
        }
        if step > 1 {
            extra = 0;
        }
    }
    if start < u64::from(bounds.min) {
        return Err(CronError::Parse(format!(
            "beginning of range ({start}) below minimum ({}): {expression}",
            bounds.min
        )));
    }
    if end > u64::from(bounds.max) {
        return Err(CronError::Parse(format!(
            "end of range ({end}) above maximum ({}): {expression}",
            bounds.max
        )));
    }
    if start > end {
        return Err(CronError::Parse(format!(
            "beginning of range ({start}) beyond end of range ({end}): {expression}"
        )));
    }
    if step == 0 {
        return Err(CronError::Parse(format!(
            "step of range should be a positive number: {expression}"
        )));
    }
    Ok(bits_between(start, end, step) | extra)
}

/// 名称或数字（名称大小写不敏感，先查表再当数字）。
fn parse_value(raw: &str, bounds: &Bounds) -> Result<u64, CronError> {
    let lowered = raw.to_ascii_lowercase();
    if let Some((_, value)) = bounds.names.iter().find(|(name, _)| *name == lowered) {
        return Ok(u64::from(*value));
    }
    parse_number(raw)
}

/// Go 的 `strconv.Atoi` 子集：允许前导 `+` 与 `007`；负数与非法字符报错。
fn parse_number(raw: &str) -> Result<u64, CronError> {
    let digits = raw.strip_prefix('+').unwrap_or(raw);
    let failed = || CronError::Parse(format!("failed to parse int from {raw}"));
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(failed());
    }
    digits.parse::<u64>().map_err(|_| failed())
}

/// `[start, end]` 内每 `step` 一个位置（robfig 的 `getBits`）。
fn bits_between(start: u64, end: u64, step: u64) -> u64 {
    let mut bits = 0u64;
    let mut value = start;
    while value <= end {
        bits |= 1u64 << value;
        value += step;
    }
    bits
}

impl CronSchedule {
    /// 严格晚于 `unix_seconds` 的下一次触发（分钟对齐）；5 年内找不到返回 `None`。
    pub(super) fn next_after(&self, unix_seconds: u64) -> Option<u64> {
        let mut candidate =
            unix_seconds / SECONDS_PER_MINUTE * SECONDS_PER_MINUTE + SECONDS_PER_MINUTE;
        for _ in 0..SEARCH_DAYS_CEILING {
            let day_start = candidate / SECONDS_PER_DAY * SECONDS_PER_DAY;
            let day_index = i64::try_from(day_start / SECONDS_PER_DAY).ok()?;
            let (year, month, day) = civil_from_days(day_index);
            if self.matches_date(year, month, day) {
                let mut second = candidate;
                while second < day_start + SECONDS_PER_DAY {
                    let minute_of_day = (second - day_start) / SECONDS_PER_MINUTE;
                    let hour = minute_of_day / 60;
                    let minute = minute_of_day % 60;
                    if self.hour & (1u64 << hour) != 0 && self.minute & (1u64 << minute) != 0 {
                        return Some(second);
                    }
                    second += SECONDS_PER_MINUTE;
                }
            }
            candidate = day_start + SECONDS_PER_DAY;
        }
        None
    }

    /// robfig 的 `dayMatches`：任一方带 starBit 取交集，否则取并集。
    fn matches_date(&self, year: i64, month: u64, day: u64) -> bool {
        if self.month & (1u64 << month) == 0 {
            return false;
        }
        let day_of_month = self.day_of_month & (1u64 << day) != 0;
        let day_of_week = self.day_of_week & (1u64 << weekday_of(year, month, day)) != 0;
        if self.day_of_month & STAR_BIT != 0 || self.day_of_week & STAR_BIT != 0 {
            day_of_month && day_of_week
        } else {
            day_of_month || day_of_week
        }
    }

    /// 「不得高于每五分钟一次」的下限校验（上游 400 天窗口的两两比较）。
    ///
    /// # Errors
    ///
    /// 任一对相邻触发间隔小于 [`MINIMUM_SCHEDULE_INTERVAL_SECS`]（UTC 下核算，见文件头注）。
    pub(super) fn validate_min_interval(&self) -> Result<(), CronError> {
        let end = WINDOW_START_EPOCH + WINDOW_DAYS * SECONDS_PER_DAY;
        // 窗口起点本身是分钟对齐的：从「起点前 1 分钟」开始取，得到 ≥ 起点的首次触发。
        let Some(mut previous) = self.next_after(WINDOW_START_EPOCH - SECONDS_PER_MINUTE) else {
            return Ok(());
        };
        while previous <= end {
            let Some(next) = self.next_after(previous) else {
                return Ok(());
            };
            if next > end {
                return Ok(());
            }
            if next - previous < MINIMUM_SCHEDULE_INTERVAL_SECS {
                return Err(CronError::TooFrequent);
            }
            previous = next;
        }
        Ok(())
    }
}

/// 0 = 周日 … 6 = 周六（Go 的 `time.Weekday`）。
fn weekday_of(year: i64, month: u64, day: u64) -> u64 {
    let days = days_from_civil(year, month, day);
    // 1970-01-01 是周四（Go 里 = 4）。
    u64::try_from((days + 4).rem_euclid(7)).unwrap_or(0)
}

/// Howard Hinnant 的 `days_from_civil`：公历 → 自 1970-01-01 起的天数。
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let month = to_i64(month);
    let day = to_i64(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 逆向：天序号 → `(年, 月, 日)`（`days_from_civil` 的逆）。
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        u64::try_from(month).unwrap_or(1),
        u64::try_from(day).unwrap_or(1),
    )
}

/// `u64 → i64`（取值域：月份 ≤ 12、日 ≤ 31、天序号 ≤ 数十万，永远装得下）。
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(expression: &str) -> CronSchedule {
        parse(expression).expect("parses")
    }

    fn parse_error(expression: &str) -> String {
        match parse(expression) {
            Err(CronError::Parse(message)) => message,
            other => panic!("expected parse error for {expression}, got {other:?}"),
        }
    }

    #[test]
    fn five_fields_are_required() {
        let message = parse_error("0 0 *");
        assert!(
            message.contains("expected exactly 5 fields, found 3"),
            "{message}"
        );
        assert!(parse_error("@daily").contains("descriptors"));
        assert_eq!(parse_error(""), "empty spec string");
        assert!(parse("  0   0  *  *  *  ").is_ok(), "whitespace collapses");
    }

    #[test]
    fn star_bit_follows_robfig_step_rule() {
        // `*` 与 `*/1` 置 starBit；`*/2` 及其它显式步长不置。
        assert_ne!(schedule("* * * * *").minute & STAR_BIT, 0);
        assert_ne!(schedule("?,* * * * *").minute & STAR_BIT, 0);
        assert_ne!(schedule("*/1 * * * *").minute & STAR_BIT, 0);
        assert_eq!(schedule("*/5 * * * *").minute & STAR_BIT, 0);
        assert_eq!(schedule("5/2 * * * *").minute & STAR_BIT, 0);
        assert_ne!(schedule("* * * * *").day_of_month & STAR_BIT, 0);
        assert_ne!(schedule("* * * * *").day_of_week & STAR_BIT, 0);
    }

    #[test]
    fn bits_match_robfig_get_bits() {
        let every_five = schedule("*/5 * * * *");
        assert_eq!(every_five.minute, bits_between(0, 59, 5));
        // "N/step" 意为 "N-max/step"
        assert_eq!(schedule("5/10 * * * *").minute, bits_between(5, 59, 10));
        assert_eq!(schedule("1-10/3 * * * *").minute, bits_between(1, 10, 3));
        assert_eq!(
            schedule("0 0 * * MON").day_of_week,
            1u64 << 1,
            "SUN=0 … SAT=6"
        );
        assert_eq!(schedule("0 0 * JAN *").month, 1u64 << 1);
        assert_eq!(
            schedule("0 0 * jAn *").month,
            1u64 << 1,
            "names are lowercased"
        );
        // 空项被丢弃 ⇒ 合法但永不触发
        assert_eq!(schedule("1,,2 * * * *").minute, (1u64 << 1) | (1u64 << 2));
        assert_eq!(schedule(",,, * * * *").minute, 0);
        // 前导零/前导加号按 Atoi 语义接受
        assert_eq!(schedule("007 * * * *").minute, 1u64 << 7);
        assert_eq!(schedule("+7 * * * *").minute, 1u64 << 7);
    }

    #[test]
    fn range_errors() {
        assert!(parse_error("60 * * * *").contains("above maximum (59)"));
        assert!(parse_error("* 24 * * *").contains("above maximum (23)"));
        assert!(parse_error("* * 0 * *").contains("below minimum (1)"));
        assert!(parse_error("* * * 13 *").contains("above maximum (12)"));
        assert!(parse_error("* * * * 7").contains("above maximum (6)"));
        assert!(parse_error("10-5 * * * *").contains("beyond end of range"));
        assert!(parse_error("*/0 * * * *").contains("positive number"));
        assert!(parse_error("1/2/3 * * * *").contains("too many slashes"));
        assert!(parse_error("1-2-3 * * * *").contains("too many hyphens"));
        assert!(parse_error("FOO * * * *").contains("failed to parse int"));
        assert!(parse_error("five * * * *").contains("failed to parse int"));
    }

    #[test]
    fn next_after_lands_on_the_matching_minute() {
        // 2024-01-01T00:00:00Z 是周一。
        assert_eq!(
            schedule("* * * * *").next_after(1_704_067_200),
            Some(1_704_067_260)
        );
        assert_eq!(
            schedule("*/5 * * * *").next_after(1_704_067_200),
            Some(1_704_067_500)
        );
        assert_eq!(
            schedule("0 0 * * MON").next_after(1_704_067_200),
            Some(1_704_672_000),
            "下一个周一 2024-01-08"
        );
        assert_eq!(
            schedule("0 0 29 2 *").next_after(0),
            Some(68_169_600),
            "epoch 之后第一个 2 月 29 日是 1972-02-29"
        );
        assert_eq!(
            schedule("0 0 29 2 *").next_after(1_704_067_200),
            Some(1_709_164_800),
            "2024-02-29"
        );
        // 双方都受限 ⇒ 并集：2024-01-31T00:00Z 之后是 2024-02-01（周四，dom=1 命中）
        assert_eq!(
            schedule("0 0 1 * MON").next_after(1_706_659_200),
            Some(1_706_745_600),
            "2024-02-01 不是周一，但 dom=1 经并集命中"
        );
        // dom 带 starBit ⇒ 交集：`*/1` 是 `*`，于是只命中周一的 2024-02-05
        assert_eq!(
            schedule("0 0 */1 * MON").next_after(1_706_659_200),
            Some(1_707_091_200)
        );
        // 永不触发的字段 → 5 年内无解
        assert_eq!(schedule(",,, * * * *").next_after(0), None);
    }

    #[test]
    fn min_interval_enforces_five_minutes() {
        assert!(schedule("*/5 * * * *").validate_min_interval().is_ok());
        assert!(schedule("0 * * * *").validate_min_interval().is_ok());
        assert!(schedule("0 0 * * *").validate_min_interval().is_ok());
        assert!(schedule("0 0 1 * *").validate_min_interval().is_ok());
        assert!(schedule("0 0 29 2 *").validate_min_interval().is_ok());
        assert!(schedule("0 0 */3 * *").validate_min_interval().is_ok());
        // 一个月里可能只有 1 次触发（4 年一次）—— 按定义合法
        assert!(schedule("0 0 31 2 *").validate_min_interval().is_ok());
        for too_frequent in [
            "* * * * *",
            "*/2 * * * *",
            "*/4 * * * *",
            "* 0 * * *",
            "0-30/3 * * * *",
        ] {
            assert_eq!(
                schedule(too_frequent).validate_min_interval(),
                Err(CronError::TooFrequent),
                "{too_frequent}"
            );
        }
    }

    #[test]
    fn day_of_month_and_day_of_week_use_robfig_union_rule() {
        // 双方受限（无 starBit）⇒ 并集：31 号（周三）与周一都命中。
        let union = schedule("0 0 1 * MON");
        assert!(union.matches_date(2024, 2, 1), "dom 命中");
        assert!(union.matches_date(2024, 2, 5), "dow 命中");
        assert!(!union.matches_date(2024, 2, 6));
        // day-of-week 带 starBit ⇒ 交集：必须是周一。
        let intersection = schedule("0 0 * * MON");
        assert!(intersection.matches_date(2024, 2, 5));
        assert!(!intersection.matches_date(2024, 2, 1));
        // 月份不匹配直接出局
        assert!(!schedule("0 0 * JAN *").matches_date(2024, 2, 5));
    }

    #[test]
    fn civil_date_helpers_round_trip() {
        for days in [-1_i64, -719_468, 0, 1, 19_723, 19_782, 1_000_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "{days}");
        }
        assert_eq!(civil_from_days(days_from_civil(1970, 1, 1)), (1970, 1, 1));
        assert_eq!(weekday_of(1970, 1, 1), 4, "周四");
        assert_eq!(weekday_of(2024, 1, 1), 1, "周一");
        assert_eq!(weekday_of(2024, 2, 29), 4, "周四");
    }
}
