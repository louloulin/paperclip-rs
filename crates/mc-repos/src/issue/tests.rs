use mc_core::Id;

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn prefix_from_slug_uppercases_and_truncates() {
    assert_eq!(issue_prefix_from_slug("acme-corp"), "ACMECORP");
    assert_eq!(issue_prefix_from_slug("lum"), "LUM");
    assert_eq!(issue_prefix_from_slug("abcdefghijkl"), "ABCDEFGH");
    assert_eq!(issue_prefix_from_slug("!!!"), "ISS");
    assert_eq!(issue_prefix_from_slug(""), "ISS");
}

#[test]
fn move_position_between_two_anchors() {
    assert_eq!(derive_move_position(Some(1.0), Some(2.0), 5.0), Some(1.5));
    assert_eq!(derive_move_position(Some(1.0), None, 5.0), Some(2.0));
    assert_eq!(derive_move_position(None, Some(4.0), 5.0), Some(3.0));
    assert_eq!(derive_move_position(None, None, 7.5), Some(7.5));
}

#[test]
fn move_position_rejects_bad_anchors() {
    // 顺序错乱
    assert_eq!(derive_move_position(Some(2.0), Some(1.0), 0.0), None);
    // 相邻浮点无法二分（间距过小）
    assert_eq!(derive_move_position(Some(1.0), Some(1.0), 0.0), None);
    // 非有限值 / 溢出为 NaN
    assert_eq!(derive_move_position(Some(f64::INFINITY), None, 0.0), None);
    assert_eq!(
        derive_move_position(Some(f64::MAX), Some(f64::INFINITY), 0.0),
        None
    );
}

#[test]
fn split_comma_param_trims_and_drops_empties() {
    assert_eq!(
        split_comma_param(" todo , in_progress ,, "),
        Some(vec!["todo".into(), "in_progress".into()])
    );
    assert_eq!(split_comma_param("   "), None);
    assert_eq!(split_comma_param(""), None);
}

#[test]
fn update_is_empty_ignores_expected_revision() {
    let patch = IssueUpdate {
        expected_revision: Some(3),
        ..IssueUpdate::default()
    };
    assert!(patch.is_empty());
    let patch = IssueUpdate {
        title: Some("x".into()),
        ..IssueUpdate::default()
    };
    assert!(!patch.is_empty());
    let patch = IssueUpdate {
        description: Some(None), // 显式置空也是变更
        ..IssueUpdate::default()
    };
    assert!(!patch.is_empty());
}

#[test]
fn group_field_whitelist() {
    assert_eq!(IssueGroupField::parse(""), Some(IssueGroupField::Status));
    assert_eq!(
        IssueGroupField::parse("priority"),
        Some(IssueGroupField::Priority)
    );
    assert_eq!(
        IssueGroupField::parse("assignee"),
        Some(IssueGroupField::Assignee)
    );
    assert_eq!(
        IssueGroupField::parse("project_id"),
        Some(IssueGroupField::Project)
    );
    assert_eq!(IssueGroupField::parse("drop table"), None);
    assert_eq!(IssueGroupField::Status.as_str(), "status");
}

#[test]
fn list_where_placeholders_are_unique() {
    // 防回归：WHERE 片段里 $1..$13 各出现至少一次，且不含 $14/$15（留给 LIMIT/OFFSET）。
    for n in 1..=13 {
        assert!(
            LIST_WHERE.contains(&format!("${n}")),
            "missing ${n} in LIST_WHERE"
        );
    }
    assert!(!LIST_WHERE.contains("$14"));
}

#[test]
fn filter_defaults_include_closed() {
    let f = IssueFilter::new(Id::nil());
    assert!(f.include_closed);
    assert!(f.terminal_statuses.is_empty());
    assert_eq!(f.limit, None);
    assert_eq!(f.order, IssueOrderBy::UpdatedDesc);
}
