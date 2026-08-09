# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **581**
- Rust unique routes: **874**
- Common: **572**
- Missing in Rust: **9**
- Extra in Rust:   **302**
- **Coverage (method+path): 98.45%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 5 |
| `/api//*` | 2 |
| `/api/root/*` | 1 |
| `/api/restart/*` | 1 |

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
| POST | `/dev-server/restart` |
