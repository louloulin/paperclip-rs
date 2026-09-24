//! `POST …/tasks/:taskId/skill-bundles/resolve` 的**三源**解析（M6-4 / LUM-1669）。
//!
//! M3-7 只实现 `workspace` 源时，这条路由没有任何端到端用例（claim 面当时也还不发
//! `skill_refs`，见 `docs/32` 偏离表）⇒ 本文件补上 `DoD` 里那三条硬证据：
//!
//! 1. 三个源各一条正例（`workspace` / `builtin` / `plugin`），且 `source` 字段回的是
//!    **台账决定**的源（不是请求里那个字符串）；
//! 2. 插件的 pinned hash 对不上 ⇒ **409**（上游一直有这道门，只是它的 `switch` 没有
//!    `plugin` 分支 ⇒ M6-4 之前程序里没有路径能走到它）；
//! 3. `builtin:<name>` 这种**不是 `uuid`** 的 id 也能解析（同一条路由同时伺候两种 id 形态）。
//!
//! 期望的 `digest` 由测试**自己**按上游 `skillbundle.BuildManifest` 复算（不调
//! `mc_core::skill`），否则「实现和期望值同一处漂」时用例仍会绿。

use axum::http::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support;

/// 上游 `skillbundle.writeHashPart`：`fmt.Fprintf(h, "%d:%s\n", len(value), value)`。
fn write_hash_part(hasher: &mut Sha256, value: &str) {
    hasher.update(format!("{}:{}\n", value.len(), value).as_bytes());
}

/// 上游 `skillbundle.BuildManifest` 的独立复算（`files` 按 `path` 升序传入）。
fn upstream_hash(
    source: &str,
    id: &str,
    name: &str,
    description: &str,
    content: &str,
    files: &[(&str, &str)],
) -> String {
    let mut sorted: Vec<(&str, &str)> = files.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = Sha256::new();
    write_hash_part(&mut hasher, "v1");
    write_hash_part(&mut hasher, source);
    write_hash_part(&mut hasher, id);
    write_hash_part(&mut hasher, name);
    write_hash_part(&mut hasher, description);
    write_hash_part(&mut hasher, content);
    for (path, body) in sorted {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())));
        write_hash_part(&mut hasher, path);
        write_hash_part(&mut hasher, &digest);
        write_hash_part(&mut hasher, body);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// 建一条 `skill` 行，返回其 id。
