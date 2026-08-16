# R728 — pc-feedback/{share,trace}/pure.rs

## 目标

补足 Node paperclip/server/src/services/feedback-share-client.ts 与
feedback-trace.ts 中的零 DB pure helpers，重点是 validation / limit
clamping / canonical 字段派生，让 share/trace service 层（非 pure 的
DB/HTTP 部分）有可单测的纯函数底座。

## 新增 helpers

### share/pure.rs (7 个)

| Node 语义 | Rust 函数 |
|---|---|
| build_object_key 前的 trace_id / company_id 非空校验 | validate_bundle_for_share(bundle) |
| backend URL schema 校验（http/https） | validate_backend_url(url) |
| object key segments 小写化 + trim | derive_object_key_segments(bundle) |
| 上传失败 status+message 描述合并 | describe_upload_failure(status, message) |
| payload byte_size 上报钳制 | clamp_payload_byte_size(byte_size) |

### trace/pure.rs (6 个)

| Node 语义 | Rust 函数 |
|---|---|
| traceId / issueId / companyId 非 nil 校验 | validate_trace_id / validate_issue_id / validate_company_id |
| list limit 钳制到 [0, 500] | clamp_trace_limit(requested) |
| limit == 0 时回退到默认 100 | resolve_trace_limit(requested) |
| hook 日志标签 | format_trace_hook_label(trace_id, issue_id) |

## 测试结果


running 61 tests
test pure::internal_tests::as_boolean_strict ... ok
test pure::internal_tests::as_number_filters_non_finite ... ok
test pure::internal_tests::append_note_dedup_and_trim ... ok
test pure::internal_tests::as_record_only_objects ... ok
test pure::internal_tests::as_string_trims_and_skips_empty ... ok
test pure::internal_tests::build_export_id_format ... ok
test pure::internal_tests::build_issue_path_basic ... ok
test pure::internal_tests::content_type_known_extensions ... ok
test pure::internal_tests::capture_status_full_for_known_sources ... ok
test pure::internal_tests::matches_skill_reference_variants ... ok
test pure::internal_tests::make_bundle_file_hashes ... ok
test pure::internal_tests::normalize_reason_only_for_down ... ok
test pure::internal_tests::parse_feedback_vote_values ... ok
test pure::internal_tests::capture_status_partial_and_unavailable ... ok
test pure::internal_tests::resolve_source_run_id_none ... ok
test pure::internal_tests::resolve_source_run_id_fallback_bundle ... ok
test pure::internal_tests::parse_run_log_entries_malformed_skipped ... ok
test pure::internal_tests::resolve_source_run_id_target_first ... ok
test pure::internal_tests::share_object_key_format ... ok
test pure::internal_tests::truncate_excerpt_collapses_whitespace ... ok
test pure::internal_tests::truncate_failure_reason_basic ... ok
test pure::internal_tests::unique_non_empty_dedup ... ok
test redaction::pure::internal_tests::increment_basic ... ok
test redaction::pure::internal_tests::increment_zero_skipped ... ok
test redaction::pure::internal_tests::is_plain_record_array_returns_false ... ok
test redaction::pure::internal_tests::is_plain_record_null_returns_false ... ok
test redaction::pure::internal_tests::is_plain_record_object ... ok
test redaction::pure::internal_tests::apply_pattern_reusable ... ok
test redaction::pure::internal_tests::is_plain_record_scalar_returns_false ... ok
test redaction::pure::internal_tests::record_field_basic ... ok
test redaction::pure::internal_tests::record_field_dedup ... ok
test redaction::pure::internal_tests::record_field_empty_skipped ... ok
test share::pure::internal_tests::clamp_payload_byte_size_normal ... ok
test share::pure::internal_tests::clamp_payload_byte_size_zero ... ok
test share::pure::internal_tests::derive_object_key_segments_trims ... ok
test share::pure::internal_tests::describe_upload_failure_no_status ... ok
test share::pure::internal_tests::derive_object_key_segments_lowercases ... ok
test share::pure::internal_tests::describe_upload_failure_includes_status ... ok
test share::pure::internal_tests::validate_backend_url_accepts_https ... ok
test share::pure::internal_tests::validate_backend_url_rejects_empty ... ok
test share::pure::internal_tests::validate_backend_url_rejects_non_http ... ok
test share::pure::internal_tests::validate_bundle_rejects_empty_company_id ... ok
test share::pure::internal_tests::validate_bundle_accepts_minimal ... ok
test share::pure::internal_tests::validate_bundle_rejects_empty_trace_id ... ok
test trace::pure::internal_tests::clamp_trace_limit_negative_to_zero ... ok
test trace::pure::internal_tests::clamp_trace_limit_normal ... ok
test trace::pure::internal_tests::clamp_trace_limit_too_large_clamped ... ok
test trace::pure::internal_tests::clamp_trace_limit_zero ... ok
test trace::pure::internal_tests::format_trace_hook_label_includes_both ... ok
test trace::pure::internal_tests::resolve_trace_limit_negative_falls_back ... ok
test trace::pure::internal_tests::resolve_trace_limit_preserves_small_positive ... ok
test redaction::pure::internal_tests::apply_pattern_single_match ... ok
test trace::pure::internal_tests::resolve_trace_limit_zero_falls_back ... ok
test redaction::pure::internal_tests::apply_pattern_multiple_matches ... ok
test trace::pure::internal_tests::validate_company_id_rejects_nil ... ok
test trace::pure::internal_tests::validate_issue_id_accepts_real ... ok
test trace::pure::internal_tests::validate_trace_id_accepts_real ... ok
test trace::pure::internal_tests::validate_issue_id_rejects_nil ... ok
test redaction::pure::internal_tests::apply_pattern_no_match ... ok
test trace::pure::internal_tests::validate_trace_id_rejects_nil ... ok
test redaction::pure::internal_tests::apply_pattern_replacement_with_capture ... ok

test result: ok. 61 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.01s

share::pure 12 个 + trace::pure 13 个 = **25 个新单测**

## 关键设计

- 所有 helper 零 IO / 零 DB / 纯函数，可直接在 share/trace service 层复用
- 与 share/service.rs 中的 Validation 错误信息字面对齐 Node
- 与 trace/service.rs 的 list limit 钳制对齐 Node MAX_TRACE_LIMIT=500
- mod 添加到 share/mod.rs 和 trace/mod.rs，pub use 由父 crate pc-feedback 平铺

## 文件

- 新增：crates/pc-feedback/src/share/pure.rs (4244 bytes)
- 新增：crates/pc-feedback/src/trace/pure.rs (3345 bytes)
- 修改：crates/pc-feedback/src/share/mod.rs (+1 行 mod pure;)
- 修改：crates/pc-feedback/src/trace/mod.rs (+1 行 mod pure;)
