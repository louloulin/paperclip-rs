# M19 — UI client × Rust OpenAPI 路径覆盖率

- UI 客户端 distinct 调用: **11**
- Rust OpenAPI paths: **899**
- 命中: **5**
- UI 调用但 OpenAPI 缺失: **6**
- OpenAPI 声明但 UI 未用: **894**
- **覆盖率: 45.45%**

## Top 30 missing (UI 真实调用，但 OpenAPI 文档未注册)

| Method | Path | File |
|---|---|---|
| GET | `/api/adapters/:type/ui-parser.js` | adapters/dynamic-loader.ts |
| GET | `/api/companies/${companyId}/audit/agent-actions.csv${qs ? ` | api/audit.ts |
| GET | `/api/issues/${encodeURIComponent(issueId)}/file-resources/content?${params.toString()}` | api/file-resources.ts |
| GET | `/api/plugins/${encodeURIComponent(pluginId)}/bridge/stream/${encodeURIComponent(channel)}?${params.toString()}` | plugins/bridge.ts |
| GET | `/api/plugins/:pluginId/actions/:key` | plugins/bridge.ts |
| GET | `/api/plugins/:pluginId/data/:key` | plugins/bridge.ts |
