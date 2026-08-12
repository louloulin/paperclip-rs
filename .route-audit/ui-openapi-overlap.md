# M19 — UI client × Rust OpenAPI 路径覆盖率

- UI 客户端 distinct 调用: **11**
- Rust OpenAPI paths: **0**
- 命中: **0**
- UI 调用但 OpenAPI 缺失: **11**
- OpenAPI 声明但 UI 未用: **0**
- **覆盖率: 0.0%**

## Top 30 missing (UI 真实调用，但 OpenAPI 文档未注册)

| Method | Path | File |
|---|---|---|
| GET | `/api/adapters/${encodeURIComponent(adapterType)}/ui-parser.js` | adapters/dynamic-loader.ts |
| GET | `/api/adapters/${encodeURIComponent(adapterType)}/ui-parser.js` | adapters/dynamic-loader.ts |
| GET | `/api/adapters/:type/ui-parser.js` | adapters/dynamic-loader.ts |
| GET | `/api/assets/${assetId}/content` | lib/attention.ts |
| GET | `/api/assets/${attachment.asset.id}/content` | api/cases.ts |
| GET | `/api/companies/${companyId}/audit/agent-actions.csv${qs ? ` | api/audit.ts |
| GET | `/api/companies/${encodeURIComponent(companyId)}/events/ws` | components/transcript/useLiveRunTranscripts.ts |
| GET | `/api/health` | lib/agent-onboarding-prompt.ts |
| GET | `/api/issues/${encodeURIComponent(issueId)}/file-resources/content?${params.toString()}` | api/file-resources.ts |
| GET | `/api/plugins/${encodeURIComponent(pluginId)}/bridge/stream/${encodeURIComponent(channel)}?${params.toString()}` | plugins/bridge.ts |
| GET | `/api/plugins/:pluginId/actions/:key` | plugins/bridge.ts |
| GET | `/api/plugins/:pluginId/data/:key` | plugins/bridge.ts |
| GET | `/api/v1/runs` | lib/agent-onboarding-prompt.ts |
