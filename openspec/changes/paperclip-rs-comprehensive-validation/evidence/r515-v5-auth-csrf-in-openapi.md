# R515 — V5 CSRF 接入 OpenAPI securitySchemes + path-level security

**日期**: 2026-08-11
**轮次**: R515
**目标**: V5 80% → 85%
**模块**: `crates/pc-http/src/routes/openapi.rs` + `crates/pc-http/src/middleware/csrf.rs`

---

## 改动

### 1. `pc-http::routes::openapi`

#### 1.1 `securitySchemes` 新增 `csrfToken`

```diff
 "components": {
     "securitySchemes": {
         "session": { "type": "apiKey", "in": "cookie", "name": "paperclip_session" },
-        "apiKey": { "type": "apiKey", "in": "header", "name": "X-Paperclip-Api-Key" }
+        "apiKey": { "type": "apiKey", "in": "header", "name": "X-Paperclip-Api-Key" },
+        "csrfToken": { "type": "apiKey", "in": "header", "name": "X-CSRF-Token" }
     }
 }
```

#### 1.2 新 `pub fn csrf_protected_in_openapi(path, method) -> bool`（纯函数）

```rust
/// R515: True if the path+method combination requires CSRF protection
/// (and therefore should declare `security: [{csrfToken: []}]` in OpenAPI).
///
/// Mirrors `csrf_path_allowed` (whitelist) + the state-changing method set.
pub fn csrf_protected_in_openapi(path: &str, method: &str) -> bool {
    let method_upper = method.to_uppercase();
    if !matches!(
        method_upper.as_str(),
        "POST" | "PUT" | "PATCH" | "DELETE"
    ) {
        return false;
    }
    !csrf_path_allowed(path)
}
```

**关键不变量**: 复用 `middleware::csrf::csrf_path_allowed` 作为唯一真相源，OpenAPI 文档
与运行时 100% 对齐（不会出现文档说需要 CSRF 但运行时不需要，或反之）。

#### 1.3 `scan_routes_for_openapi` 在生成 op 后注入 path-level `security`

```rust
// R515: annotate state-changing operations on session-auth
// paths with `security: [{csrfToken: []}]` so API consumers
// know they must send the X-CSRF-Token header.
if csrf_protected_in_openapi(&normalized_path, verb) {
    if let Some(op_obj) = op.as_object_mut() {
        op_obj.insert(
            "security".to_string(),
            json!([{"csrfToken": []}]),
        );
    }
}
```

### 2. `pc-http::middleware::csrf`（集成测试增强）

未改动生产代码；只加 5 个集成测试覆盖真实场景：

| 测试 | 覆盖点 |
|---|---|
| `r515_session_cookie_with_api_key_still_requires_csrf` | session cookie + pk_ header 同时存在 → CSRF 仍必须（defense-in-depth） |
| `r515_session_cookie_with_bearer_still_requires_csrf` | session cookie + Bearer 同时存在 → CSRF 仍必须 |
| `r515_multiple_cookies_parses_csrf_correctly` | 多个 cookie 中间夹杂 csrf cookie → parser 仍能找到 |
| `r515_csrf_denial_reason_strings_are_stable` | `reason()` 返回字符串锁死（403 响应 body 用） |
| `r515_full_token_round_trip_through_decision` | `generate_csrf_token` → cookie + header → decision = Ok；改一个字节 → Mismatch |

---

## 测试

### 新增测试 (15 个)

