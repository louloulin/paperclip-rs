# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **581**
- Rust unique routes: **878**
- Common: **576**
- Missing in Rust: **5**
- Extra in Rust:   **302**
- **Coverage (method+path): 99.14%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 3 |
| `/api/root/*` | 1 |
| `/api/restart/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| GET | `/` |
| GET | `/api/_plugins/:param/ui/*filePath` |
| GET | `/api/companies/:param/search/extract` |
| POST | `/api/cases/:param/issue-links` |
| POST | `/dev-server/restart` |
