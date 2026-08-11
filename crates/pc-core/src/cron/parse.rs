//! cron 表达式解析：token 切分 + 字段语法识别 + 边界校验

use std::collections::BTreeSet;

use super::{CronError, FieldSpec, ParsedCron, FIELD_SPECS};

// ============================================================================
// Top-level parse
// ============================================================================

pub fn parse_cron(expression: &str) -> Result<ParsedCron, CronError> {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Err(CronError::Empty);
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() != 5 {
        return Err(CronError::WrongFieldCount {
            expression: trimmed.to_string(),
            got: tokens.len(),
        });
    }

    Ok(ParsedCron {
        minutes: parse_field(tokens[0], FIELD_SPECS[0])?,
        hours: parse_field(tokens[1], FIELD_SPECS[1])?,
        days_of_month: parse_field(tokens[2], FIELD_SPECS[2])?,
        months: parse_field(tokens[3], FIELD_SPECS[3])?,
        days_of_week: parse_field(tokens[4], FIELD_SPECS[4])?,
    })
}

// ============================================================================
// Field parsing
// ============================================================================

/// 解析单个字段 token（如 `"5"` / `"1-3"` / `"*"` / `"*/10"` / `"1,3,5"`）。
///
/// 返回有序去重后的合法整数数组。
pub fn parse_field(token: &str, spec: FieldSpec) -> Result<Vec<u32>, CronError> {
    let mut values: BTreeSet<u32> = BTreeSet::new();

    // 先按逗号切分，每个 part 可以是值、范围、或 step
    for part in token.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return Err(CronError::EmptyElement { field: spec.name });
        }

        // step 语法：X/S（X 为 `*`、范围、或数字）
        if let Some(slash_idx) = trimmed.find('/') {
            let base = &trimmed[..slash_idx];
            let step_str = &trimmed[slash_idx + 1..];
            let step: u32 = step_str.parse().map_err(|_| CronError::InvalidStep {
                field: spec.name,
                step: step_str.to_string(),
            })?;
            if step == 0 {
                return Err(CronError::InvalidStep {
                    field: spec.name,
                    step: step_str.to_string(),
                });
            }

            let (range_start, range_end) = if base == "*" {
                // */S — 从 field min 起每 S 一值
                (spec.min, spec.max)
            } else if let Some(dash_idx) = base.find('-') {
                // N-M/S — 范围内每 S 一值
                let a_str = &base[..dash_idx];
                let b_str = &base[dash_idx + 1..];
                let a: u32 = a_str.parse().map_err(|_| CronError::InvalidRange {
                    field: spec.name,
                    base: base.to_string(),
                })?;
                let b: u32 = b_str.parse().map_err(|_| CronError::InvalidRange {
                    field: spec.name,
                    base: base.to_string(),
                })?;
                (a, b)
            } else {
                // N/S — 从 N 起每 S 一值
                let start: u32 = base.parse().map_err(|_| CronError::InvalidStart {
                    field: spec.name,
                    start: base.to_string(),
                })?;
                (start, spec.max)
            };

            validate_bounds(range_start, spec)?;
            validate_bounds(range_end, spec)?;
            if range_start > range_end {
                return Err(CronError::InvertedRange {
                    field: spec.name,
                    start: range_start as i64,
                    end: range_end as i64,
                });
            }

            let mut i = range_start;
            while i <= range_end {
                values.insert(i);
                // 防溢出：i + step 可能超过 u32::MAX
                match i.checked_add(step) {
                    Some(next) if next <= range_end => i = next,
                    _ => break,
                }
            }
            continue;
        }

        // range 语法：N-M
        if let Some(dash_idx) = trimmed.find('-') {
            let a_str = &trimmed[..dash_idx];
            let b_str = &trimmed[dash_idx + 1..];
            let a: u32 = a_str.parse().map_err(|_| CronError::InvalidValue {
                field: spec.name,
                value: trimmed.to_string(),
            })?;
            let b: u32 = b_str.parse().map_err(|_| CronError::InvalidValue {
                field: spec.name,
                value: trimmed.to_string(),
            })?;
            validate_bounds(a, spec)?;
            validate_bounds(b, spec)?;
            if a > b {
                return Err(CronError::InvertedRange {
                    field: spec.name,
                    start: a as i64,
                    end: b as i64,
                });
            }
            for v in a..=b {
                values.insert(v);
            }
            continue;
        }

        // 通配符 `*`
        if trimmed == "*" {
            for v in spec.min..=spec.max {
                values.insert(v);
            }
            continue;
        }

        // 单值
        let val: u32 = trimmed.parse().map_err(|_| CronError::InvalidValue {
            field: spec.name,
            value: trimmed.to_string(),
        })?;
        validate_bounds(val, spec)?;
        values.insert(val);
    }

    if values.is_empty() {
        return Err(CronError::EmptyResult { field: spec.name });
    }

    Ok(values.into_iter().collect())
}

