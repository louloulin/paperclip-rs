//! R519 — GET / 根路径 Node 契约对齐
//!
//! Node 端 (paperclip/server/src/routes/health.ts:112):
//!   router.get("/", async (req, res) => { ... })
//!
//! Rust 端契约：根路径与 /api、/api/health、/health 返回相同的 health
//! 探测体（status/version/deploymentMode/bootstrapStatus/authReady/db）。
//!
//! 该测试是契约级验证（不依赖 DB），因此仅做：
//! - 路径注册存在性（Router::route("/"...) 在 routes::health 模块中）
//! - 4 个路径的同构性（行为一致）

#[test]
fn health_module_exposes_root_path() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes/health.rs"),
    )
    .expect("read health.rs");
    let want_root = "route(\"/\", get(handler))";
    let want_health = "route(\"/health\", get(handler))";
    let want_api = "route(\"/api\", get(handler))";
    let want_api_health = "route(\"/api/health\", get(handler))";
    assert!(
        src.contains(want_root),
        "R519: GET / not registered (Node parity). src head: {}",
        &src[..src.len().min(400)]
    );
    assert!(src.contains(want_health), "R519: /health missing");
    assert!(src.contains(want_api), "R519: /api missing");
    assert!(src.contains(want_api_health), "R519: /api/health missing");
}

#[test]
fn health_module_router_function_compiles() {
    // 文档级：health::router 是 pc-http 的 pub fn，可被 routes/mod.rs 引用。
    // 这里只 import 路径确保编译期约束（与 R517 模式一致）。
    use pc_http::routes::health;
    let _ = health::router;
}
