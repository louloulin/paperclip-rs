# R537 — pc-network-bind 新 crate（Node network-bind.ts 复刻）

> 时间：2026-08-11 · 状态：✅ 完成 + 35 测试通过 + clippy 干净 + fmt 干净

## 1. 目标

按 "高内聚低耦合" 原则，1:1 port `paperclip/packages/shared/src/network-bind.ts`
（约 100 LOC pure functions）到独立 Rust crate `pc-network-bind`。

## 2. 范围

| Node 上游 | Rust port | 说明 |
|---|---|---|
| `LOOPBACK_BIND_HOST` / `ALL_INTERFACES_BIND_HOST` | 同名 `pub const &str` | 1:1 常量 |
| `BindMode` (literal union) | `BindMode` enum (Loopback/Lan/Tailnet/Custom) | serde lowercase |
| `DeploymentMode` (literal union) | `DeploymentMode` enum (LocalTrusted/Authenticated) | serde snake_case |
| `DeploymentExposure` (literal union) | `DeploymentExposure` enum (Private/Public) | serde lowercase |
| `isLoopbackHost` | `is_loopback_host` | case-insensitive + trim |
| `isAllInterfacesHost` | `is_all_interfaces_host` | 同上 |
| `inferBindModeFromHost(host, opts?)` | `infer_bind_mode_from_host(host, opts)` | 5 路决策 |
| `validateConfiguredBindMode(input)` | `validate_configured_bind_mode(&input) -> Vec<String>` | 累积 errors |
| `resolveRuntimeBind(input)` | `resolve_runtime_bind(&input) -> ResolvedRuntimeBind` | 5 case match |

## 3. 关键设计决策

### 3.1 enum 替代 TS literal union

- `BindMode`, `DeploymentMode`, `DeploymentExposure` 全部 enum
- 编译期穷尽匹配 — 新增变体时编译器会强制所有 match arm 更新
- `Default` derive on `DeploymentMode::LocalTrusted` / `DeploymentExposure::Private`
  — 让 Input struct 可以 derive `Default`（TS 不需要 default）

### 3.2 `Copy` on 三个 Input struct

`InferBindModeOptions`, `ValidateConfiguredBindModeInput`, `ResolveRuntimeBindInput`
都只包含 `Option<&str>` 字段（合计 16 字节），全部 derive `Copy`。
clippy `trivially_copy_pass_by_ref` lint 强制传值（而非 `&`），与上游 Node 的
object spread 一致 — 不可变快照，不修改。

### 3.3 `Vec<String>` 错误累积

上游 `validateConfiguredBindMode` 返回 `string[]`，多个错误时全部累积。
Rust 版本保持同样语义 — 不在第一个错误时早退（与 `?` 操作符相反）。

### 3.4 switch → match arm

Node `switch (bind)` 用 exhaustive TS 模式匹配。Rust 用 `match bind` 强制穷尽
所有 4 个 variant，编译期保证无遗漏 case。

### 3.5 `is_some_and` for nullable host check

上游 JS 用 truthy check (`legacyHost && !isLoopbackHost(legacyHost) && ...`)。
Rust 用 `Option::is_some_and` 闭包 — 更声明式，无 unwrap 风险。

## 4. 验证（真实运行）

```
$ cargo test -p pc-network-bind
running 35 tests
test tests::r537_loopback_host_recognition ... ok
test tests::r537_loopback_host_negative ... ok
test tests::r537_all_interfaces_host_recognition ... ok
test tests::r537_all_interfaces_host_negative ... ok
test tests::r537_infer_empty_host_returns_loopback ... ok
test tests::r537_infer_loopback_hosts ... ok
test tests::r537_infer_lan_hosts ... ok
test tests::r537_infer_tailnet_when_matches ... ok
test tests::r537_infer_tailnet_when_no_match_returns_custom ... ok
test tests::r537_infer_unrecognized_returns_custom ... ok
test tests::r537_validate_local_trusted_requires_loopback ... ok
test tests::r537_validate_local_trusted_with_loopback_passes ... ok
test tests::r537_validate_custom_requires_custom_bind_host ... ok
test tests::r537_validate_custom_with_loopback_legacy_host_still_requires_custom ... ok
test tests::r537_validate_custom_with_non_loopback_legacy_host_passes ... ok
test tests::r537_validate_custom_with_explicit_custom_bind_host_passes ... ok
test tests::r537_validate_authenticated_public_tailnet_rejected ... ok
test tests::r537_validate_authenticated_private_tailnet_passes ... ok
test tests::r537_validate_infers_bind_from_host ... ok
test tests::r537_validate_accumulates_multiple_errors ... ok
test tests::r537_resolve_loopback_default ... ok
test tests::r537_resolve_loopback_explicit ... ok
test tests::r537_resolve_lan_explicit ... ok
test tests::r537_resolve_lan_inferred_from_host ... ok
test tests::r537_resolve_custom_with_explicit_host ... ok
test tests::r537_resolve_custom_missing_custom_bind_host ... ok
test tests::r537_resolve_custom_falls_back_to_legacy_non_loopback_host ... ok
test tests::r537_resolve_tailnet_with_bind_host ... ok
test tests::r537_resolve_tailnet_missing_bind_host ... ok
test tests::r537_resolve_tailnet_no_bind_host_no_legacy ... ok
test tests::r537_resolve_infers_from_host_when_no_bind ... ok
test tests::r537_bind_mode_as_str ... ok
test tests::r537_deployment_mode_as_str ... ok
test tests::r537_deployment_exposure_as_str ... ok
test tests::r537_bind_mode_serialization ... ok

test result: ok. 35 passed; 0 failed

$ cargo clippy -p pc-network-bind -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.16s

$ cargo fmt -p pc-network-bind -- --check
(no diff — clean)
```