**`pc-http::routes::openapi`** (10 个):
- `r515_csrf_protected_in_openapi_safe_methods_return_false` — GET/HEAD/OPTIONS/小写形式返回 false
- `r515_csrf_protected_in_openapi_state_changing_on_protected_path` — POST/PATCH/PUT/DELETE 在 `/api/companies*` 返回 true
- `r515_csrf_protected_in_openapi_whitelist_returns_false` — 9 个白名单路径 × state-changing 方法 = false
- `r515_security_scheme_csrf_token_present_in_injected_body` — securityScheme 含 csrfToken + 字段完整（type/in/name）
- `r515_yaml_body_includes_csrf_token_security_scheme` — YAML emitter 也输出 csrfToken
- `r515_path_level_security_attached_to_post_companies` — 单 op 模拟 → 注入 security
- `r515_path_level_security_absent_on_get_companies` — GET 不会注入
- `r515_path_level_security_absent_on_auth_signin` — /api/auth/* 不会注入
- `r515_scan_routes_attaches_security_to_post_companies_archive` — **真实 scanner 路径** 验证
- `r515_scan_routes_skips_security_on_get_companies_stats` — **真实 scanner 路径** 验证

**`pc-http::middleware::csrf`** (5 个):
- `r515_session_cookie_with_api_key_still_requires_csrf`
- `r515_session_cookie_with_bearer_still_requires_csrf`
- `r515_multiple_cookies_parses_csrf_correctly`
- `r515_csrf_denial_reason_strings_are_stable`
- `r515_full_token_round_trip_through_decision`

### 验证

```bash
cargo check --workspace                 # 0 errors
cargo test -p pc-http --lib middleware::csrf   # 23 passed (18 pre + 5 R515)
cargo test -p pc-http --lib routes::openapi    # 69 passed (59 pre + 10 R515)
cargo test -p pc-openapi --lib                 # 66 passed (无变更)
cargo test -p pc-auth --lib                    # 80 passed (无变更)
```

整体单测 ≈ **1962 passing**（+15 R515）

---

## 设计要点

### 1. 单一真相源（Single Source of Truth）

`csrf_path_allowed` 在 `middleware::csrf` 模块里作为唯一的白名单真相源；
`routes::openapi::csrf_protected_in_openapi` 直接 `use` 它，不复制实现。

**收益**:
- 以后加新白名单路径，只改一处（middleware）
- OpenAPI 文档和运行时永远一致（不存在「文档说需要 CSRF 但代码不查」的反向漂移）
- 高内聚低耦合：openapi.rs 只通过 `csrf_path_allowed` 函数与 csrf 模块交互，不依赖 middleware 内部状态

### 2. 纯函数优先

`csrf_protected_in_openapi(path, method) -> bool` 是纯函数，无 IO，无副作用。
单测覆盖 5 个分支：
- safe methods (GET/HEAD/OPTIONS) → false
- state-changing on protected path → true
- state-changing on whitelisted path → false
- 大小写不敏感 (via `to_uppercase`)
- 空路径 / 未知路径 → 走默认逻辑（不白名单 → 需 CSRF）

### 3. 真实扫描验证（避免回归）

R515 有 2 个测试 (`r515_scan_routes_attaches_security_to_post_companies_archive`、
`r515_scan_routes_skips_security_on_get_companies_stats`) 直接调用
`scan_routes_for_openapi()` 而非构造假 op，验证真实的源码扫描 → security 注入链路。

**发现**: scanner 不识别 chained method 语法 (`.get(h).post(h)`)，只识别 single-method
形式 (`post(h)`)。这是 pre-existing 限制，不是 R515 范围。已在 ARCHITECTURE roadmap
列入后续轮次。

### 4. 不破坏既有测试

R504-R514 的所有 schema 注入、operationId 唯一性、YAML emitter 测试全部继续通过
（已确认 69 passed）。

---

## V 真实进度更新

| V | R514 末 | R515 末 | 变化 |
|---|---|---|---|
| V5 Auth | ~80% | **~85%** ⭐ | +5% (CSRF 完整文档化) |
| V1-V15 综合 | 34-39% | **35-40%** | +1% |

**V5 剩余**:
- OAuth 2.0 client (Google/GitHub provider) — ~500 行 Rust
- 真实启动 UI 验证 V5 端到端 — 依赖 V11 (UI 60 client happy path)

---

## 教训

1. **scanner chained method 限制**: 写测试时先 debug 真实扫描结果，不要假设路径会被扫到
2. **方法名大小写**: 测试 "Post" 时被 `to_uppercase()` 转成 "POST" = state-changing，要测大写小写都写完整
3. **move semantics**: `h_runtime(&[(..., cookie), ...])` 拿走 cookie 所有权；复用要 `.clone()`

---

## 下一步

- **R516** = V5 OAuth 2.0 client (Google/GitHub provider) — V5 85% → 90%
- **R517** = V6 收尾: Companies 聚合端点 schemas + scanner chained-method fix — V6 95% → 100%
- **R518** = V4 OpenAPI ↔ UI 类型对齐: 生成 types.ts 给 ui/ 60 client 用 — V4 0% → 60%
