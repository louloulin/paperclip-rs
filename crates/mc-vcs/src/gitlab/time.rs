//! GitLab 方言时间戳 → UTC `RFC3339Nano`（等价于 Go `time.Parse(layout).UTC().Format(time.RFC3339Nano)`）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 = `docs/32`
//! §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。**纯移动**：函数体、
//! 签名、可见性以外的一切都未改动。

/// 上游 `normalizeGitLabTime`：把 GitLab 的四种方言（以及 RFC3339 自身）统一成
/// **UTC 的 `RFC3339Nano`**；认不出的输入 ⇒ `None`（上游返回 `""`，handler 随之回落到
/// 摄入时间）。
///
/// 接受的上游 layout（逐条来自上游的 `layout` 列表）：
/// - `2006-01-02T15:04:05Z07:00`（RFC3339，含小数秒）
/// - `2006-01-02 15:04:05 MST`（GitLab 实际发的 `"2017-09-20 08:31:45 UTC"`）
/// - `2006-01-02 15:04:05 -0700`
/// - `2006-01-02 15:04:05.999999 MST`
///
/// ⚠️ 与上游的差异：命名时区只认 `UTC` / `GMT`（上游的 Go `time.Parse` 认整张 zoneinfo
/// 表）。GitLab 只会发 `UTC` 或数字偏移，而把整张 zoneinfo 表搬进本仓是没有收益的
/// 体积 ⇒ 其余命名时区返回 `None`（= 回落摄入时间，与上游的"解析失败"同路）。
pub fn normalize_gitlab_time(raw: &str) -> Option<String> {
    let parts = split_timestamp(raw)?;
    let seconds = parts.epoch_seconds()?;
    Some(format_rfc3339_nano(seconds, parts.nanos))
}

/// 拆出来的时间戳字段（全部 `i64`：公历算法里没有一处需要窄类型，`u32` 只会引来
/// `as` 截断警告 —— clippy 的 `cast_possible_truncation` 在 `-D warnings` 下是硬失败）。
struct TimestampParts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanos: i64,
    /// 相对 UTC 的偏移秒数（东为正）。
    offset_seconds: i64,
}

impl TimestampParts {
    fn epoch_seconds(&self) -> Option<i64> {
        if !(1..=12).contains(&self.month) || !(1..=31).contains(&self.day) {
            return None;
        }
        if self.hour > 23 || self.minute > 59 || self.second > 60 {
            return None;
        }
        Some(
            days_from_civil(self.year, self.month, self.day) * 86_400
                + self.hour * 3_600
                + self.minute * 60
                + self.second
                - self.offset_seconds,
        )
    }
}

/// 手写的语法解析（`YYYY-MM-DD` + `T`/空格 + `HH:MM:SS` + 可选小数秒 + 可选时区）。
fn split_timestamp(raw: &str) -> Option<TimestampParts> {
    let raw = raw.trim();
    let (date, rest) = raw.split_at_checked(10)?;
    let rest = rest.strip_prefix(['T', ' '])?;

    let (year, month, day) = parse_date(date)?;
    let (hour, minute, second, nanos, zone_text) = parse_time(rest)?;
    let offset_seconds = parse_offset(zone_text)?;

    Some(TimestampParts {
        year,
        month,
        day,
        hour,
        minute,
        second,
        nanos,
        offset_seconds,
    })
}

fn parse_date(date: &str) -> Option<(i64, i64, i64)> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    Some((
        parse_digits(&date[0..4])?,
        parse_digits(&date[5..7])?,
        parse_digits(&date[8..10])?,
    ))
}

/// `HH:MM:SS[.fff][zulu]` → 各字段 + 时区文本。
fn parse_time(rest: &str) -> Option<(i64, i64, i64, i64, &str)> {
    if rest.len() < 8 || rest.as_bytes()[2] != b':' || rest.as_bytes()[5] != b':' {
        return None;
    }
    let hour = parse_digits(&rest[0..2])?;
    let minute = parse_digits(&rest[3..5])?;
    let second = parse_digits(&rest[6..8])?;

    let mut tail = &rest[8..];
    let mut nanos = 0i64;
    if let Some(fraction) = tail.strip_prefix('.') {
        let digits: String = fraction.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        // 只看前 9 位（纳秒）；多出来的位数按 Go 的 layout 规则是**非法**的，这里截断
        // 到纳秒即可（不改变 UTC 时刻的排序语义，且不会丢整秒）。
        let kept: String = digits.chars().take(9).collect();
        let padded = format!("{kept:0<9}");
        nanos = padded.parse().ok()?;
        tail = &fraction[digits.len()..];
    }
    Some((hour, minute, second, nanos, tail.trim()))
}

/// 时区文本 → 相对 UTC 的秒数。`Z` / `UTC` / `GMT` / 空串都按 0（上游的 `MST` 分支
/// 在 GitLab 的实际载荷上就是 `UTC`）。
fn parse_offset(zone: &str) -> Option<i64> {
    if zone.is_empty() || zone == "Z" || zone == "z" || zone == "UTC" || zone == "GMT" {
        return Some(0);
    }
    let bytes = zone.as_bytes();
    let sign = match bytes.first()? {
        b'+' => 1i64,
        b'-' => -1i64,
        _ => return None,
    };
    let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours = parse_digits(&digits[0..2])?;
    let minutes = parse_digits(&digits[2..4])?;
    // 上游 `time.Parse` 会拒绝对不存在的偏移（`+25:00`）⇒ 这里同样拒绝，"解析失败" = `None`。
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn parse_digits(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// 上游 `time.RFC3339Nano`：小数秒**去掉尾随零**，零则整个省略。
fn format_rfc3339_nano(epoch_seconds: i64, nanos: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let seconds_of_day = epoch_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if nanos > 0 {
        let fraction = format!("{nanos:09}");
        let trimmed = fraction.trim_end_matches('0');
        out.push('.');
        out.push_str(trimmed);
    }
    out.push('Z');
    out
}

/// 公历 → 从 1970-01-01 起的天数（Howard Hinnant 的 `days_from_civil`，公开算法）。
pub(super) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let month_prime = (month + 9) % 12; // [0, 11]
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 天数 → 公历（Howard Hinnant 的 `civil_from_days`，`days_from_civil` 的逆）。
pub(super) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}
