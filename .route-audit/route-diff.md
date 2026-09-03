# M30 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **641**
- Rust unique routes: **903**
- Common: **579**
- Missing in Rust: **62**
- Extra in Rust:   **324**
- **Coverage (method+path): 90.33%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 46 |
| `/api/oauth/*` | 5 |
| `/api/me/*` | 3 |
| `/api/runtime-tools/*` | 2 |
| `/api/task-drain/*` | 2 |
| `/api/connections/*` | 2 |
| `/api/root/*` | 1 |
| `/api/vercel-connect/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| DELETE | `/api/agents/me/secret-proposals/:param` |
| DELETE | `/api/heartbeat-runs/:param/provider-trace` |
| DELETE | `/api/instance/task-drain` |
| DELETE | `/api/tool-connections/:param/grants/:param/delegations/:param` |
| DELETE | `/api/tool-connections/:param/services/:param` |
| GET | `/api/agents/me/secret-proposals` |
| GET | `/api/companies${COMPANY_IMPORT_TRANSFERS_ROUTE_PATH}/:param` |
| GET | `/api/companies/:param/claude-oauth-token-status` |
| GET | `/api/companies/:param/decision-queue-seed-rules` |
| GET | `/api/companies/:param/decision-queues` |
| GET | `/api/companies/:param/decision-queues/:param/items` |
| GET | `/api/companies/:param/decision-triage/:param/:param` |
| GET | `/api/companies/:param/managed-agent-profiles` |
| GET | `/api/companies/:param/provider-traces` |
| GET | `/api/companies/:param/remote-agent-profiles` |
| GET | `/api/companies/:param/secret-proposals` |
| GET | `/api/companies/:param/secrets/catalog` |
| GET | `/api/companies/:param/setup-token-login-sessions/:param` |
| GET | `/api/companies/:param/setup-token-login-sessions/:param/prompt` |
| GET | `/api/companies/:param/tools/apps/:param/preflight` |
| GET | `/api/connection-intents/:param/setup-options` |
| GET | `/api/heartbeat-runs/:param/provider-trace` |
| GET | `/api/instance/task-drain` |
| GET | `/api/issues/:param/queued-comments` |
| GET | `/api/mcp/runtime-tools` |
| GET | `/api/stacks` |
| GET | `/api/tool-connections/:param/services` |
| GET | `/api/tool-connections/:param/services/:param/status` |
| GET | `/api/tool-connections/:param/test-agents/:param/access` |
| GET | `/api/tools/oauth/cloud-connector/callback` |
| GET | `/api/tools/oauth/cloud-connector/enrollment` |
| GET | `/api/tools/oauth/cloud-connector/enrollment-callback` |
| GET | `/api/tools/oauth/paperclip-id/callback` |
| GET | `/api/tools/vercel-connect/callback` |
| PATCH | `/api/companies/:param/decision-queues/:param` |
| POST | `/api/agents/me/secret-proposals` |
| POST | `/api/cases/:param/issue-links` |
| POST | `/api/companies${COMPANY_IMPORT_TRANSFERS_ROUTE_PATH}/:param/apply` |
| POST | `/api/companies${COMPANY_IMPORT_TRANSFERS_ROUTE_PATH}/:param/preview` |
| POST | `/api/companies/:param/decision-queues` |
| POST | `/api/companies/:param/decision-retention/:param/:param/archive` |
| POST | `/api/companies/:param/decision-retention/:param/:param/revive` |
| POST | `/api/companies/:param/managed-agent-profiles` |
| POST | `/api/companies/:param/remote-agent-profiles` |
| POST | `/api/companies/:param/secret-proposals/:param/approve` |
| POST | `/api/companies/:param/secret-proposals/:param/reject` |
| POST | `/api/companies/:param/setup-token-login-sessions` |
| POST | `/api/companies/:param/setup-token-login-sessions/:param/cancel` |
| POST | `/api/companies/:param/setup-token-login-sessions/:param/code` |
| POST | `/api/companies/:param/setup-token-login-sessions/:param/completion` |
