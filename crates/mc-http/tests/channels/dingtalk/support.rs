//! `DingTalk` 端到端测试的**共用件**（M7-9）。
//!
//! 从 `../dingtalk.rs` 拆出来是**门 ⑩**（单文件 800 行硬限）的要求；切点是「装置 / 用例」。
//! 与 `channels/support.rs`（M7-4 的共用件）不同：这份只管 `DingTalk` 面自己的装置
//! （落库密钥、`AppKey` 生成、群观察行的插入、`accessToken` 替身），**不改** M7-4 的文件。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{app_with, call, cleanup, connect, seed, Seed};

use mc_http::state::ChannelKeys;

/// `DingTalk` 的落库密钥（`MULTICA_DINGTALK_SECRET_KEY`：base64 的 32 字节）。
pub(super) const SECRET_KEY_BASE64: &str = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";

/// 一条形态合法的 `AppSecret`（**测试专用**，不是任何真实凭据）。
pub(super) const APP_SECRET: &str = "dingtest-app-secret";

/// 每次用例生成一个**新的** `AppKey`：`(dingtalk, app_id)` 是全局唯一的路由槽
/// （同一个机器人不能连到两个地方 —— 那是**真实**语义），用固定值会让重复运行 / 上一次
/// 失败留下的行互相占槽。
pub(super) fn app_key() -> String {
    format!("dingtest-{}", Uuid::new_v4().simple())
}

pub(super) async fn unconfigured() -> Option<Fx> {
    build(ChannelKeys::default()).await
}

/// 配好落库密钥的装置。
pub(super) async fn configured() -> Option<Fx> {
    build(ChannelKeys::from_env_with(|name| {
        if name == "MULTICA_DINGTALK_SECRET_KEY" {
            Some(SECRET_KEY_BASE64.to_string())
        } else {
            None
        }
    }))
    .await
}

pub(super) async fn build(keys: ChannelKeys) -> Option<Fx> {
    let (pool, db) = connect().await?;
    let seed = seed(&pool).await;
    let app = app_with(db.clone(), keys);
    Some(Fx {
        pool,
        db,
        app,
        seed,
    })
}