fn validate_bounds(value: u32, spec: FieldSpec) -> Result<(), CronError> {
    if value < spec.min || value > spec.max {
        return Err(CronError::OutOfRange {
            field: spec.name,
            value: value as i64,
            min: spec.min as i64,
            max: spec.max as i64,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::validate_cron;
    use super::*;

    #[test]
    fn parse_wildcard() {
        let v = parse_field("*", FIELD_SPECS[0]).unwrap();
        assert_eq!(v.len(), 60);
        assert_eq!(v[0], 0);
        assert_eq!(v[59], 59);
    }

    #[test]
    fn parse_single_value() {
        assert_eq!(parse_field("5", FIELD_SPECS[0]).unwrap(), vec![5]);
        assert_eq!(parse_field("0", FIELD_SPECS[1]).unwrap(), vec![0]);
        assert_eq!(parse_field("23", FIELD_SPECS[1]).unwrap(), vec![23]);
    }

    #[test]
    fn parse_range() {
        assert_eq!(parse_field("1-3", FIELD_SPECS[0]).unwrap(), vec![1, 2, 3]);
        assert_eq!(
            parse_field("0-5", FIELD_SPECS[1]).unwrap(),
            vec![0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn parse_step_from_min() {
        // */15 on minute field → 0, 15, 30, 45
        assert_eq!(
            parse_field("*/15", FIELD_SPECS[0]).unwrap(),
            vec![0, 15, 30, 45]
        );
    }

    #[test]
    fn parse_step_from_start() {
        // 5/10 on minute field → 5, 15, 25, 35, 45, 55
        assert_eq!(
            parse_field("5/10", FIELD_SPECS[0]).unwrap(),
            vec![5, 15, 25, 35, 45, 55]
        );
    }

    #[test]
    fn parse_step_with_range() {
        // 10-30/5 on minute field → 10, 15, 20, 25, 30
        assert_eq!(
            parse_field("10-30/5", FIELD_SPECS[0]).unwrap(),
            vec![10, 15, 20, 25, 30]
        );
    }

    #[test]
    fn parse_list() {
        assert_eq!(parse_field("1,3,5", FIELD_SPECS[0]).unwrap(), vec![1, 3, 5]);
    }

    #[test]
    fn parse_list_with_ranges_and_steps() {
        assert_eq!(
            parse_field("0,15,30,45", FIELD_SPECS[0]).unwrap(),
            vec![0, 15, 30, 45]
        );
    }

    #[test]
    fn parse_dedup() {
        // 1,2,1,2 → [1, 2]
        assert_eq!(parse_field("1,2,1,2", FIELD_SPECS[0]).unwrap(), vec![1, 2]);
    }

    #[test]
    fn parse_empty_element_error() {
        let err = parse_field("1,,2", FIELD_SPECS[0]).unwrap_err();
        assert!(matches!(err, CronError::EmptyElement { .. }));
    }

    #[test]
    fn parse_out_of_range_error() {
        let err = parse_field("60", FIELD_SPECS[0]).unwrap_err();
        assert!(matches!(err, CronError::OutOfRange { .. }));
    }

    #[test]
    fn parse_inverted_range_error() {
        let err = parse_field("5-1", FIELD_SPECS[0]).unwrap_err();
        assert!(matches!(err, CronError::InvertedRange { .. }));
    }

    #[test]
    fn parse_invalid_value_error() {
        let err = parse_field("abc", FIELD_SPECS[0]).unwrap_err();
        assert!(matches!(err, CronError::InvalidValue { .. }));
    }

    #[test]
    fn parse_zero_step_error() {
        let err = parse_field("*/0", FIELD_SPECS[0]).unwrap_err();
        assert!(matches!(err, CronError::InvalidStep { .. }));
    }

    #[test]
    fn parse_full_expression() {
        let cron = parse_cron("0 * * * *").unwrap();
        assert_eq!(cron.minutes, vec![0]);
        assert_eq!(cron.hours.len(), 24);
        assert_eq!(cron.days_of_month.len(), 31);
        assert_eq!(cron.months.len(), 12);
        assert_eq!(cron.days_of_week.len(), 7);
    }

    #[test]
    fn parse_empty_expression() {
        assert!(matches!(parse_cron(""), Err(CronError::Empty)));
        assert!(matches!(parse_cron("   "), Err(CronError::Empty)));
    }

    #[test]
    fn parse_wrong_field_count() {
        let err = parse_cron("* * *").unwrap_err();
        assert!(matches!(err, CronError::WrongFieldCount { .. }));
    }

    #[test]
    fn parse_six_fields_error() {
        let err = parse_cron("* * * * * *").unwrap_err();
        assert!(matches!(err, CronError::WrongFieldCount { got: 6, .. }));
    }

    #[test]
    fn validate_cron_returns_none_for_valid() {
        assert_eq!(validate_cron("0 * * * *"), None);
        assert_eq!(validate_cron("*/15 * * * *"), None);
    }

    #[test]
    fn validate_cron_returns_err_for_invalid() {
        assert!(matches!(
            validate_cron("bad"),
            Some(CronError::WrongFieldCount { .. })
        ));
        assert!(matches!(
            validate_cron("60 * * * *"),
            Some(CronError::OutOfRange { .. })
        ));
    }
}
