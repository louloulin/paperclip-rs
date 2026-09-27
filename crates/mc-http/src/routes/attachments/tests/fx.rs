//! M10-B1 真库用例的**共用夹具**（`Fx` / `Att` / `fixture()` / `app_with_objects`）。
//!
//! 拆出来是因为真库那一半分两个文件（`db.rs` 元数据读、`db_bytes.rs` 取字节 + 删除），
//! 而门 ⑩ 的 800 行上限对单文件生效（`routes/onboarding/tests/` 的同款拆法：
//! `support.rs` 装夹具、`db.rs` 装用例）。
//!
//! ## 真库判据总表（`docs/64` §6.5 的 M10-B1 行 + 专属验收）
//!
//! | 键 | 钉住的东西 |
//! |---|---|
//! | `GET /api/attachments/{id}` | 200 + 响应体逐字段；跨 workspace **404**；非成员 **404** |
//! | `GET /api/issues/{id}/attachments` | 200 + 列表顺序（`created_at ASC`）；**不含**能力链接 |
//! | `.../content` | 200 + `text/plain` + `X-Original-Content-Type`；非白名单 **415** |
//! | `.../download` | 200 + 字节 + `Content-Disposition` inline；非成员 **404**（IDOR 形状） |
//! | `.../signed-download` | 无签名 **403**（fail-closed） |
//! | `DELETE /api/attachments/{id}` | 上传者 200 / admin 200 / **别人 403** / 抓取上下文副本 **404** |
//!
//! **零出站**：6 条全是本地路由，不替任何云侧。

use uuid::Uuid;

use super::support::{new_uuid, pool, test_app_with_objects, TempDir};
/// 真库夹具：两个 workspace（`ws` / `other_ws`）各一个 user，外加一个 admin。
pub struct Fx {
    pub db: mc_db::Db,
    pub user: Uuid,
    pub other: Uuid,
    pub admin: Uuid,
    pub ws: Uuid,
    pub other_ws: Uuid,
    pub issue: Uuid,
    pub issue_other_ws: Uuid,
}

pub async fn fixture() -> Option<Fx> {
    let db = pool().await?;
    let user = new_uuid();
    let other = new_uuid();
    let admin = new_uuid();
    let ws = new_uuid();
    let other_ws = new_uuid();

    // 🔴 `workspace.slug` 有 UNIQUE（`workspace_slug_key`）⇒ 写死 `'a'` / `'b'`
    // 会让**并行跑的每一条**用例互相撞 `23505`（本轮真跑实测：20 条全红在同一格）。
    // 定式 = slug 由**本用例那枚 uuid** 派生 ⇒ 既唯一又不需要全局协调。
    // 这与「`user.email` UNIQUE ⇒ 空白邮箱用例要用 tag 映射成唯一值」是同一条判例。
    for (w, tag) in [(ws, "a"), (other_ws, "b")] {
        sqlx::query("INSERT INTO workspace (id, name, slug) VALUES ($1, $2, $3)")
            .bind(w)
            .bind(format!("ws-{tag}"))
            .bind(format!("w{}-{}", tag, w.simple()))
            .execute(db.pool())
            .await
            .expect("workspace");
    }
    // 🔴 `member.user_id` 有 FK → `user(id)` ⇒ 三个 user 都得先落一行。
    // 🔴 `user.email` **有 UNIQUE**（`user_email_key`）⇒ email 必须由本用例那枚
    // uuid 派生，写死会在并行用例间互撞 `23505`（与 slug 同一判例）。
    for u in [user, other, admin] {
        sqlx::query("INSERT INTO \"user\" (id, name, email) VALUES ($1, $2, $3)")
            .bind(u)
            .bind(format!("user-{}", u.simple()))
            .bind(format!("u{}@m10b1.local", u.simple()))
            .execute(db.pool())
            .await
            .expect("user");
    }
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
        // 形状照 `mc_repos::comment` 的既有测试（`comment.rs:858`）：
        // `creator_type` / `creator_id`（**不是** `uploader_*`）+ `number` / `identifier`。
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, number, identifier, title, status, \
                                creator_type, creator_id) \
             VALUES ($1,$2,1,$3,'t','todo','user',$4)",
        )
        .bind(i)
        .bind(w)
        .bind(format!("M10B1-{}", i.simple()))
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
        ws,
        other_ws,
        issue,
        issue_other_ws,
    })
}

/// 一条待插入的附件（`url` 就是对象引用）。
///
/// 用结构体而不是一长串位置参数：9 个参数里 5 个是同一形状的 `Uuid`，
/// 位置传参极易把 `issue` / `workspace` / `uploader` 串错一行，而那种错**不会编译失败**。
pub struct Att<'a> {
    pub issue: Uuid,
    pub ws: Uuid,
    pub uploader: Uuid,
    pub uploader_type: &'a str,
    pub filename: &'a str,
    pub url: &'a str,
    pub content_type: &'a str,
    pub captured: bool,
}

impl Fx {
    /// 在 `ws` 下插一条属于 `issue` 的附件。
    pub async fn attach(&self, a: Att<'_>) -> Uuid {
        let id = new_uuid();
        sqlx::query(
            "INSERT INTO attachment (id, workspace_id, issue_id, uploader_type, uploader_id, \
                                    filename, url, content_type, size_bytes, source_context_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,0,$9)",
        )
        .bind(id)
        .bind(a.ws)
        .bind(a.issue)
        .bind(a.uploader_type)
        .bind(a.uploader)
        .bind(a.filename)
        .bind(a.url)
        .bind(a.content_type)
        .bind(a.captured.then(new_uuid))
        .execute(self.db.pool())
        .await
        .expect("attachment");
        id
    }
}

/// 造一个带本地磁盘存储的 app（对象真写进磁盘）。
pub fn app_with_objects(fx: &Fx, tmp: &TempDir) -> (axum::Router, String) {
    let bucket = "mc-assets".to_owned();
    (
        test_app_with_objects(fx.db.clone(), tmp.path(), &bucket),
        bucket,
    )
}