macro_rules! fixture {
    ($f:expr) => {
        match $f.await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

pub(super) use fixture;

pub(super) struct Fx {
    pub(super) pool: sqlx::PgPool,
    db: mc_db::Db,
    pub(super) app: axum::Router,
    pub(super) seed: Seed,
}

impl Fx {
    pub(super) fn installations_uri(&self) -> String {
        format!(
            "/api/workspaces/{}/dingtalk/installations",
            self.seed.workspace_id
        )
    }

    pub(super) fn groups_uri(&self) -> String {
        format!("/api/workspaces/{}/dingtalk/groups", self.seed.workspace_id)
    }

    pub(super) fn byo_uri(&self, agent_id: Uuid) -> String {
        format!(
            "/api/workspaces/{}/dingtalk/install/byo?agent_id={agent_id}",
            self.seed.workspace_id
        )
    }

    pub(super) fn forget_uri(&self, installation_id: Uuid, conversation_id: &str) -> String {
        format!(
            "/api/workspaces/{}/dingtalk/installations/{}/groups/{conversation_id}",
            self.seed.workspace_id, installation_id
        )
    }

    pub(super) fn agent_groups_uri(&self, agent_id: Uuid) -> String {
        format!(
            "/api/agents/{agent_id}/dingtalk/groups?workspace_id={}",
            self.seed.workspace_id
        )
    }

    /// 造一个 agent（复用 seed 的 runtime）。
    ///
    /// `permission_mode` 默认 `private`（迁移的列默认值）⇒ 只有 owner / workspace admin
    /// 能打开它 —— 这正是 scope 矩阵要的那一态。
    pub(super) async fn new_agent(&self, owner_id: Uuid, permission_mode: &str) -> Uuid {
        let runtime_id: Uuid =
            sqlx::query_scalar("SELECT id FROM agent_runtime WHERE workspace_id = $1 LIMIT 1")
                .bind(self.seed.workspace_id)
                .fetch_one(&self.pool)
                .await
                .expect("runtime");
        sqlx::query_scalar(
            "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind, \
              permission_mode) VALUES ($1, $2, 'local', $3, $4, 'user', $5) RETURNING id",
        )
        .bind(self.seed.workspace_id)
        .bind(format!("itest-m79-{}", Uuid::new_v4()))
        .bind(runtime_id)
        .bind(owner_id)
        .bind(permission_mode)
        .fetch_one(&self.pool)
        .await
        .expect("insert agent")
    }

    /// 一条观察行（`dingtalk_group_presence`）。
    pub(super) async fn observe(
        &self,
        installation_id: Uuid,
        conversation_id: &str,
        title: &str,
        active: bool,
    ) {
        sqlx::query(
            "INSERT INTO dingtalk_group_presence \
             (workspace_id, installation_id, conversation_id, conversation_title, last_active_at, \
              mention_count) VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN now() ELSE NULL END, 3)",
        )
        .bind(self.seed.workspace_id)
        .bind(installation_id)
        .bind(conversation_id)
        .bind(title)
        .bind(active)
        .execute(&self.pool)
        .await
        .expect("observe");
    }

    /// 发一次成员请求（无体）—— 用例里最常见的形状。
    pub(super) async fn request(
        &self,
        method: &str,
        uri: &str,
        user: Uuid,
    ) -> (StatusCode, serde_json::Value) {
        call(&self.app, method, uri, Some(user), None).await
    }

    /// 发一次带 JSON 体的成员请求。
    pub(super) async fn request_body(
        &self,
        method: &str,
        uri: &str,
        user: Uuid,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        call(&self.app, method, uri, Some(user), Some(body)).await
    }

    /// 一条安装行的当前状态。
    pub(super) async fn installation_status(&self, installation_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM channel_installation WHERE id = $1")
            .bind(installation_id)
            .fetch_one(&self.pool)
            .await
            .expect("status")
    }

    /// 清场：先删 `dingtalk_*` 三张表（它们**没有** FK ⇒ `cleanup` 删不掉），再走通用清理。
    pub(super) async fn teardown(self) {
        for sql in [
            "DELETE FROM dingtalk_group_presence WHERE workspace_id = $1",
            "DELETE FROM dingtalk_bot_identity WHERE workspace_id = $1",
            "DELETE FROM dingtalk_group_route WHERE workspace_id = $1",
        ] {
            let _ = sqlx::query(sql)
                .bind(self.seed.workspace_id)
                .execute(&self.pool)
                .await;
        }
        cleanup(&self.pool, &self.seed).await;
        self.db.close().await;
    }
}

/// 起一个只认 `POST /v1.0/oauth2/accessToken` 的替身，返回基址。
///
/// **只替平台 wire**（`docs/60` §4.2 第 1 条）：断言链是"真 HTTP → 真 handler → 真 DB"。
pub(super) async fn serve_token_stub(ok: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let base = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().expect("addr").port()
    );
    let app = axum::Router::new().route(
        "/v1.0/oauth2/accessToken",
        axum::routing::post(move || async move {
            if ok {
                axum::Json(json!({ "accessToken": "stub-token", "expireIn": 7200 }))
            } else {
                axum::Json(json!({
                    "code": "InvalidAuthentication",
                    "message": "appKey rejected"
                }))
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    base
}

impl Fx {
    /// 直接落一条**活跃**安装行（跳过 BYO 的活校验，让群清单用例不必起替身）。
    pub(super) async fn install_active(&self) -> Uuid {
        self.install_active_for(self.seed.agent_id).await
    }

    pub(super) async fn install_active_for(&self, agent_id: Uuid) -> Uuid {
        let app_key = format!("dingkey-{}", Uuid::new_v4().simple());
        sqlx::query_scalar(
            "INSERT INTO channel_installation \
             (workspace_id, agent_id, channel_type, config, installer_user_id) \
             VALUES ($1, $2, 'dingtalk', $3, $4) RETURNING id",
        )
        .bind(self.seed.workspace_id)
        .bind(agent_id)
        .bind(json!({ "app_id": app_key, "robot_code": app_key }))
        .bind(self.seed.admin)
        .fetch_one(&self.pool)
        .await
        .expect("insert installation")
    }

    /// 直接落一枚绑定令牌（`binding.rs` 自己的用例覆盖"铸"的那一半）。
    pub(super) async fn mint_token(&self, raw: &str, installation_id: Uuid, channel_user_id: &str) {
        sqlx::query(
            "INSERT INTO channel_binding_token \
             (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
             VALUES ($1, $2, $3, 'dingtalk', $4, now() + interval '15 minutes')",
        )
        .bind(mc_channel::dingtalk::binding::hash_binding_token(raw))
        .bind(self.seed.workspace_id)
        .bind(installation_id)
        .bind(channel_user_id)
        .execute(&self.pool)
        .await
        .expect("insert token");
    }
}
