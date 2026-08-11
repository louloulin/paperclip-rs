# R572 — R-INTEGRATION-12: pc-responsible-user-denial-copy → pc-responsible-user-denial

**状态**: ✅ 完成 (2026-08-12)

## 1. 目标

将 R558 创建的 `pc-responsible-user-denial-copy` crate（authz 层 denial 的
用户可见文案 + 类型守卫）接入 `pc-responsible-user-denial`，让 server-side
middleware / error handler 通过一个 crate 入口同时拿到：

- **Copy-side 代码常量 + 文案渲染**（"RESPONSIBLE_USER_UNAUTHORIZED" /
  "RESPONSIBLE_USER_UNAVAILABLE" —— Node `server/src/middleware/auth.ts`
  实际发出的代码）
- **Run-outcome 分类 + 持久化**（"rate_limited" / "not_entitled" /
  "quota_exceeded" / "unsupported_channel" / "other" —— run_outcomes 子模块）

两个域完全不同：前者是 authz 层的 copy contract，后者是 run-outcome 端的
分类。R572 把前者作为 `pc-responsible-user-denial::copy` 子模块挂入主 crate。

## 2. 设计挑战

两个 crate 服务**不同领域**：

| 域 | crate | 代码 | Node 来源 |
|---|---|---|---|
| **Authz copy** | pc-responsible-user-denial-copy | `RESPONSIBLE_USER_UNAUTHORIZED` / `RESPONSIBLE_USER_UNAVAILABLE` | `server/src/middleware/auth.ts:364` 等 |
| **Run-outcome 分类** | pc-responsible-user-denial | `rate_limited` / `unsupported_channel` / `quota_exceeded` / `not_entitled` / `other` | `server/src/services/responsible-user-denial-run-outcomes.ts` |

集成方案：
1. `pc-responsible-user-denial` 把 `pc-responsible-user-denial-copy` 加入
   `[dependencies]`（物理依赖，无代码重复）。
2. 新增 `pc_responsible_user_denial::copy` 子模块，**纯 re-export + 一个
   `render_responsible_user_denial_copy` 桥接 helper**。
3. 顶层 `is_responsible_user_denial_code` re-export **保持不变**——仍指向
   run-outcome 域的 `is_valid_code`，避免破坏 R558/R706 的向后兼容别名。

## 3. 集成实现

### 3.1 新增依赖

```toml
# crates/pc-responsible-user-denial/Cargo.toml
pc-responsible-user-denial-copy = { path = "../pc-responsible-user-denial-copy" }
```

### 3.2 新增 copy 子模块（crates/pc-responsible-user-denial/src/copy.rs, 125 LOC）

```rust
pub use pc_responsible_user_denial_copy::{
    describe_responsible_user_denial, is_responsible_user_denial_code,
    responsible_user_label, ResponsibleUserDenialCode, ResponsibleUserDenialCopy,
    ResponsibleUserDenialOptions, ResponsibleUserDenialTone, RESPONSIBLE_USER_DENIAL_CODES,
};

/// Server-side convenience: parse + render in one call.
/// Returns None when code is not a known copy-side denial.
pub fn render_responsible_user_denial_copy(
    code: &str,
    user_name: Option<&str>,
) -> Option<ResponsibleUserDenialCopy> { ... }
```

### 3.3 lib.rs 暴露

```rust
// crates/pc-responsible-user-denial/src/lib.rs
pub mod copy;
pub mod run_outcomes;

mod codes;
pub use codes::{
    is_valid_code, normalize_responsible_user_denial_code,
    normalize_responsible_user_denial_code_value, ResponsibleUserDenialCode,
};

// 向后兼容：顶层 alias 仍指向 run-outcome 域
pub use codes::is_valid_code as is_responsible_user_denial_code;
```

## 4. 测试

### 4.1 lib 单元测试（新增 5 个 in `copy.rs`）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `constants_re_exported_match_canonical` | 常量 re-export 字节级一致 |
| 2 | `is_responsible_user_denial_code_gates_copy_codes` | 只接受 copy-side 大写代码 |
| 3 | `render_unauthorized_with_name_produces_copy` | 已知 name 渲染完整文案 |
| 4 | `render_unavailable_without_name_falls_back` | 缺失 name 回退到通用短语 |
| 5 | `render_returns_none_for_run_outcome_codes` | run-outcome code 返回 None |

