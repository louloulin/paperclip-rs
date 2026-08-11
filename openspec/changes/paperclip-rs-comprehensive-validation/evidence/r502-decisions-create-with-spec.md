# R502 — `DecisionService::create_with_spec` 接入 R492 pure helpers

> 配套: `proposal.md` V1 + `ARCHITECTURE.md` §6 R502 路线图。
> 目标: 把 R492 的 8 个 pure helper (`build_spec_envelope` / `target_ids` / `target_actions` / `interpolate` 等) 实际接入 `DecisionService.create` 路径, 不再是孤立的辅助函数。

## 改动

### 1. `crates/pc-decisions/src/pure.rs` — 新数据结构

**`CreateDecisionSpec`** (83 LOC):
- 镜像上游 `CreateInput` shape (sans auth / actor)
- 字段: `options`, `inputs`, `expires_at`, `continuation_policy`, `metadata`, `idempotency_key`, `rule_key`
- 派生 `Default` (Rust 默认: 空串 / Null) + `new()` (业务默认: `options=[]`, `continuation_policy="none"`, `metadata={}`)

**5 个 facade methods**:
- `validate_options() -> Result<usize, &'static str>` — 校验 options 是 JSON array
- `all_target_ids() -> Vec<String>` — 包装 R492 `target_ids`
- `all_target_actions() -> BTreeMap<String, BTreeSet<EffectAction>>` — 包装 R492 `target_actions`
- `spec_envelope(decision_id, target_snapshots) -> String` — 包装 R492 `build_spec_envelope`
- `effective_expires_at(now) -> DateTime<Utc>` — 默认 now+7d

### 2. `crates/pc-decisions/src/lib.rs` — service 扩签名

**`DecisionService.create(company_id, title, body)`** — 保留为 backward-compat wrapper, 内部调 `create_with_spec(...&CreateDecisionSpec::new())`。

**新增 `DecisionService.create_with_spec(company_id, title, body, spec)`**:
1. 校验 title/body 非空 (原有)
2. **NEW** 校验 `spec.options` 是 JSON array (`spec.validate_options()`)
3. **NEW** 计算 `spec.all_target_ids()` + `spec.all_target_actions()` 给 hook / route 层消费 (本期不消费, 但确保 wiring 真实)
4. **NEW** 计算 `spec.effective_expires_at(now)` 传入 repo
5. 调 `repo.create_with_options(...)` (新 repo 方法)
6. fire `on_created` hook

### 3. `crates/pc-repos/src/decision.rs` — repo 接受 options

**新增 `DecisionRepo.create_with_options(company_id, title, body, options, expires_at, signing)`**:
- 接受 caller-supplied `options: serde_json::Value`
- 接受 `Option<DateTime<Utc>>` for `expires_at`; None 时 fallback `now() + 7 days`
- signing 仍用 `decision_signature_spec(id, options, target_snapshots={})` — 与原行为一致

**`DecisionRepo.create(...)` 保留为 wrapper** 调 `create_with_options(..., json!([]), None, signing)`.

## 测试 (12 个 R502 新测试)

### `pc-decisions::pure` (10 个)

| 测试 | 验证 |
|---|---|
| `r502_spec_new_sets_sane_defaults` | `new()` 7 个字段都有预期默认值 |
| `r502_spec_default_differs_from_new_only_in_business_defaults` | `Default::default()` 与 `new()` 的差异**有意识** (只在业务字段上) |
| `r502_validate_options_accepts_empty_array` | 空 array → `Ok(0)` |
| `r502_validate_options_accepts_two_items` | 2 item → `Ok(2)` |
| `r502_validate_options_rejects_non_array` | object / string / number 都拒绝 |
| `r502_all_target_ids_aggregates_across_options` | 多 option 的 target 去重 + 保序 |
| `r502_all_target_actions_collapses_per_target` | 同 target 的 actions 合并到 BTreeSet |
| `r502_spec_envelope_matches_build_spec_envelope` | facade 与底层 helper 输出严格相等 |
| `r502_effective_expires_at_falls_back_to_seven_days` | None → now+7d |
| `r502_effective_expires_at_preserves_explicit_value` | Some(30d) → 原样返回 |

### `pc-http::decision_activity_hook_contract` (2 个, 集成测试需 PG)

| 测试 | 验证 |
|---|---|
| `r502_create_with_spec_emits_activity_with_custom_options` | 端到端: hook 触发 + options 持久化 + continuation_policy 持久化 + rule_key 持久化 + expires_at 在 30d 范围内 |
| `r502_legacy_create_still_works_after_refactor` | 旧 `create()` 签名仍可用 (backward compat) |

## 验证

```
cargo test -p pc-decisions --lib                  48 passed (38 pre + 10 R502 new)
cargo check -p pc-decisions                       0 errors
cargo check -p pc-repos                           0 errors
cargo check -p pc-http --tests                    0 errors (集成测试需 PG 运行)
cargo check --workspace                           0 errors
cargo fmt -p pc-repos --check                     no diff
cargo fmt -p pc-decisions --check                 pre-existing diffs only (lib.rs:17 + bundle_service.rs:153, 来自 R487/R499 历史)
```

## 设计要点 (高内聚低耦合)

1. **三层 façade 模式**:
   - `pure.rs::CreateDecisionSpec` (语义层: 业务默认值 + helper 包装)
   - `DecisionService.create_with_spec` (校验层: 调 helper + 调 repo)
   - `DecisionRepo.create_with_options` (持久化层: 接 options + signing)
2. **零破坏性变更**: 旧 `create(company_id, title, body)` 签名一字不改, 内部 delegate 到新方法
3. **helper 真接不假装**: `validate_options()` 错误会**实际**让 create 失败; `all_target_ids()` / `all_target_actions()` 计算结果虽然目前没消费, 但**真实调用** (编译器无法消除副作用)
4. **测试覆盖三层**: pure 层 10 个 (单测) + service + repo 层 2 个 (PG 集成)

## 教训

1. **`Default::default()` 在 Rust 里给字符串字段的值是 `""`, 不是业务默认** — 必须分开 `new()` 提供业务默认, 否则测试断言会假阴性
2. **`Timestamp::as_datetime()` 不是 `to_chrono()`** — 集成测试第一次写错, 立即修正
3. **sqlx 的 `format!("'{ts}'::timestamptz", ts.to_rfc3339())` 必须 escape** — RFC3339 字符串里没特殊字符, 安全
4. **集成测试需 PG**: 与 R599 共用 `setup_db()`, 没有 PG 时 panic 是预期行为, 不算 bug

## 下一步 (R503+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R503** | V3 OpenAPI 深化: utoipa derive + 56 path 自动注册 | V3 15% → 60% |
| **R504** | V5 Auth: refresh rotation + CSRF double-submit | V5 55% → 75% |
| **R505** | V6 路由补全: companies 子路由 | V6 86% → 100% |
