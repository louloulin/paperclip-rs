# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **581**
- Rust unique routes: **869**
- Common: **567**
- Missing in Rust: **14**
- Extra in Rust:   **302**
- **Coverage (method+path): 97.59%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 10 |
| `/api//*` | 2 |
| `/api/restart/*` | 1 |
| `/api/root/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| DELETE | `/api/labels/:param` |
| DELETE | `/api/secrets/:param` |
| GET | `/` |
| GET | `/api/_plugins/:param/ui/*filePath` |
| GET | `/api/companies/` |
| GET | `/api/companies/:param/search/extract` |
| POST | `/api/cases/:param/issue-links` |
| POST | `/api/companies/` |
| POST | `/api/companies/:param/activity` |
| POST | `/api/companies/:param/approvals` |
| POST | `/api/companies/:param/decisions` |
| POST | `/api/companies/:param/pipelines` |
| POST | `/dev-server/restart` |
| PUT | `/api/cases/:param/documents/:param` |
