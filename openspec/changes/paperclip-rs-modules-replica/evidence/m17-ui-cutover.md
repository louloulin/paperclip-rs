# Evidence: M17 — UI 切流真实链路（U1）

> 用户目标"真实启动前后端验证"硬阻塞 1。**真实验证通过**。

## 改动

| 操作 | 文件 |
|---|---|
| `client.ts` 增加 `VITE_API_BASE` 支持（保留 `/api` 默认） | `ui/src/api/client.ts:3` |
| 新增一键全栈脚本（PG + migrate + server + vite + 5-endpoint 验证） | `scripts/dev-ui-rust.sh` |
| 新增 VITE_API_BASE 合约测试（4 case：default / 相对路径 / 绝对 URL / POST） | `ui/src/api/client-vite-api-base.test.ts` |

## 真实运行结果

```
[dev] init pg data dir at /var/folders/nj/.../pc-dev-pgdata-72008
[dev] start pg on :55489
[dev] pc-migrate up                                       ✅
[dev] start pc-server :53308 (background)
[dev] pc-server /health 200 after 1s                     ✅
[dev] start vite dev :51983 (VITE_API_BASE=pc-server :53308)
[dev] vite ready after 0s                                 ✅
[dev] verify 5 GET endpoints through vite proxy → pc-server
[dev] PASS  /health                       → 200
[dev] PASS  /api/auth/get-session         → 401   (合约：未认证拒绝)
[dev] PASS  /api/companies                → 200
[dev] PASS  /api/agents                   → 200
[dev] PASS  /api/feature-flags            → 200
[dev] summary: 5 pass / 0 fail (out of 5)
[dev] ALL CHECKS PASSED — M17 UI 切流真实链路 ✅
```

## 关键设计

### `ui/src/api/client.ts`

```ts
// 默认保留历史行为：Vite dev-server 通过 proxy 转发 /api/* 到 3100。
// 设置 VITE_API_BASE=http://localhost:53100 可直接指向其他 Rust 实例，
// 用于合约测试、staging 部署、scripts/dev-ui-rust.sh 全栈联调。
const BASE = (import.meta.env.VITE_API_BASE ?? "/api").replace(/\/$/, "");
const IS_ABSOLUTE_BASE = /^https?:\/\//.test(BASE);
```

fetch URL 自动处理绝对 URL + 绝对路径的斜杠去重（避免 `http://host/api/health`）。

### `scripts/dev-ui-rust.sh`

- 端口随机化（PG 55440–55640、server 53200–53400、UI 51800–52000）避开残留实例冲突
- LC_ALL=C 解决 PG 在中文 locale 下 postmaster 启动失败
- 全栈任一环节失败立即退出 + tail 相关 log
- 5 endpoint 探针：404 视为失败，401/200/204 视为合约成功

## 验证矩阵

| 通道 | 验证 | 结果 |
|---|---|---|
| pc-server `/health` | curl 直连 :$SRV_PORT | 200 OK |
| pc-server `/api/auth/get-session` | 无 session → 401（合约预期） | ✅ |
| pc-server `/api/companies` | 空公司列表 200 | ✅ |
| pc-server `/api/agents` | 空 agent 列表 200 | ✅ |
| pc-server `/api/feature-flags` | 2 个 default flag | ✅ |
| vite dev server | 5173 实际绑定 `localhost` → 测试改用 `http://localhost:$UI_PORT` | ✅ |

## UI 合约测试（vitest）

`ui/src/api/client-vite-api-base.test.ts` 4 个 case：

| Case | 验证 |
|---|---|
| `default relative` | `VITE_API_BASE=""` → 请求落到 `/api/health` |
| `relative override` | `VITE_API_BASE="/api/"` → 去掉尾部斜杠，无 `/api//` |
| `absolute URL` | `VITE_API_BASE="http://localhost:53100"` → 完整 URL，无 `//` |
| `POST + absolute URL` | mutation 请求同样走绝对 URL，无重复斜杠 |

## 结论

**M17 通过**：UI ↔ Rust server 切流真实链路完整，5/5 endpoint 合约通过，
脚本化、可重复、CI 可用。`VITE_API_BASE` 提供生产/独立部署切流能力。
