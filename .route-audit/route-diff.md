# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **693**
- Rust unique routes: **865**
- Common: **648**
- Missing in Rust: **45**
- Extra in Rust:   **217**
- **Coverage (method+path): 93.51%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 17 |
| `/api/root/*` | 13 |
| `/api/exports/*` | 2 |
| `/api/export/*` | 2 |
| `/api/imports/*` | 2 |
| `/api/misc/*` | 1 |
| `/api/restart/*` | 1 |
| `/api/archive/*` | 1 |
| `/api/timeline/*` | 1 |
| `/api/feedback-traces/*` | 1 |
| `/api/preview/*` | 1 |
| `/api/artifacts/*` | 1 |
| `/api/jobs/*` | 1 |
| `/api/branding/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| DELETE | `/api/:param` |
| DELETE | `/api/companies/:param/skills/:param/files` |
| DELETE | `/api/labels/:param` |
| DELETE | `/api/secrets/:param` |
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
| GET | `/apiauthorization` |
| GET | `/apicontent-type` |
| GET | `/apihost` |
| GET | `x-paperclip-dev-server-status-token` |
| PATCH | `/api/:param` |
| PATCH | `/api/:param/branding` |
| PATCH | `/api/companies/:param/skills/:param/files` |
| PATCH | `/api/companies/:param/smoke-lab/runs/:param` |
| PATCH | `/api/profile` |
| PATCH | `/api/tool-profiles/:param` |
| POST | `/api/` |
| POST | `/api/:param/archive` |
| POST | `/api/:param/export` |
| POST | `/api/:param/exports` |
| POST | `/api/:param/exports/preview` |
| POST | `/api/:param/imports/apply` |
| POST | `/api/:param/imports/preview` |
| POST | `/api/cases/:param/issue-links` |
| POST | `/api/companies/:param/activity` |
| POST | `/api/companies/:param/approvals` |
| POST | `/api/companies/:param/decisions` |
| POST | `/api/companies/:param/pipelines` |
| POST | `/api/companies/:param/teams/catalog/:param/preview` |
| POST | `/api/import/preview` |
| POST | `/api/issues/:param/read` |
| POST | `/dev-server/restart` |
| PUT | `/api/cases/:param/documents/:param` |
| PUT | `/api/pipelines/:param/transitions` |
