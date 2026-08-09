# M21 — Node ↔ Rust 路由 method+path 重合率

- Node unique routes: **581**
- Rust unique routes: **880**
- Common: **578**
- Missing in Rust: **3**
- Extra in Rust:   **302**
- **Coverage (method+path): 99.48%**

## Top missing categories

| Category | Missing count |
|---|---:|
| `/api/:param/*` | 2 |
| `/api/restart/*` | 1 |

## Top 50 missing method+path

| Method | Path |
|---|---|
| GET | `/api/_plugins/:param/ui/*filePath` |
| GET | `/api/companies/:param/search/extract` |
| POST | `/dev-server/restart` |
