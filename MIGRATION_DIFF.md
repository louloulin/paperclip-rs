# paperclip-rs Migration Diff Report

Generated: 2026-08-20T12:59:38.136420+00:00

## Summary

- Total migrations: 207
- Upstream Node baseline tables: 109
- Rust migrations CREATE TABLE count: 281
- Distribution by category:
  - `deprecation`: 1
  - `initial`: 1
  - `new-index`: 5
  - `new-table`: 4
  - `other`: 196

## Per-Migration

| # | File | Category | CREATE TABLE |
|---|------|----------|-------------:|
| 1 | `0000_mature_masked_marvel.sql` | initial | 11 |
| 2 | `0001_fast_northstar.sql` | other | 3 |
| 3 | `0002_big_zaladane.sql` | other | 0 |
| 4 | `0003_shallow_quentin_quire.sql` | other | 0 |
| 5 | `0004_issue_identifiers.sql` | other | 0 |
| 6 | `0005_chief_luke_cage.sql` | other | 1 |
| 7 | `0006_overjoyed_mister_sinister.sql` | other | 2 |
| 8 | `0007_new_quentin_quire.sql` | other | 1 |
| 9 | `0008_amused_zzzax.sql` | other | 0 |
| 10 | `0009_fast_jackal.sql` | other | 2 |
| 11 | `0010_stale_justin_hammer.sql` | other | 2 |
| 12 | `0011_windy_corsair.sql` | other | 1 |
| 13 | `0012_perpetual_ser_duncan.sql` | other | 0 |
| 14 | `0013_dashing_wasp.sql` | other | 0 |
| 15 | `0014_many_mikhail_rasputin.sql` | other | 9 |
| 16 | `0015_project_color_archived.sql` | other | 0 |
| 17 | `0016_agent_icon.sql` | other | 0 |
| 18 | `0017_tiresome_gabe_jones.sql` | other | 0 |
| 19 | `0018_flat_sleepwalker.sql` | other | 2 |
| 20 | `0019_public_victor_mancha.sql` | other | 1 |
| 21 | `0020_white_anita_blake.sql` | other | 0 |
| 22 | `0021_chief_vindicator.sql` | other | 0 |
| 23 | `0022_company_brand_color.sql` | other | 0 |
| 24 | `0023_fair_lethal_legion.sql` | other | 0 |
| 25 | `0024_far_beast.sql` | other | 0 |
| 26 | `0025_nasty_salo.sql` | other | 1 |
| 27 | `0026_lying_pete_wisdom.sql` | other | 1 |
| 28 | `0027_tranquil_tenebrous.sql` | other | 0 |
| 29 | `0028_harsh_goliath.sql` | other | 3 |
| 30 | `0029_plugin_tables.sql` | other | 9 |
| 31 | `0030_rich_magneto.sql` | other | 1 |
| 32 | `0031_zippy_magma.sql` | other | 1 |
| 33 | `0032_pretty_doctor_octopus.sql` | other | 2 |
| 34 | `0033_shiny_black_tarantula.sql` | other | 0 |
| 35 | `0034_fat_dormammu.sql` | other | 0 |
| 36 | `0035_marvelous_satana.sql` | other | 2 |
| 37 | `0036_cheerful_nitro.sql` | other | 1 |
| 38 | `0037_friendly_eddie_brock.sql` | other | 1 |
| 39 | `0038_careless_iron_monger.sql` | other | 0 |
| 40 | `0039_fat_magneto.sql` | other | 6 |
| 41 | `0040_eager_shotgun.sql` | other | 0 |
| 42 | `0041_curly_maria_hill.sql` | other | 0 |
| 43 | `0042_spotty_the_renegades.sql` | other | 2 |
| 44 | `0043_reflective_captain_universe.sql` | other | 0 |
| 45 | `0044_illegal_toad.sql` | other | 4 |
| 46 | `0045_workable_shockwave.sql` | other | 1 |
| 47 | `0046_smooth_sentinels.sql` | other | 0 |
| 48 | `0047_overjoyed_groot.sql` | other | 4 |
| 49 | `0048_flashy_marrow.sql` | other | 0 |
| 50 | `0049_flawless_abomination.sql` | other | 1 |
| 51 | `0050_stiff_luckman.sql` | other | 0 |
| 52 | `0051_young_korg.sql` | other | 0 |
| 53 | `0052_mushy_trauma.sql` | other | 1 |
| 54 | `0053_sharp_wild_child.sql` | other | 2 |
| 55 | `0054_draft_routines.sql` | other | 0 |
| 56 | `0055_kind_weapon_omega.sql` | other | 0 |
| 57 | `0056_spooky_ultragirl.sql` | other | 2 |
| 58 | `0057_tidy_join_requests.sql` | other | 0 |
| 59 | `0058_wealthy_starbolt.sql` | other | 0 |
| 60 | `0059_plugin_database_namespaces.sql` | other | 4 |
| 61 | `0060_orange_annihilus.sql` | other | 2 |
| 62 | `0061_lively_thor_girl.sql` | other | 0 |
| 63 | `0062_routine_run_dispatch_fingerprint.sql` | other | 0 |
| 64 | `0063_issue_thread_interactions.sql` | other | 2 |
| 65 | `0064_issue_thread_interaction_idempotency.sql` | other | 0 |
| 66 | `0065_environments.sql` | other | 2 |
| 67 | `0066_issue_tree_holds.sql` | other | 4 |
| 68 | `0067_agent_default_environment.sql` | other | 0 |
| 69 | `0068_environment_local_driver_unique.sql` | other | 0 |
| 70 | `0069_liveness_recovery_dedupe.sql` | other | 0 |
| 71 | `0070_active_run_output_watchdog.sql` | other | 2 |
| 72 | `0071_default_hire_approval_off.sql` | other | 0 |
| 73 | `0072_large_sandman.sql` | other | 0 |
| 74 | `0073_shiny_salo.sql` | other | 0 |
| 75 | `0074_striped_genesis.sql` | other | 0 |
| 76 | `0075_cultured_sebastian_shaw.sql` | other | 0 |
| 77 | `0076_useful_elektra.sql` | other | 2 |
| 78 | `0077_unusual_karnak.sql` | other | 2 |
| 79 | `0078_white_darwin.sql` | other | 0 |
| 80 | `0079_company_search_document_indexes.sql` | new-index | 0 |
| 81 | `0080_company_search_fuzzystrmatch.sql` | other | 0 |
| 82 | `0081_optimal_dormammu.sql` | other | 0 |
| 83 | `0082_dry_vision.sql` | other | 4 |
| 84 | `0083_company_secret_provider_configs.sql` | other | 2 |
| 85 | `0084_issue_recovery_actions.sql` | other | 2 |
| 86 | `0085_tranquil_the_executioner.sql` | other | 0 |
| 87 | `0086_routine_env_runtime_contract.sql` | other | 0 |
| 88 | `0087_backfill_environment_manage_human_defaults.sql` | other | 0 |
| 89 | `0088_backfill_principal_access_compatibility.sql` | other | 0 |
| 90 | `0089_cloud_upstreams.sql` | other | 4 |
| 91 | `0090_resource_memberships.sql` | other | 4 |
| 92 | `0091_old_swarm.sql` | other | 6 |
| 93 | `0092_mighty_puma.sql` | other | 1 |
| 94 | `0093_giant_green_goblin.sql` | other | 0 |
| 95 | `0094_backfill_archived_company_agent_pauses.sql` | other | 0 |
| 96 | `0095_issue_comment_tombstones.sql` | other | 0 |
| 97 | `0096_document_annotation_issue_comment_links.sql` | other | 0 |
| 98 | `0097_low_trust_source_trust.sql` | other | 0 |
| 99 | `0098_project_icon.sql` | other | 0 |
| 100 | `0099_skills_store_foundation.sql` | other | 6 |
| 101 | `0100_skill_install_count_backfill.sql` | other | 0 |
| 102 | `0101_plugin_company_id_tenant_isolation.sql` | other | 0 |
| 103 | `0102_managed_sandbox_dedup_index.sql` | new-index | 0 |
| 104 | `0103_agent_error_reason.sql` | other | 0 |
| 105 | `0104_issue_watchdogs.sql` | other | 2 |
| 106 | `0105_instance_scoped_environments.sql` | other | 0 |
| 107 | `0106_external_object_references.sql` | other | 4 |
| 108 | `0107_external_object_display_metadata.sql` | other | 0 |
| 109 | `0108_workspace_operations_issue_id.sql` | other | 0 |
| 110 | `0109_routine_description_annotations.sql` | other | 2 |
| 111 | `0110_document_company_cascade.sql` | other | 0 |
| 112 | `0111_backfill_skill_create_human_defaults.sql` | new-table | 0 |
| 113 | `0112_rename_skill_create_permission_key.sql` | new-table | 0 |
| 114 | `0113_pipeline_foundation.sql` | other | 18 |
| 115 | `0114_pipeline_case_issue_unlinked_event.sql` | other | 0 |
| 116 | `0115_pipeline_routine_origin.sql` | other | 0 |
| 117 | `0116_pipeline_upstream_drift_event.sql` | other | 0 |
| 118 | `0117_pipeline_transition_forced_event.sql` | other | 0 |
| 119 | `0118_pipeline_case_agent_fanout.sql` | other | 0 |
| 120 | `0119_pipeline_drift_acknowledged_event.sql` | other | 0 |
| 121 | `0120_pipeline_stage_working_primitives.sql` | other | 0 |
| 122 | `0121_pipeline_automation_retry_effects.sql` | other | 0 |
| 123 | `0122_pipeline_case_documents.sql` | other | 2 |
| 124 | `0123_document_annotation_source_trust.sql` | other | 0 |
| 125 | `0124_agent_api_key_scope_config.sql` | other | 0 |
| 126 | `0125_environment_custom_image_templates.sql` | other | 2 |
| 127 | `0127_environment_custom_images_instance_scoped.sql` | other | 0 |
| 128 | `0128_user_specific_secrets.sql` | other | 4 |
| 129 | `0129_agent_api_key_responsible_user.sql` | other | 0 |
| 130 | `0131_repair_run_responsible_user_context_refs.sql` | other | 0 |
| 131 | `0132_issue_comment_derived_attribution_fast.sql` | other | 0 |
| 132 | `0133_resource_membership_stars.sql` | other | 0 |
| 133 | `0134_run_responsible_user_invariant.sql` | other | 0 |
| 134 | `0135_repair_run_responsible_user_updated_at_sweep.sql` | other | 0 |
| 135 | `0136_acpx_default_engine_migration.sql` | other | 0 |
| 136 | `0137_skill_studio_server_foundation.sql` | other | 4 |
| 137 | `0138_skill_studio_run_retention.sql` | other | 0 |
| 138 | `0139_skill_studio_run_templates.sql` | other | 2 |
| 139 | `0140_built_in_managed_resources.sql` | other | 2 |
| 140 | `0141_heartbeat_runs_company_created_at_index.sql` | new-index | 0 |
| 141 | `0142_company_search_sort_indexes.sql` | new-index | 0 |
| 142 | `0143_cases_foundation.sql` | other | 12 |
| 143 | `0144_case_document_annotations.sql` | other | 0 |
| 144 | `0145_inbox_dismissal_snooze_kind.sql` | other | 0 |
| 145 | `0146_routine_activity_gate.sql` | other | 0 |
| 146 | `0147_cost_event_status.sql` | other | 0 |
| 147 | `0148_tool_access_mcp_connections.sql` | other | 10 |
| 148 | `0149_agent_access_phase2_contracts.sql` | other | 16 |
| 149 | `0150_tool_invocation_catalog_snapshots.sql` | other | 0 |
| 150 | `0151_tool_gateway_sessions.sql` | other | 2 |
| 151 | `0152_tool_connection_application_no_cascade.sql` | other | 0 |
| 152 | `0153_tool_stdio_command_templates.sql` | other | 2 |
| 153 | `0154_tool_oauth_states.sql` | other | 2 |
| 154 | `0155_tool_oauth_state_actor_binding.sql` | other | 0 |
| 155 | `0156_tool_oauth_state_session_binding.sql` | other | 0 |
| 156 | `0157_tool_profile_new_tools_review.sql` | other | 0 |
| 157 | `0158_tool_invocation_connected_mcp_metadata.sql` | other | 0 |
| 158 | `0159_named_mcp_gateways.sql` | other | 4 |
| 159 | `0160_mcp_gateway_contract_expansion.sql` | other | 0 |
| 160 | `0161_environment_custom_image_company_scope_repair.sql` | other | 0 |
| 161 | `0162_tool_runtime_metric_counters.sql` | other | 2 |
| 162 | `0163_secret_binding_projection_class.sql` | other | 0 |
| 163 | `0164_plugin_config_company_scope.sql` | other | 0 |
| 164 | `0165_connection_token_issuances.sql` | other | 2 |
| 165 | `0166_smoke_lab_results.sql` | other | 4 |
| 166 | `0167_environment_custom_image_instance_scope_cleanup.sql` | other | 0 |
| 167 | `0168_tool_connection_installs.sql` | other | 2 |
| 168 | `0169_tool_gateway_protocol_rate_limit_counters.sql` | other | 2 |
| 169 | `0170_company_skill_policies.sql` | other | 2 |
| 170 | `0171_issue_create_idempotency_keys.sql` | new-table | 2 |
| 171 | `0172_inbox_archive_agent_policies.sql` | other | 2 |
| 172 | `0173_inbox_policy_agent_cleanup.sql` | other | 0 |
| 173 | `0174_folders.sql` | other | 2 |
| 174 | `0175_nested_skill_folders.sql` | other | 0 |
| 175 | `0176_issue_create_idempotency_key_expiry.sql` | new-table | 0 |
| 176 | `0177_activity_log_responsible_user.sql` | other | 0 |
| 177 | `0178_summary_slots.sql` | other | 2 |
| 178 | `0179_summary_slot_failure_reason.sql` | other | 0 |
| 179 | `0180_decision_training_examples.sql` | other | 2 |
| 180 | `0181_decision_training_retention_policy.sql` | other | 0 |
| 181 | `0182_connections_v3_schema_core.sql` | other | 1 |
| 182 | `0183_connection_user_authorization_state.sql` | other | 0 |
| 183 | `0184_routable_blocked.sql` | other | 0 |
| 184 | `0185_status_cards.sql` | other | 4 |
| 185 | `0186_status_card_compile_provenance.sql` | other | 0 |
| 186 | `0187_status_card_pending_change_hash.sql` | other | 0 |
| 187 | `0188_status_card_generation_issue_index.sql` | new-index | 0 |
| 188 | `0189_status_card_agent.sql` | other | 0 |
| 189 | `0190_status_card_single_prompt.sql` | other | 0 |
| 190 | `0191_status_card_mentioned_issue_ids.sql` | other | 0 |
| 191 | `0192_task_watchdog_stop_snapshots.sql` | other | 0 |
| 192 | `0193_document_memberships.sql` | other | 2 |
| 193 | `0194_company_skill_releases.sql` | other | 0 |
| 194 | `0195_built_in_agent_unique_marker.sql` | other | 0 |
| 195 | `0196_drop_cloud_upstream_tables.sql` | deprecation | 0 |
| 196 | `0197_decisions_v1.sql` | other | 8 |
| 197 | `0198_issue_checkout_locks.sql` | other | 1 |
| 198 | `0199_team_installs.sql` | other | 2 |
| 199 | `0200_project_artifacts.sql` | other | 2 |
| 200 | `0201_issue_tree_holds_scope.sql` | other | 0 |
| 201 | `0202_company_assets.sql` | other | 2 |
| 202 | `0203_workspace_action_log.sql` | other | 2 |
| 203 | `0204_board_chat.sql` | other | 4 |
| 204 | `0205_smoke_lab_services.sql` | other | 6 |
| 205 | `0206_skill_configs.sql` | other | 2 |
| 206 | `0207_execution_lease.sql` | other | 2 |
| 207 | `0208_company_skills_soft_delete.sql` | other | 0 |

## Notes

- Initial migration is generated by drizzle from current schema
- Subsequent migrations are additive (new tables / columns / indexes)
- Deprecations are scheduled separately (no production drops yet)
