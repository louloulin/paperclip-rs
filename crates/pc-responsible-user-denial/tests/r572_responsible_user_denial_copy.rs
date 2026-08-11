//! R572 — pc-responsible-user-denial-copy → pc-responsible-user-denial 集成验证。
//!
//! 验证本 crate 通过 `copy` 子模块对外提供 pc-responsible-user-denial-copy 的
//! 全部公开 API，并保证：
//!
//! 1. 常量 `RESPONSIBLE_USER_DENIAL_CODES` 与 pc-responsible-user-denial-copy 中
//!    的定义字节级一致（两 crate 共享同一来源真相）。
//! 2. `is_responsible_user_denial_code` 只接受 copy-side 的两个大写代码，
//!    不接受 run-outcome 的小写代码。
//! 3. `render_responsible_user_denial_copy` 对已知 copy code 渲染出完整文案，
//!    对 run-outcome code 返回 `None`。
//! 4. `describe_responsible_user_denial` 的输出与 pc-responsible-user-denial-copy
//!    的直接调用一致（验证我们没有 re-wrap 引入漂移）。
//! 5. 顶层 `is_responsible_user_denial_code` re-export 仍然指向 run-outcome 域
//!    的 `is_valid_code`（保持向后兼容）。

#![allow(clippy::doc_markdown)]

use pc_responsible_user_denial::copy::{
    describe_responsible_user_denial, is_responsible_user_denial_code,
    render_responsible_user_denial_copy, responsible_user_label, ResponsibleUserDenialCode,
    ResponsibleUserDenialOptions, ResponsibleUserDenialTone, RESPONSIBLE_USER_DENIAL_CODES,
};
// 验证顶层 `is_responsible_user_denial_code` re-export 仍然指向 run-outcome 域
// （保持 R558/R706 的向后兼容别名）。
use pc_responsible_user_denial::is_responsible_user_denial_code as top_level_gate;

#[test]
fn r572_constants_match_copy_crate_byte_for_byte() {
    // 与 pc-responsible-user-denial-copy 内部定义完全一致
    assert_eq!(
        RESPONSIBLE_USER_DENIAL_CODES,
        [
            "RESPONSIBLE_USER_UNAUTHORIZED",
            "RESPONSIBLE_USER_UNAVAILABLE"
        ]
    );
    // 同时验证通过 re-export 与直接导入得到同一常量
    use pc_responsible_user_denial_copy::RESPONSIBLE_USER_DENIAL_CODES as COPY_CONSTANTS;
    assert_eq!(RESPONSIBLE_USER_DENIAL_CODES, COPY_CONSTANTS);
}

#[test]
fn r572_copy_gate_only_accepts_copy_codes() {
    assert!(is_responsible_user_denial_code(
        "RESPONSIBLE_USER_UNAUTHORIZED"
    ));
    assert!(is_responsible_user_denial_code(
        "RESPONSIBLE_USER_UNAVAILABLE"
    ));
    // 必须拒绝 run-outcome 域代码（小写、蛇形）
    assert!(!is_responsible_user_denial_code("rate_limited"));
    assert!(!is_responsible_user_denial_code("unsupported_channel"));
    assert!(!is_responsible_user_denial_code("quota_exceeded"));
    assert!(!is_responsible_user_denial_code("not_entitled"));
    assert!(!is_responsible_user_denial_code("other"));
    // 必须拒绝其它噪声字符串
    assert!(!is_responsible_user_denial_code(""));
    assert!(!is_responsible_user_denial_code("OTHER_CODE"));
}

#[test]
fn r572_top_level_alias_is_run_outcome_domain() {
    // 顶层 re-export 仍然等价于 run-outcome 的 `is_valid_code`，
    // 所以它接受小写蛇形代码、拒绝大写 copy 代码。
    assert!(top_level_gate("rate_limited"));
    assert!(top_level_gate("not_entitled"));
    assert!(top_level_gate("other"));
    assert!(!top_level_gate("RESPONSIBLE_USER_UNAUTHORIZED"));
    assert!(!top_level_gate("RESPONSIBLE_USER_UNAVAILABLE"));
}