## 5. 覆盖矩阵（vs Node 行为）

### `inferBindModeFromHost` 决策树 (5 路)

| 路径 | Node | Rust |
|---|---|---|
| 空 / missing host | `Loopback` | `Loopback` ✅ |
| Loopback host (`127.0.0.1` / `localhost` / `::1`) | `Loopback` | `Loopback` ✅ |
| All-interfaces (`0.0.0.0` / `::`) | `Lan` | `Lan` ✅ |
| 匹配 `tailnet_bind_host` | `Tailnet` | `Tailnet` ✅ |
| 其它 | `Custom` | `Custom` ✅ |

### `validateConfiguredBindMode` 错误规则 (3 条)

| 规则 | Node 错误消息 | Rust 错误消息 |
|---|---|---|
| `local_trusted` + `bind != loopback` | "local_trusted requires server.bind=loopback" | 一致 ✅ |
| `bind=custom` + 无 `customBindHost` + legacy host 是 loopback/all-interfaces/None | "server.customBindHost is required when server.bind=custom" | 一致 ✅ |
| `authenticated` + `public` + `tailnet` | "server.bind=tailnet is only supported for authenticated/private deployments" | 一致 ✅ |

### `resolveRuntimeBind` 5 个 case

| Case | Node 行为 | Rust 行为 |
|---|---|---|
| Loopback | host = `127.0.0.1`, errors = [] | 一致 ✅ |
| Lan | host = `0.0.0.0`, errors = [] | 一致 ✅ |
| Custom + 有 customBindHost | host = custom, errors = [] | 一致 ✅ |
| Custom + 无 customBindHost | host = legacy host → fallback `127.0.0.1`, errors = ["customBindHost is required"] | 一致 ✅ |
| Tailnet + 有 bindHost | host = bindHost, errors = [] | 一致 ✅ |
| Tailnet + 无 bindHost | host = legacy → fallback `127.0.0.1`, errors = ["Tailscale or PAPERCLIP_TAILNET_BIND_HOST required"] | 一致 ✅ |

## 6. 文件清单

```
crates/pc-network-bind/
├── Cargo.toml      (8 行：name + workspace deps + serde + serde_json)
└── src/
    └── lib.rs      (~480 行 + 35 测试 = 777 行)
```

新增 workspace members：
- `crates/pc-network-bind`

workspace crates **79 → 80**

## 7. 累计进度（R534-R537 四连）

| 轮次 | Crate | Node LOC | Rust LOC | 测试数 |
|---|---|---|---|---|
| R534 | `pc-environment-support` | ~170 | ~600 | 31 |
| R535 | `pc-environment-redaction` | ~115 | ~520 | 28 |
| R536 | `pc-portability-hash` | ~30 | ~150 | 26 |
| R537 | `pc-network-bind` | ~100 | ~480 | 35 |
| **累计** | **4 个新 crate** | **~415** | **~1750** | **+120** |

workspace crates **76 → 80** (+4)

## 8. R538 候选

继续 port Node `packages/shared/` 纯函数模块：
1. `packages/shared/src/agent-eligibility.ts` — ~150 LOC，agent invokability 检查
2. `packages/shared/src/document-anchors.ts` — ~200 LOC，markdown anchor 投影
3. `packages/shared/src/pipeline-health.ts` — ~370 LOC，pipeline setup-health warnings
4. `packages/shared/src/frontmatter.ts` — ~640 LOC，YAML frontmatter 解析（最大单文件）