### 4.2 集成测试（crates/pc-responsible-user-denial/tests/r572_*.rs, 11 个）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r572_constants_match_copy_crate_byte_for_byte` | re-export 与直接导入一致 |
| 2 | `r572_copy_gate_only_accepts_copy_codes` | copy 域守卫正确 |
| 3 | `r572_top_level_alias_is_run_outcome_domain` | 顶层 alias 仍指向 run-outcome 域 |
| 4 | `r572_render_unauthorized_with_known_name` | Unauthorized + 已知 name |
| 5 | `r572_render_unauthorized_with_blank_name_falls_back` | Unauthorized + 空白/None name |
| 6 | `r572_render_unavailable_with_name` | Unavailable + 已知 name |
| 7 | `r572_render_rejects_run_outcome_codes` | render 对 run-outcome code 返回 None |
| 8 | `r572_two_codes_produce_distinct_copy` | 两个 code 输出视觉可区分 |
| 9 | `r572_delegation_matches_direct_copy_call` | bridge 与直接调用字节级一致 |
| 10 | `r572_label_helper_consistent` | `responsible_user_label` 行为一致 |
| 11 | `r572_copy_module_path_re_exports_resolve` | 命名空间路径可达 |

### 4.3 测试统计

```
$ cargo test -p pc-responsible-user-denial
lib tests:           13 passed (8 pre + 5 new copy) ⭐
e2e_run_outcomes:     9 passed (无回归)
r572_integration:    11 passed (新) ⭐
doc-tests:            0
TOTAL:               33 passed / 0 failed

$ cargo test -p pc-responsible-user-denial-copy
lib + r558 tests:    18 passed (无回归)
```

## 5. 无回归验证

```bash
$ cargo test -p pc-responsible-user-denial-copy 2>&1 | grep "test result:"
test result: ok. 18 passed; 0 failed   # 5 lib + 13 r558
```

零回归。

## 6. 设计亮点

### 6.1 两域严格分离

R572 **不**把 copy 域的代码合并到 run-outcome 域的枚举里（虽然它们都叫
"responsible-user-denial"）。原因：

- authz 层用大写蛇形 + 业务术语（"UNAUTHORIZED" / "UNAVAILABLE"）—— 文案
  是给最终用户读的；
- run-outcome 层用小写蛇形 + 技术术语（"rate_limited" / "quota_exceeded"）
  —— 分类是给持久化 + dashboard 用的。

如果合并，enum 变体名要选一个，导致另一边的字符串不一致；server-side
middleware 也要分两段 match。

### 6.2 单点 bridge helper

`render_responsible_user_denial_copy(code, user_name)` 把"parse + 包装
options + 调用 describe"压成一个调用。server 端 middleware 只要拿到
`ApiError.code` 字符串，就能一行代码产出用户可见文案，无需关心
`ResponsibleUserDenialOptions` 结构。

### 6.3 向后兼容别名

顶层 `is_responsible_user_denial_code` **仍然指向 run-outcome 域的
`is_valid_code`**。这保留了 R706 建立的对外 API：
- 调用方写 `pc_responsible_user_denial::is_responsible_user_denial_code("rate_limited")`
  仍然返回 `true`（run-outcome gate）。
- 调用方写 `pc_responsible_user_denial::copy::is_responsible_user_denial_code("RESPONSIBLE_USER_UNAUTHORIZED")`
  返回 `true`（copy gate）。

两个 `is_responsible_user_denial_code` 函数同名但服务不同域，靠模块路径
消歧义。

## 7. 累计 R-INTEGRATION 进度

| # | Crate A | → | Crate B | Round |
|---|---|---|---|---|
| 1 | pc-config-schema | → | pc-config | R-INTEGRATION-1 |
| 2 | pc-mentions | → | pc-issues | R-INTEGRATION-2 |
| 3 | pc-pipeline-case-type | → | pc-pipelines | R-INTEGRATION-3 |
| 4 | pc-adapter-type | → | pc-* adapters | R-INTEGRATION-4 |
| 5 | pc-portability-fidelity | → | pc-portability | R-INTEGRATION-5 |
| 6 | pc-execution-workspace-guards | → | pc-http | R566 |
| 7 | pc-external-objects | → | pc-issue-references | R567 |
| 8 | pc-app-definitions | → | pc-http | R568 |
| 9 | pc-trust-policy | → | pc-authz | R569 |
| 10 | pc-workspace-commands | → | pc-cli | R570 |
| 11 | pc-api-routes | → | pc-http | R571 |
| **12** | **pc-responsible-user-denial-copy** | → | **pc-responsible-user-denial** | **R572** ⭐ |

**12/12 = 100%** ✅ ✅ ✅

R-INTEGRATION 全部完成。下一步切换到 V1-V15 硬目标。

## 8. 下一步

R-INTEGRATION 已 100% 完成，下一轮切换到硬目标（按 R534 gap analysis 排序）：

| 优先级 | 目标 | 状态 |
|---|---|---|
| **V1** | e2e baseline 真实启动 + 验证 | 当前阻塞于 pc-server 慢启动 |
| **V11** | UI 60 client happy path 真实验证 | 0% |
| **V12** | Playwright 真实 UI 剧本 | 0% |
| **V6** | 路由字节级补全（companies 子路由 + /api/admin/*） | 部分 |
| **V8** | 远程 execution（restoreRemoteWorkspace 等） | 0% |
| **G5/G6** | claude-local/codex-local 远程路径补全 | 部分 |
| **G11** | Companies subroute DELETE + /api/admin/* routes | 0% |
