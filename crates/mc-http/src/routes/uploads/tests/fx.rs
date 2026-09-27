//! M10-B2 真库用例的**共用夹具**（`Fx` / `fixture()`）。
//!
//! ## 为什么不能直接复用 M10-B1 的 `attachments/tests/fx.rs`
//!
//! 那个夹具是 `routes/attachments` 的**私有子模块**（`#[path]` 挂在 B1 的
//! `tests.rs` 下）⇒ 从 `routes/uploads` 走 `super::super::attachments::tests::fx`
//! 是跨切片的私有路径耦合，且 B1 的 `TempDir` 生命周期挂在它自己的 `Drop` 上。
//! 同一份建表序列在两个切片里各存一份，比耦合更便宜（判据：本仓的
//! `routes/onboarding` / `routes/cloud` 都是同款各存一份）。

use uuid::Uuid;

use super::support::{new_uuid, pool};

/// 真库夹具：两个 workspace（`ws` / `other_ws`）各一个 user，外加一个 admin 与一个
/// **非成员**。
#[allow(dead_code)] // 夹具字段：不是每条用例都读全部字段（`other` / `admin` / `other_ws` 是留给后续切片的）。
pub struct Fx {
    pub db: mc_db::Db,
    /// `ws` 的 owner。
    pub user: Uuid,
    /// `other_ws` 的 owner（也是 `ws` 的 member）。
    pub other: Uuid,
    /// `ws` 的 admin。
    pub admin: Uuid,
    /// **不在** `ws` 的 `member` 表里的 user（「非成员 ⇒ 403」那条的夹具）。
    pub outsider: Uuid,
    pub ws: Uuid,
    pub ws_slug: String,
    pub other_ws: Uuid,
    pub issue: Uuid,
    pub issue_other_ws: Uuid,
}

/// 建立夹具。`MULTICA_TEST_DATABASE_URL` 没设 ⇒ 返回 `None`（用例直接 return）。
pub async fn fixture() -> Option<Fx> {
    let db = pool().await?;
    let user = new_uuid();
    let other = new_uuid();
    let admin = new_uuid();
    let outsider = new_uuid();
    let ws = new_uuid();
    let other_ws = new_uuid();
    let ws_slug = format!("w{}", ws.simple());

    // 🔴 `workspace.slug` 有 UNIQUE（`workspace_slug_key`）⇒ 写死 `'a'` / `'b'`
    // 会让**并行跑的每一条**用例互相撞 `23505`。定式 = slug 由本用例那枚 uuid 派生。
    for (w, tag) in [(ws, "a"), (other_ws, "b")] {
        sqlx::query("INSERT INTO workspace (id, name, slug) VALUES ($1, $2, $3)")
            .bind(w)
            .bind(format!("ws-{tag}"))
            .bind(if w == ws {
                ws_slug.clone()
            } else {
                format!("w{}", w.simple())
            })
            .execute(db.pool())
            .await
            .expect("workspace");
    }
    // 🔴 `member.user_id` 有 FK → `user(id)`；`user.email` **有 UNIQUE** ⇒ 同款定式。
    for u in [user, other, admin, outsider] {
        sqlx::query("INSERT INTO \"user\" (id, name, email) VALUES ($1, $2, $3)")
            .bind(u)
            .bind(format!("user-{}", u.simple()))
            .bind(format!("u{}@m10b2.local", u.simple()))
            .execute(db.pool())
            .await
            .expect("user");
    }
    // 🔴 `outsider` **故意不在** `member` 表里。
    for (w, u, role) in [
        (ws, user, "owner"),
        (ws, admin, "admin"),
        (ws, other, "member"),
        (other_ws, user, "owner"),
    ] {
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ($1,$2,$3)")
            .bind(w)
            .bind(u)
            .bind(role)
            .execute(db.pool())
            .await
            .expect("member");
    }

    let issue = new_uuid();
    let issue_other_ws = new_uuid();
    for (i, w) in [(issue, ws), (issue_other_ws, other_ws)] {
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, number, identifier, title, status, \
                                creator_type, creator_id) \
             VALUES ($1,$2,1,$3,'t','todo','user',$4)",
        )
        .bind(i)
        .bind(w)
        .bind(format!("M10B2-{}", i.simple()))
        .bind(user)
        .execute(db.pool())
        .await
        .expect("issue");
    }

    Some(Fx {
        db,
        user,
        other,
        admin,
        outsider,
        ws,
        ws_slug,
        other_ws,
        issue,
        issue_other_ws,
    })
}
