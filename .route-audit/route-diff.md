# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **581**
- Rust unique routes: **879**
- Common: **577**
- Missing in Rust: **4**
- Extra in Rust:   **302**
- **Coverage (method+path): 99.31%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 3 |
| `/api/restart/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| GET | `/api/_plugins/:param/ui/*filePath` |
| GET | `/api/companies/:param/search/extract` |
| POST | `/api/cases/:param/links` |
| POST | `/dev-server/restart` |
