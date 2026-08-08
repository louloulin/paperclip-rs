# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **693**
- Rust unique routes: **687**
- Common: **526**
- Missing in Rust: **167**
- Extra in Rust:   **161**
- **Coverage (method+path): 75.9%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 129 |
| `/api/root/*` | 14 |
| `/api/settings/*` | 3 |
| `/api/gateways/*` | 3 |
| `/api/exports/*` | 2 |
| `/api/imports/*` | 2 |
| `/api/export/*` | 2 |
| `/api/sessions/*` | 1 |
| `/api/restart/*` | 1 |
| `/api/artifacts/*` | 1 |
| `/api/users/*` | 1 |
| `/api/archive/*` | 1 |
| `/api/branding/*` | 1 |
| `/api/me/*` | 1 |
| `/api/timeline/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| DELETE | `/api/:param` |
| DELETE | `/api/adapters/:param` |
| DELETE | `/api/agents/:param` |
| DELETE | `/api/agents/:param/instructions-bundle/file` |
| DELETE | `/api/attachments/:param` |
| DELETE | `/api/companies/:param/folders/:param` |
| DELETE | `/api/companies/:param/me/user-secrets/:param` |
| DELETE | `/api/companies/:param/skill-policy` |
| DELETE | `/api/companies/:param/skill-test-run-templates/:param` |
| DELETE | `/api/companies/:param/skills/:param` |
| DELETE | `/api/companies/:param/skills/:param/comments/:param` |
| DELETE | `/api/companies/:param/skills/:param/files` |
| DELETE | `/api/companies/:param/skills/:param/star` |
| DELETE | `/api/companies/:param/skills/:param/test-inputs/:param` |
| DELETE | `/api/companies/:param/tools/policies/:param` |
| DELETE | `/api/decision-training/:param` |
| DELETE | `/api/environments/:param` |
| DELETE | `/api/environments/:param/custom-image-template` |
| DELETE | `/api/goals/:param` |
| DELETE | `/api/issues/:param` |
| DELETE | `/api/issues/:param/comments/:param` |
| DELETE | `/api/issues/:param/documents/:param` |
| DELETE | `/api/issues/:param/inbox-archive` |
| DELETE | `/api/issues/:param/watchdog` |
| DELETE | `/api/labels/:param` |
| DELETE | `/api/pipelines/:param/stages/:param` |
| DELETE | `/api/plugins/:param` |
| DELETE | `/api/projects/:param` |
| DELETE | `/api/projects/:param/workspaces/:param` |
| DELETE | `/api/routine-triggers/:param` |
| DELETE | `/api/secret-provider-configs/:param` |
| DELETE | `/api/secrets/:param` |
| DELETE | `/api/status-cards/:param` |
| DELETE | `/api/tool-applications/:param` |
| DELETE | `/api/tool-connections/:param` |
| DELETE | `/api/tool-profile-entries/:param` |
| DELETE | `/api/work-products/:param` |
| GET | `/` |
| GET | `/api/` |
| GET | `/api/:param` |
| GET | `/api/:param/artifacts` |
| GET | `/api/:param/export/fidelity` |
| GET | `/api/:param/feedback-traces` |
| GET | `/api/:param/timeline` |
| GET | `/api/_plugins/:param/ui/*filePath` |
| GET | `/api/companies/:param/search/extract` |
| GET | `/api/import/jobs/:param` |
| GET | `/apiPrefer` |
| GET | `/apiX-Paperclip-Run-Id` |
| GET | `/apiaccept` |
