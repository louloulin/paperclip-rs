# R635 — middleware 补齐 batch 1（compression / trust-proxy / private-hostname-guard / http-log-policy）

## Status

DONE — 四个 middleware 复刻完成并注册进默认 stack，全部测试绿。

## Files added / modified

| Path | Status | Notes |
|---|---|---|
| crates/pc-http/src/middleware/compression.rs | new (414 LOC) | gzip/deflate 协商 + 阈值 + ETag 弱化 + Vary |
| crates/pc-http/src/middleware/trust_proxy.rs | new (~580 LOC) | TRUST_PROXY 解析 + proxy-addr 语义 req.ip |
| crates/pc-http/src/middleware/private_hostname_guard.rs | new (~380 LOC) | exposure=private 时 Host 守卫 |
| crates/pc-http/src/middleware/http_log_policy.rs | new (189 LOC) | 8 个静默 API 正则 + 静态前缀 |
| crates/pc-http/src/middleware/access_log.rs | modified | client_ip + 静默策略接入 |
| crates/pc-http/src/middleware/stack.rs | modified | 链式 .layer() 装配（含 Extension 注入） |
| crates/pc-http/src/middleware/mod.rs | modified | 注册 4 个新模块 |
| apps/pc-server/src/main.rs | modified | 注入 PrivateHostnameGuardConfig + TrustProxyConfig + ConnectInfo |
| crates/pc-http/Cargo.toml | modified | + flate2 / url |
| apps/pc-server/Cargo.toml | modified | + pc-network-bind |

## 与 Node 语义对齐（含实测校准）

### compression（api-compression.ts）
- 仅 JSON content-type（`application/json` 或 `+json`）压缩；≥1024 字节阈值
- `Accept-Encoding` 协商：q 值、`*` 通配、q=0 排除、平局 gzip 优先
- `deflate` 用 **zlib 封装**（Node `zlib.deflate` = RFC 1950，浏览器语义一致）
- 跳过 no-transform（**词边界扫描**：`-` 是 non-word，不能按字符 split）、
  Content-Disposition/Accept-Ranges/Content-Range、204/304、已编码响应、HEAD
- 压缩后追加 Vary、弱化强 ETag（`"abc"` → `W/"abc"`）、删 content-md5

### trust-proxy（trust-proxy.ts + Express 5 proxy-addr）
- 解析：unset/""/false/0 → 不设置；true → TrustAll；严格正整数 → Hops；
  逗号列表 → Subnets（named: loopback/linklocal/uniquelocal + CIDR）
- 真值（node proxy-addr@2.0.7 + Express compileTrust 实测）：
  - no-trust → socket；trust-all → XFF 最左
  - hops=n → 链第 n+1 项；n ≥ 链长 → 最左（全部可信）
  - subnets → 从 socket 向左第一个不可信；全可信 → 最左
- socket 带端口 → 剥离端口后返回（与 Express `req.ip` 一致）
- IPv4-mapped IPv6（`::ffff:10.1.2.3`）双向跨族匹配（对齐 proxy-addr）

### private-hostname-guard
- 仅 `PAPERCLIP_DEPLOYMENT_EXPOSURE=private` 启用
- `x-forwarded-host` 优先，URL 解析失败回退原始值
- IPv6 host 剥离方括号（Node `URL.hostname` 返回 `[::1]` 导致其自身
  loopback 判定永不命中——此处有意修正该上游怪癖）
- 403 响应：`/api` 前缀或 `req.accepts(["json","html","text"])==="json"` →
  JSON，否则 text/plain。accepts 判定按 **negotiator 算法**移植
  （specificity bits + q + accept 顺序 + 候选顺序，平局时 accept 头先出现者胜）

### http-log-policy
- 对齐 Node 8 个 API 正则 + 静态前缀静默规则（access_log 集成）

## Test results（真实执行输出）

```
cargo test -p pc-http --lib middleware  -> 92 passed; 0 failed
cargo test -p pc-http --lib             -> 451 passed; 0 failed
cargo check -p pc-server                -> Finished dev profile (0 error)
```

测试覆盖：gzip/deflate 协商、阈值、no-transform、ETag 弱化、HEAD/204 跳过、
TRUST_PROXY 全部解析分支、hops/subnets/named 子网、IPv6 压缩与点分混合、
IPv4-mapped、socket 端口剥离、hostname 提取、accepts 9 条规则、守卫启用/放行/阻断。

## Design decisions

1. **axum from_fn + 链式 layer**：`ServiceBuilder` + from_fn 组合有 trait bound
   问题，改用链式 `.layer()`（后添加者外层先执行），stack.rs 中按执行顺序逆序添加。
2. **纯函数与 IO 分离**：协商/解析/匹配全部纯函数导出，层薄包装；测试直接测纯函数。
3. **配置经 Extension 注入**：TrustProxyConfig / PrivateHostnameGuardConfig 由
   pc-server 启动时从环境解析，middleware 不直接读 env（可测性）。
4. **压缩挂在最内层**：保证能看到 handler 最终 Content-Type（String 响应默认
   text/plain 不会被压缩——与 Node 检查 res.getHeader 行为一致）。
5. **node 实测校准**：accepts 平局（html 先出现 → html）、hops 越界回退最左、
   deflate=zlib 格式三处均按真实 Node 行为修正，避免"想当然"实现。

## Next

R636：validate / board-mutation-guard + error-handler 错误映射扩展。
