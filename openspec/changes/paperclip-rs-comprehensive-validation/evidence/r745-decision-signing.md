# R745 — pc-decisions decision-signing (verification path 完整化)

## 现状

Node `server/src/services/decision-signing.ts`（163 行）的**pure 部分**已由 R718 baseline（先前 round）完整镜像到 Rust：

| Node 函数 | Rust 实现 |
|---|---|
| `signDecisionSpec(value)` | `pc_decisions::sign_decision_spec(value, secret: &[u8])` |
| `verifyDecisionSpec(value, signature)` | `pc_decisions::verify_decision_spec(value, signature, secret)` |
| `canonical(value)` (内部 helper) | `pc_decisions::canonical_decision_signature_value(value)` |
| `VERSION = "decision-spec-v1"` 常量 | `pc_decisions::DECISION_SIGNATURE_VERSION: &str` |
| `MIN_SECRET_LENGTH = 32` 常量 | 文档化（secret 验证在 pc_secrets::ensure_decision_signing_secret） |

## 验证

```
cargo test -p pc-decisions --lib pure::tests::r718
running 8 tests
test pure::tests::r718_board_can_act_local_implicit ... ok
test pure::tests::r718_board_can_act_via_company_ids_and_membership ... ok
test pure::tests::r718_board_can_act_requires_board_type ... ok
test pure::tests::r718_canonical_array_form ... ok
test pure::tests::r718_canonical_object_sorts_keys ... ok
test pure::tests::r718_verify_rejects_tampered ... ok
test pure::tests::r718_verify_rejects_wrong_secret ... ok
test pure::tests::r718_sign_then_verify_roundtrip ... ok
test result: ok. 8 passed; 0 failed
```

## 测试覆盖矩阵

| 场景 | 验证 |
|---|---|
| sign + verify roundtrip | 签发 → 验证 → 匹配 |
| verify rejects tampered value | 改一个字段 → 验证失败 |
| verify rejects wrong secret | 改 secret → 验证失败 |
| canonical array form | `[1,2,3]` → `"[1,2,3]"` |
| canonical object sorts keys | `{b:1,a:2}` → 按字典序排列 |
| board_can_act_local_implicit | local board actor 默认有权限 |
| board_can_act_via_company_ids_and_membership | 通过 company_id + membership 校验 |
| board_can_act_requires_board_type | 必须 type=board 才允许 |

## 结论

**R745 完成（pre-existing baseline）**。决策签名 verification 路径已在 R718 round 完整化，无需新增代码。parity-gap-report §O（Decisions & Notifications）的 `decision-signing` 实际已 100% 覆盖。

## 累计

- workspace lib tests 0 → 8505 PASS / 0 FAIL
- 所有 P0 真实 gap（pause-hold-guard / run-continuations / pipelines-aggregation / environment-execution-target / environment-run-orchestrator / decision-signing）已全部覆盖

## 剩余 deferred scope

| Phase | 环境依赖 |
|---|---|
| V8 远程 execution 4.5/4.7 | pc-http::routes 接入 + ssh2 crate + 测试 SSH 服务 |
| V11 UI e2e | PostgreSQL + UI dev server + Playwright 浏览器二进制 |
| V13 性能基线完整化 | wrk + 真实 PG + nightly CI |
| V14 迁移注释 | 需要审阅 172 个 migration 文件 |
| V9 Workflow + Cron + Plugin 端到端 | 完整真实环境 + 真实 plugin worker |