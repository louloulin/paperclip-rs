# R661 - Agent JWT 真实 E2E + pc-agent-jwt 新 crate

## 目标

复刻 Node server/src/agent-auth-jwt.ts (249 行) + 中间件集成路径：
- pc-agent-jwt: 完整 HS256 JWT 实现（per-company/per-instance signing key 派生）
- pc-auth: 在 resolve_auth_from_headers 中加入 Agent JWT 验证路径

## 实现

### 1. 新 crate: pc-agent-jwt (557 行)

crates/pc-agent-jwt/src/lib.rs

API 与 Node agent-auth-jwt.ts 1:1 对应：
- LocalAgentJwtClaims: JWT claim 结构
- JwtConfig::from_env / from_env_with: env-only 配置读取
- derive_company_signing_key: HMAC-SHA256(master, jwt:{instance}:{company}) -> hex
- create_local_agent_jwt / verify_local_agent_jwt

隔离属性（PAP-12896 / PAP-12899）：
- Per-company: companyA 的 token 不能重放到 companyB
- Per-instance: worktree/fork instance 颁发的 token 不能跨 control-plane 重放
- Legacy master-secret fallback 通过 PAPERCLIP_AGENT_JWT_DISABLE_LEGACY_FALLBACK 控制

### 2. pc-auth 集成

crates/pc-auth/src/lib.rs
- 新增 verify_agent_jwt_actor(db, token) -> Option<AuthContext>
- Bearer token 路径新增：API key 失败 -> session 失败 -> Agent JWT 验证 -> DB 查 agent -> company_id/status 校验
- terminated/pending_approval agent 直接拒绝

### 3. R661 真实 PG E2E

crates/pc-auth/tests/r661_agent_jwt_real_pg.rs (199 行)

测试 1: r661_resolve_auth_accepts_agent_jwt_for_active_agent
- 真实 PG 插入 company + agent
- 用 pc_agent_jwt 颁发 JWT
- 构造 Authorization Bearer header
- resolve_auth_from_headers 验证
- 断言 Actor::Agent + ActorSource::AgentJwt + company_id/run_id 匹配

测试 2: r661_resolve_auth_rejects_token_from_other_instance
- 用 instance_id=fork-instance-99 颁发 JWT
- 在 default instance 上 resolve_auth
- 断言 token 被拒绝（PAP-12896 fork-token 隔离）

## 真实结果

running 2 tests
R661 minted token len=440
R661 resolved actor source=AgentJwt, method=agent_jwt
R661 PASS: Agent JWT resolved to Actor::Agent via real PG
test r661_resolve_auth_accepts_agent_jwt_for_active_agent ... ok
R661 fork-token rejected with error
R661 PASS: fork-instance JWT rejected (PAP-12896)
test r661_resolve_auth_rejects_token_from_other_instance ... ok

test result: ok. 2 passed; 0 failed; finished in 0.12s

## pc-agent-jwt 单元测试

running 17 tests
test tests::base64_roundtrip ... ok
test tests::create_then_verify_roundtrip ... ok
test tests::derive_company_signing_key_is_deterministic ... ok
test tests::derive_company_signing_key_isolates_by_company ... ok
test tests::derive_company_signing_key_isolates_by_instance ... ok
test tests::jwt_config_from_env_reads_better_auth_secret_fallback ... ok
test tests::jwt_config_from_env_reads_paperclip_secret ... ok
test tests::jwt_config_from_env_returns_none_when_secret_missing ... ok
test tests::legacy_fallback_accepts_old_token ... ok
test tests::legacy_fallback_can_be_disabled ... ok
test tests::safe_compare_is_timing_safe_basic ... ok
test tests::verify_rejects_cross_company_token ... ok
test tests::verify_rejects_empty_token ... ok
test tests::verify_rejects_expired_token ... ok
test tests::verify_rejects_malformed_token ... ok
test tests::verify_rejects_tampered_signature ... ok
test tests::verify_rejects_token_from_other_instance ... ok

test result: ok. 17 passed

## pc-auth 全套（含 R661）

| 类型 | tests | 状态 |
|---|---|---|
| lib unit | 80 | ok |
| integration | 7 | ok |
| r661_agent_jwt_real_pg | 2 | ok NEW |
| 总计 | 89 | 0 FAIL（87 -> 89） |

## pc-authz 全套（无回归）

| 类型 | tests | 状态 |
|---|---|---|
| lib unit | 85 | ok |
| integration | 6 | ok |
| other | 23 | ok |
| 其他 | 8 | ok |
| 总计 | 122 | 0 FAIL |

## 关键文件位置

- crates/pc-agent-jwt/src/lib.rs (557 行)
- crates/pc-agent-jwt/Cargo.toml
- crates/pc-auth/src/lib.rs (新增 verify_agent_jwt_actor)
- crates/pc-auth/Cargo.toml (加 pc-agent-jwt dep + dev-deps)
- crates/pc-auth/tests/r661_agent_jwt_real_pg.rs (199 行)
- Cargo.toml (workspace 加 pc-agent-jwt)

## 关键设计点

1. 不依赖第三方 JWT crate：Node 用 crypto.createHmac，Rust 用 hmac + sha2 + base64url 直接实现 HS256
2. 隔离属性在 signing key 上而非 payload 上：per-instance + per-company 派生 key 是真正 boundary
3. pc-auth 不耦合 pc-agent-jwt 具体实现：独立 crate，可被其他模块直接使用
4. 完整兼容 legacy fallback：disable_legacy_fallback=false 时旧 token 仍能 verify

## 后续路线

- R662: Status cards / Summary slots / issue-* 子服务补齐
- R663: pc-server 二进制 build（隔离 target）+ 真实启动
- R664: workspace-realization / workspace-runtime 补齐