async fn seed_skill(
    pool: &PgPool,
    workspace_id: Uuid,
    name: &str,
    content: &str,
    plugin_installation_id: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO skill (workspace_id, name, description, content, plugin_installation_id) \
         VALUES ($1, $2, 'itest', $3, $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(content)
    .bind(plugin_installation_id)
    .fetch_one(pool)
    .await
    .expect("insert skill")
}

async fn bind(pool: &PgPool, agent_id: Uuid, skill_id: Uuid) {
    sqlx::query("INSERT INTO agent_skill (agent_id, skill_id, enabled) VALUES ($1, $2, TRUE)")
        .bind(agent_id)
        .bind(skill_id)
        .execute(pool)
        .await
        .expect("insert agent_skill");
}

/// 三源夹具：workspace skill（1 个支持文件）+ plugin skill（1 个支持文件）+ 内置 skill。
struct Fixture {
    app: axum::Router,
    pool: PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    runtime_id: Uuid,
    task_id: Uuid,
    workspace_skill: Uuid,
    plugin_skill: Uuid,
}

impl Fixture {
    async fn build() -> Option<Self> {
        let (pool, db) = support::connect().await?;
        let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
        let (runtime_id, agent_id, task_id) =
            support::seed_ready_task(&pool, workspace_id, user_id, "m64").await;
        // 状态必须落在 `dispatched` / `waiting_local_directory`，否则 409 `task is not preparing`。
        sqlx::query("UPDATE agent_task_queue SET status = 'dispatched' WHERE id = $1")
            .bind(task_id)
            .execute(&pool)
            .await
            .expect("set task dispatched");

        let workspace_skill =
            seed_skill(&pool, workspace_id, "ws-skill", "main skill content", None).await;
        sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES ($1, 'rules.md', $2)")
            .bind(workspace_skill)
            .bind("rules content")
            .execute(&pool)
            .await
            .expect("insert skill_file");
        bind(&pool, agent_id, workspace_skill).await;

        // 插件贡献的行：`368` 只加了列，**没有**外键 ⇒ 不必先建 `plugin_installation`。
        let plugin_skill = seed_skill(
            &pool,
            workspace_id,
            "plugin-skill",
            "plugin skill content",
            Some(Uuid::new_v4()),
        )
        .await;
        sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES ($1, 'extra.md', $2)")
            .bind(plugin_skill)
            .bind("extra content")
            .execute(&pool)
            .await
            .expect("insert skill_file");
        bind(&pool, agent_id, plugin_skill).await;

        Some(Self {
            app: support::app_with_db(db),
            pool,
            workspace_id,
            user_id,
            runtime_id,
            task_id,
            workspace_skill,
            plugin_skill,
        })
    }

    fn uri(&self) -> String {
        format!(
            "/api/daemon/runtimes/{}/tasks/{}/skill-bundles/resolve",
            self.runtime_id, self.task_id
        )
    }

    async fn resolve(&self, skills: Value) -> (StatusCode, Value) {
        support::call(
            &self.app,
            "POST",
            &self.uri(),
            self.user_id,
            Some("m64"),
            Some(json!({ "skills": skills })),
        )
        .await
    }
}

/// 三个源各一条正例：解析出来的 bundle 的 `source` 由**台账**决定，digest 与独立复算一致。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn resolve_serves_workspace_builtin_and_plugin_sources() {
    let Some(fx) = Fixture::build().await else {
        return;
    };
    let workspace_id = fx.workspace_skill.to_string();
    let plugin_id = fx.plugin_skill.to_string();
    let builtin_id = mc_skill::builtin::builtin_skill_id(mc_skill::builtin::PLATFORM_SKILL_NAME);

    // 插件的 pinned hash 是 claim 交给 daemon 的那个值；`skill` 表里没有 digest 列，
    // 所以用例按上游 `BuildManifest` 自己复算一份（独立于 `mc_core` 的实现）。
    let plugin_hash = upstream_hash(
        "plugin",
        &plugin_id,
        "plugin-skill",
        "itest",
        "plugin skill content",
        &[("extra.md", "extra content")],
    );
    let (status, body) = fx
        .resolve(json!([
            { "id": workspace_id, "source": "workspace", "hash": "sha256:whatever" },
            { "id": builtin_id, "source": "builtin", "hash": "sha256:whatever" },
            { "id": plugin_id, "source": "plugin", "hash": plugin_hash },
        ]))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundles = body["bundles"].as_array().expect("bundles array").clone();
    assert_eq!(bundles.len(), 3, "{body}");

    // workspace：源来自台账（请求字符串相同也照台账写），digest = 独立复算。
    assert_eq!(bundles[0]["source"], "workspace");
    assert_eq!(bundles[0]["id"], workspace_id);
    assert_eq!(bundles[0]["content"], "main skill content");
    assert_eq!(
        bundles[0]["hash"],
        json!(upstream_hash(
            "workspace",
            &workspace_id,
            "ws-skill",
            "itest",
            "main skill content",
            &[("rules.md", "rules content")],
        ))
    );
    assert_eq!(bundles[0]["files"][0]["path"], "rules.md");
    assert_eq!(bundles[0]["files"][0]["content"], "rules content");

    // builtin：id **不是** uuid，正文来自编译期内联资产。
    assert_eq!(bundles[1]["source"], "builtin");
    assert_eq!(bundles[1]["id"], builtin_id);
    assert!(
        bundles[1]["content"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "{body}"
    );
    assert_eq!(
        bundles[1]["hash"],
        json!(
            mc_skill::builtin::builtin_skill_by_id(&builtin_id)
                .expect("platform skill")
                .manifest()
                .hash
        )
    );

    // plugin：源由 `plugin_installation_id` 决定。
    assert_eq!(bundles[2]["source"], "plugin");
    assert_eq!(bundles[2]["id"], plugin_id);
    assert_eq!(bundles[2]["content"], "plugin skill content");
    assert_eq!(bundles[2]["hash"], json!(plugin_hash));

    let (status, body) = fx
        .resolve(json!([
            { "id": plugin_id, "source": "plugin", "hash": "sha256:stale" },
        ]))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = support::error_message(&body).to_string();
    assert!(message.contains("pinned plugin"), "{body}");

    support::cleanup(&fx.pool, fx.workspace_id, &[fx.user_id]).await;
}

/// 非规范 / 未知形态的 `ref` 一律 `not found`（不是 400）：与「agent 没有这个 skill」同一答案。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn resolve_reports_unknown_refs_as_not_found() {
    let Some(fx) = Fixture::build().await else {
        return;
    };
    let missing = Uuid::new_v4().to_string();

    for (label, refs) in [
        (
            "unknown builtin",
            json!([{ "id": "builtin:nope", "source": "builtin", "hash": "sha256:x" }]),
        ),
        (
            "unknown workspace uuid",
            json!([{ "id": missing, "source": "workspace", "hash": "sha256:x" }]),
        ),
        (
            "non-uuid workspace id",
            json!([{ "id": "not-a-uuid", "source": "workspace", "hash": "sha256:x" }]),
        ),
        (
            "source without a server-side producer",
            json!([{ "id": missing, "source": "local", "hash": "sha256:x" }]),
        ),
        (
            "workspace ref naming a plugin row",
            json!([{
                "id": fx.plugin_skill.to_string(),
                "source": "workspace",
                "hash": "sha256:x",
            }]),
        ),
    ] {
        let (status, body) = fx.resolve(refs).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{label}: {body}");
    }

    // 空字段才是 400（上游 handler 的那道门）。
    let (status, body) = fx
        .resolve(json!([{ "id": fx.workspace_skill.to_string(), "source": "workspace" }]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    support::cleanup(&fx.pool, fx.workspace_id, &[fx.user_id]).await;
}