#[test]
fn r572_render_unauthorized_with_known_name() {
    let copy = render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAUTHORIZED", Some("Alice"))
        .expect("should resolve to copy");
    assert_eq!(copy.code, ResponsibleUserDenialCode::Unauthorized);
    assert_eq!(copy.tone, ResponsibleUserDenialTone::Unauthorized);
    assert!(!copy.title.is_empty());
    assert!(!copy.description.is_empty());
    assert!(!copy.recommended_action.is_empty());
    // Known name should appear in the description or action text.
    assert!(copy.description.contains("Alice"));
    assert!(copy.recommended_action.contains("Alice"));
}

#[test]
fn r572_render_unauthorized_with_blank_name_falls_back() {
    let copy_blank =
        render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAUTHORIZED", Some("   "))
            .expect("should resolve");
    assert!(copy_blank.description.contains("the responsible user"));

    let copy_none = render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAUTHORIZED", None)
        .expect("should resolve");
    assert!(copy_none.description.contains("the responsible user"));
}

#[test]
fn r572_render_unavailable_with_name() {
    let copy = render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAVAILABLE", Some("Bob"))
        .expect("should resolve");
    assert_eq!(copy.code, ResponsibleUserDenialCode::Unavailable);
    assert_eq!(copy.tone, ResponsibleUserDenialTone::Unavailable);
    assert!(!copy.title.is_empty());
    assert!(!copy.description.is_empty());
    assert!(!copy.recommended_action.is_empty());
    assert!(copy.description.contains("Bob"));
}

#[test]
fn r572_render_rejects_run_outcome_codes() {
    // run-outcome code 不应通过 render helper
    assert!(render_responsible_user_denial_copy("rate_limited", Some("Eve")).is_none());
    assert!(render_responsible_user_denial_copy("not_entitled", None).is_none());
    assert!(render_responsible_user_denial_copy("other", Some("Mallory")).is_none());
    assert!(render_responsible_user_denial_copy("", None).is_none());
    assert!(render_responsible_user_denial_copy("nope", Some("Trent")).is_none());
}

#[test]
fn r572_two_codes_produce_distinct_copy() {
    let unauthorized =
        render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAUTHORIZED", Some("Alice"))
            .expect("unauthorized resolves");
    let unavailable =
        render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAVAILABLE", Some("Alice"))
            .expect("unavailable resolves");

    assert_ne!(unauthorized.code, unavailable.code);
    assert_ne!(unauthorized.tone, unavailable.tone);
    assert_ne!(unauthorized.title, unavailable.title);
    // Description starts differ enough to be visually distinct.
    assert_ne!(
        unauthorized.description.split('.').next(),
        unavailable.description.split('.').next(),
    );
}

#[test]
fn r572_delegation_matches_direct_copy_call() {
    // 直接调用 copy crate 与通过本 crate bridge 调用应得到完全相同的对象。
    let direct = describe_responsible_user_denial(
        ResponsibleUserDenialCode::Unavailable,
        Some(ResponsibleUserDenialOptions {
            user_name: Some("Carol"),
        }),
    );
    let bridged =
        render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAVAILABLE", Some("Carol"))
            .expect("should resolve");
    assert_eq!(direct, bridged);
}

#[test]
fn r572_label_helper_consistent() {
    // `responsible_user_label` 在两个 crate 间行为一致
    assert_eq!(responsible_user_label(None), "the responsible user");
    assert_eq!(responsible_user_label(Some("")), "the responsible user");
    assert_eq!(responsible_user_label(Some("  Dave  ")), "Dave");
}

#[test]
fn r572_copy_module_path_re_exports_resolve() {
    // 确认通过 `pc_responsible_user_denial::copy` 命名空间能访问所有公开类型
    fn _accepts_code(_: ResponsibleUserDenialCode) {}
    fn _accepts_tone(_: ResponsibleUserDenialTone) {}
    fn _accepts_copy(_: &pc_responsible_user_denial::copy::ResponsibleUserDenialCopy) {}

    let _: ResponsibleUserDenialCode = ResponsibleUserDenialCode::Unauthorized;
    let _: ResponsibleUserDenialTone = ResponsibleUserDenialTone::Unavailable;

    // suppress unused-fn warnings
    let _ = _accepts_code;
    let _ = _accepts_tone;
    let _ = _accepts_copy;
}
