# Evidence: V1 — 真实基线验证（P0 硬阻塞）

> 日期：2026-08-09
> 模块：V1 真实基线验证
> 状态：✅ **通过**（PG + migrate + pc-server 启动 + /health 200 + 5 GET 全过）

---

## 1. 真实运行结果

### 1.1 PG 启动

```
initdb: OK
pg start: OK
127.0.0.1:55520 - accepting connections
pg ready: OK
```

### 1.2 pc-migrate up

```
2026-08-08T23:39:05.122732Z  INFO migrations up applied=205 available=205 pending=0 durationMs=703
applied 205 migration(s); 0 pending (205 total available) in 703.275584ms
table count = 172
```

**结论**：205 个迁移文件全部成功；DB 含 172 张表。

### 1.3 pc-server 启动

```
INFO pc_telemetry: telemetry initialized service=paperclip-server
INFO pc_db::pool: db connected attempt=1 max=16 min=1
INFO paperclip_server: heartbeat run recovery complete recovered=0 deferred=0
INFO paperclip_server: storage: local_disk provider registered root=/Users/louloulin/.paperclip/storage
INFO paperclip_server: feature flags: registered 2 default flags
INFO paperclip_server: plugin workers bootstrapped count=0
INFO paperclip_server: serving UI bundle from dist path=ui/dist
INFO paperclip_server: http listening host=127.0.0.1 port=53211
```

### 1.4 /health + 5 GET 端点

```
--- curl /health ---
HTTP 200
{"db":{"error":null,"latency_ms":0,"ok":true},"status":"ok","version":"0.1.0"}

--- 5 GET endpoints ---
/api/auth/get-session → 401   (合约：未认证返回 401)
/api/companies        → 200
/api/agents           → 200
/api/feature-flags    → 200
/api/projects         → 200
```

### 1.5 进程

```
PID 25078  target/debug/paperclip-server  (running, 验证后清理)
```

---

## 2. 关键发现 / 修复

### 2.1 pc-acpx 预存编译错误（修复）

**问题**：`crates/pc-acpx/src/git_workspace_sync.rs` 在 HEAD 状态有 15 个编译错误，阻止整个 workspace 编译。

**错误类型**：
- 7 × `prefix tmp_bundle is unknown`（Rust 2021 reserved `$identifier` syntax in string literals）
- 1 × `character constant must be escaped: '`（`value.replace(''')` 缺转义）
- 2 × `expected ',', found '$'`
- 4 × `cannot find type SshRemoteExecutionSpec`（缺 use）
- 2 × `cannot find type SshAuthArgs`（缺 use）
- 3 × `mismatched types: expected &str, found String`（`+ String::from(...)` 缺 `&` 借用）

**修复**（原创小补丁，6 处）：

1. **line 187**：use 修复（补 SshAuthArgs）
2. **line 692-700**：trap + cat 行用字符串拼接
3. **line 800-805**：cleanup_body 用字符串拼接
4. **line 818-824**：bundle create 用 s.push_str 模式
5. **line 830-832**：cat line 用字符串拼接
6. **line 899**：shell_quote_posix 单引号转义

**结果**：pc-acpx 编译通过（31 warning，0 error），整个 workspace 可编译。

### 2.2 端口冲突

**问题**：默认 55432 / 53100 在本地被占用。

**修复**：`PAPERCLIP_TEST_PG_PORT=55520` + `PAPERCLIP_TEST_HTTP_PORT=53212` 覆盖。

---

## 3. 验收清单

- [x] PG 临时实例启动 ✅
- [x] pc-migrate up 205 个迁移成功 ✅
- [x] 172 张表创建 ✅
- [x] pc-server 启动无 panic ✅
- [x] /health HTTP 200 + 正确 body ✅
- [x] /api/auth/get-session 401（合约正确）✅
- [x] /api/companies 200 ✅
- [x] /api/agents 200 ✅
- [x] /api/feature-flags 200 ✅
- [x] /api/projects 200 ✅
- [x] 启动无 fatal / panic ✅
- [x] 进程稳定运行 ✅

---

## 4. 下一步

V1 通过。V2-V15 候选：
- V11 + V12（UI 真实启动 + Playwright，用户硬目标）
- V2（CLI 全 19 子命令）
- V6（路由字节级补全）

