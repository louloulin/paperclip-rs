# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **581**
- Rust unique routes: **870**
- Common: **568**
- Missing in Rust: **13**
- Extra in Rust:   **302**
- **Coverage (method+path): 97.76%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 9 |
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
| POST | `/api/companies/:param/approvals` |
| POST | `/api/companies/:param/decisions` |
| POST | `/api/companies/:param/pipelines` |
| POST | `/dev-server/restart` |
| PUT | `/api/cases/:param/documents/:param` |
