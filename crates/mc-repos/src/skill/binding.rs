//! `agent_skill` 的绑定面（**授权面**）+ runtime-local skill 的 per-agent 覆盖列。
//!
//! - **写者**：M6-4（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/agent.go` 的 `/api/agents/{id}/skills*` 六个 handler。
//! - **语义**：`agent_skill` 这张表**就是**授权 —— 一个 skill 能不能被某个 agent 看到/用，
//!   唯一的判据是这里有没有一行且 `enabled = TRUE`（`ListAgentSkillsByIDs` 是唯一入口）。
//!   所以本文件的写操作必须与「读面」用同一套过滤条件（`enabled` 不要一边查一边不查）。
//! - **幂等**：绑定 / 解绑 / 启停都是**幂等**动作（重复 add = 200 而不是 500）。
//!   实现上走 `ON CONFLICT (agent_id, skill_id) DO UPDATE SET enabled = ...`。
//! - **bundle 的 `source`**：这里只落/读行；bundle 的 `SkillRef{source}` 投影在 route 层
//!   （`workspace` / `builtin` / `plugin` 三态见 `mc_core::skill::SkillSource`）。
//!   判据是 `skill.plugin_installation_id`：非空 ⇒ 插件贡献的（迁移 `368`），本文件把它
//!   原样读出来（[`AgentSkillBundleRow`]），**不在 SQL 里下 `IS NULL` 的收窄** ——
//!   插件贡献的 skill 也是普通 workspace skill（`368` 的注释），resolve 路径要按源分流。
//! - **runtime-local 覆盖列**：`agent.disabled_runtime_skills` 是同一个「哪些 skill 不该被继承」
//!   的授权面（`PUT /api/agents/{id}/runtime-skills/enabled`），只有本文件写它。
//!   它落在 `agent` 表上（不是 `agent_skill`），但语义上是这一类绑定的另一半 ⇒ 同文件。
//!   为了不碰 `mc_repos::agent`（M3-5 的文件），这里只写这一列、只读它的旧值。
//! - **不做什么**：不建「skill 组 / 目录」（上游这一代没有）；不拼 DTO；不做权限判定
//!   （成员 / 角色判定在 route 层）。
//!
//! **状态：M6-4 已落地（LUM-1669）**。
//!
//! 行预算（门 ⑩）：预计 200 行以内。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value as Json;
use sqlx::FromRow;
use uuid::Uuid;

use super::read::SkillRow;
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 全量 `skill` 列（含 `content` 与 `plugin_installation_id`）。
const SKILL_COLUMNS: &str = "s.id, s.workspace_id, s.name, s.description, s.content, s.config, \
                             s.created_by, s.plugin_installation_id, s.created_at, s.updated_at";
/// 摘要列（上游 `ListAgentSkillSummaries`：**不含 `content`**，理由同 `ListSkillSummariesByWorkspace`）。
const SUMMARY_COLUMNS: &str = "s.id, s.workspace_id, s.name, s.description, s.config, \
                               s.created_by, s.created_at, s.updated_at, ask.enabled";

/// `ListAgentSkillSummaries` 的行 —— `GET /api/agents/{id}/skills` 的响应单元。
///
/// ⚠️ `enabled` 是**行的真实值**，且查询**不带** `ask.enabled = TRUE` 过滤：列表要显示
/// 被停用的绑定（`enabled = false`），而解析/供给路径（[`SkillBindingRepo::skill_bundles_for_agent`]）
/// 才会加这个谓词。上游两条 SQL 的口径差就这一处。
#[derive(Debug, Clone, FromRow)]
pub struct AgentSkillSummaryRow {
    /// `skill.id`。
    pub id: Uuid,
    /// `skill.workspace_id`。
    pub workspace_id: Uuid,
    /// `skill.name`。
    pub name: String,
    /// `skill.description`。
    pub description: String,
    /// `skill.config` JSONB。
    pub config: Json,
    /// `skill.created_by`。
    pub created_by: Option<Uuid>,
    /// `skill.created_at`。
    pub created_at: DateTime<Utc>,
    /// `skill.updated_at`。
    pub updated_at: DateTime<Utc>,
    /// `agent_skill.enabled`。
    pub enabled: bool,
}

impl AgentSkillSummaryRow {
    /// 领域 id。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }
}

/// `agent_skill` 授权下的一个 skill + 它的支持文件（`…/skill-bundles/resolve` 的读单元）。
///
/// 与 `mc_repos::daemon::SkillBundleRow` 的区别：`skill` 是**全 10 列**的行（`SkillRow`），
/// 因此带 `plugin_installation_id` —— resolve 路径正是靠它把 `source` 分成
/// `workspace` / `plugin`。`daemon::SkillRow`（9 列）是 M3-7 的旧投影，本片不改它。
#[derive(Debug, Clone)]
pub struct AgentSkillBundleRow {
    /// `skill` 行（10 列，含 `plugin_installation_id`）。
    pub skill: SkillRow,
    /// `(path, content)`，按 `skill_id, path` 升序。
    pub files: Vec<(String, String)>,
}

impl AgentSkillBundleRow {
    /// 是否由插件安装贡献（`skill.plugin_installation_id IS NOT NULL`）。
    #[must_use]
    pub fn is_plugin(&self) -> bool {
        self.skill.plugin_installation_id.is_some()
    }
}

/// `agent.disabled_runtime_skills` 的当前值（`PUT …/runtime-skills/enabled` 读改写的一半）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentRuntimeSkillStateRow {
    /// `agent.id`。
    pub id: Uuid,
    /// 绑定中的 runtime（换机器后这次覆盖要作废 ⇒ 上游比这个值）。
    pub runtime_id: Option<Uuid>,
    /// JSONB 数组。
    pub disabled_runtime_skills: Json,
}

/// 绑定面仓储（`agent_skill` 的读写 + `agent.disabled_runtime_skills`）。
#[derive(Clone)]
pub struct SkillBindingRepo {
    db: Db,
}

impl SkillBindingRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// agent 的 skill 摘要列表（上游 `ListAgentSkillSummaries`，`s.name ASC`；含停用行）。
    ///
    /// `content` 不出库：SKILL.md 动辄 50–200KB，列表页带上正文会把 CLI 拖到 15s 超时
    /// （GH #2174）。`FromRow` 是按列名取值，缺列会运行期报错 ⇒ 不能图省事查全列。
    pub async fn list_agent_skill_summaries(
        &self,
        agent_id: Id,
    ) -> Result<Vec<AgentSkillSummaryRow>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM skill s \
             JOIN agent_skill ask ON ask.skill_id = s.id \
             WHERE ask.agent_id = $1 ORDER BY s.name ASC"
        );
        sqlx::query_as::<_, AgentSkillSummaryRow>(&sql)
            .bind(agent_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 解析路径：**恰好**给定 id 集合里、该 agent 已启用（`enabled = TRUE`）的 skill + 支持文件。
    ///
    /// 上游 `ListAgentSkillsByIDs` 的逐字移植（`ORDER BY s.name ASC` + `s.id = ANY(...)`）：
    /// 关联谓词**就是**授权 —— agent 没有的 id 只是查不到行，调用方把它报成 404，
    /// 「不存在」与「没权限」因此是同一个答案（不泄露存在性）。
    ///
    /// 空集合直接返回空表（省一次往返；上游在 `len(requestedIDs) > 0` 时才查）。
    /// 支持文件用**第二条 SQL 一次取回**再按 `skill_id` 归组（上游
    /// `ListSkillFilesBySkillIDs`），不是每条 skill 一次 N+1。
    pub async fn skill_bundles_for_agent(
        &self,
        agent_id: Id,
        skill_ids: &[Uuid],
    ) -> Result<Vec<AgentSkillBundleRow>> {
        if skill_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {SKILL_COLUMNS} FROM skill s \
             JOIN agent_skill ask ON ask.skill_id = s.id \
             WHERE ask.agent_id = $1 AND ask.enabled = TRUE AND s.id = ANY($2::uuid[]) \
             ORDER BY s.name ASC"
        );
        let skills = sqlx::query_as::<_, SkillRow>(&sql)
            .bind(agent_id.0)
            .bind(skill_ids)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        if skills.is_empty() {
            return Ok(Vec::new());
        }
        let files = sqlx::query_as::<_, (Uuid, String, String)>(
            "SELECT skill_id, path, content FROM skill_file \
             WHERE skill_id = ANY($1::uuid[]) ORDER BY skill_id, path ASC",
        )
        .bind(skill_ids)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        // 按 skill_id 归组：稳定的两条循环（不是 N+1，也不是 HashMap 的无序迭代）。
        let mut out = Vec::with_capacity(skills.len());
        for skill in skills {
            let own: Vec<(String, String)> = files
                .iter()
                .filter(|(sid, _, _)| *sid == skill.id)
                .map(|(_, path, content)| (path.clone(), content.clone()))
                .collect();
            out.push(AgentSkillBundleRow { skill, files: own });
        }
        Ok(out)
    }

    /// 全量替换绑定（上游 `SetAgentSkills`：先 `RemoveAllAgentSkills` 再逐条 `AddAgentSkill`）。
    ///
    /// 一个事务：清空与重插之间不能有别的读者看到「一个 skill 都没有」的中间态。
    /// 重插走 `ON CONFLICT DO NOTHING` ⇒ 重复 id 幂等；`enabled` 回到列默认 `TRUE`
    /// （`161_agent_skill_enabled.up.sql`），这正是上游「PUT 重置启停状态」的语义。
    pub async fn replace_agent_skills(&self, agent_id: Id, skill_ids: &[Uuid]) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        sqlx::query("DELETE FROM agent_skill WHERE agent_id = $1")
            .bind(agent_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        for skill_id in skill_ids {
            sqlx::query(
                "INSERT INTO agent_skill (agent_id, skill_id) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(agent_id.0)
            .bind(skill_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }
        tx.commit().await.map_err(map_sqlx_err)
    }

    /// 追加绑定（上游 `AddAgentSkills`）：**不动**已有行，因此已停用的绑定保持停用。
    pub async fn add_agent_skills(&self, agent_id: Id, skill_ids: &[Uuid]) -> Result<()> {
        for skill_id in skill_ids {
            sqlx::query(
                "INSERT INTO agent_skill (agent_id, skill_id) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(agent_id.0)
            .bind(skill_id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        }
        Ok(())
    }

    /// 解绑一条（上游 `RemoveAgentSkill`，**幂等**：删不到行也算成功）。
    pub async fn remove_agent_skill(&self, agent_id: Id, skill_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM agent_skill WHERE agent_id = $1 AND skill_id = $2")
            .bind(agent_id.0)
            .bind(skill_id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 启停一条（上游 `SetAgentSkillEnabled`，`:execrows`）—— 返回受影响行数。
    ///
    /// `rows == 0` ⇒ 该 agent 根本没绑这个 skill，route 层映射 **404**
    /// （`agent skill not found`）。这正是「不 upsert」的原因：upsert 会把
    /// 一条不存在的绑定变成存在，语义就反了。
    pub async fn set_agent_skill_enabled(
        &self,
        agent_id: Id,
        skill_id: Uuid,
        enabled: bool,
    ) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE agent_skill SET enabled = $3 WHERE agent_id = $1 AND skill_id = $2",
        )
        .bind(agent_id.0)
        .bind(skill_id)
        .bind(enabled)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(result.rows_affected())
    }

    /// `PUT …/runtime-skills/enabled` 的读半：该 agent 的 runtime + 当前覆盖数组。
    ///
    /// 上游在读改写之间用 `GetAgentForUpdate` 加行锁（`FOR UPDATE`），本仓这里也加 ——
    /// 两个并发的 toggle 若各读各写，后写的那个会把前者的结果整段覆盖掉。
    /// ⚠️ 调用方必须**紧接着**在**同一个事务**里调 [`Self::update_disabled_runtime_skills`]，
    /// 所以本方法返回持有者事务的连接；本仓仓储层不跨事务持有锁的做法在这里不适用
    /// （上游同样在一个 tx 内）。
    pub async fn lock_agent_runtime_skills(
        &self,
        tx: &mut sqlx::PgConnection,
        agent_id: Id,
    ) -> Result<Option<AgentRuntimeSkillStateRow>> {
        sqlx::query_as::<_, AgentRuntimeSkillStateRow>(
            "SELECT id, runtime_id, disabled_runtime_skills FROM agent WHERE id = $1 FOR UPDATE",
        )
        .bind(agent_id.0)
        .fetch_optional(tx)
        .await
        .map_err(map_sqlx_err)
    }

    /// `PUT …/runtime-skills/enabled` 的写半（上游 `UpdateAgentDisabledRuntimeSkills`）。
    pub async fn update_disabled_runtime_skills(
        &self,
        tx: &mut sqlx::PgConnection,
        agent_id: Id,
        value: &Json,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE agent SET disabled_runtime_skills = $2::jsonb, updated_at = now() \
             WHERE id = $1",
        )
        .bind(agent_id.0)
        .bind(value)
        .execute(tx)
        .await
        .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 开始一个事务（`PUT …/runtime-skills/enabled` 的读改写要用同一个连接）。
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        self.db.pool().begin().await.map_err(map_sqlx_err)
    }
}

impl RepoWithDb for SkillBindingRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod db_tests {
    //! `agent_skill` 绑定面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
    //!
    //! 手感与 `crate::agent::db_tests` 一致：**未设置**变量 → 打印跳过并 `return`；
    //! **已设置但连不上 / 没建表** → panic（不许静默跳过假装绿）。
    //! 目标库 = 上游 schema（`cargo run -p mc-migrate -- run --dir migrations`）。
    //!
    //! 为什么这几条必须有真库：本文件的语义全在 SQL 谓词上（`enabled = TRUE` 过不过滤、
    //! `ON CONFLICT DO NOTHING` 保不保旧值、`:execrows` 数得对不对），
    //! 用 mock 断言这些等于什么都没测。

    use super::*;

    /// 建 workspace + agent + 两个 skill（含 1 个支持文件）→ `(db, ws, agent, [s1, s2])`。
    async fn setup() -> Option<(Db, Id, Id, [Uuid; 2])> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        let suffix = Uuid::new_v4().simple().to_string();
        let ws: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m6-4-binding', $1) RETURNING id",
        )
        .bind(format!("itest-m6-4-{suffix}"))
        .fetch_one(db.pool())
        .await
        .unwrap_or_else(|e| panic!("workspace fixture failed (run `mc-migrate run`): {e}"));
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent(workspace_id, name, runtime_mode) VALUES ($1, $2, 'cloud') \
             RETURNING id",
        )
        .bind(ws)
        .bind(format!("itest-m6-4-agent-{suffix}"))
        .fetch_one(db.pool())
        .await
        .unwrap_or_else(|e| panic!("agent fixture failed: {e}"));
        let mut skills = [Uuid::nil(); 2];
        for (i, slot) in skills.iter_mut().enumerate() {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO skill(workspace_id, name, description, content) \
                 VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(ws)
            .bind(format!("itest-skill-{i}-{suffix}"))
            .bind(format!("desc {i}"))
            .bind(format!("# body {i}"))
            .fetch_one(db.pool())
            .await
            .unwrap_or_else(|e| panic!("skill fixture failed: {e}"));
            *slot = id;
        }
        // 注意：**每个 skill 各自一条文件行**（不是共用一张表），文件行是 skill 级联删除的。
        sqlx::query("INSERT INTO skill_file(skill_id, path, content) VALUES ($1, $2, $3)")
            .bind(skills[0])
            .bind("references/a.md")
            .bind("# a")
            .execute(db.pool())
            .await
            .unwrap_or_else(|e| panic!("skill_file fixture failed: {e}"));
        Some((db, Id::from(ws), Id::from(agent), skills))
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(v) => v,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn summaries_keep_disabled_bindings_and_carry_enabled_flag() {
        let (db, _ws, agent, skills) = fixture!();
        let repo = SkillBindingRepo::new(db.clone());
        repo.replace_agent_skills(agent, &skills).await.unwrap();
        // 停用其中一条：列表仍要看得见它（`enabled = false`），resolve 则看不见。
        assert!(
            repo.set_agent_skill_enabled(agent, skills[1], false)
                .await
                .unwrap()
                > 0
        );
        let rows = repo.list_agent_skill_summaries(agent).await.unwrap();
        assert_eq!(rows.len(), 2, "停用的绑定也要出现在列表里");
        println!("ok: summaries={}", rows.len());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn resolve_hides_disabled_bindings_and_groups_files() {
        let (db, _ws, agent, skills) = fixture!();
        let repo = SkillBindingRepo::new(db.clone());
        repo.replace_agent_skills(agent, &skills).await.unwrap();
        let rows = repo
            .skill_bundles_for_agent(agent, &[skills[0], skills[1]])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].files.len(), 1, "支持文件要按 skill 归组");

        repo.set_agent_skill_enabled(agent, skills[0], false)
            .await
            .unwrap();
        let after = repo.skill_bundles_for_agent(agent, &skills).await.unwrap();
        assert_eq!(after.len(), 1, "解析路径只认 enabled = TRUE");
        assert_eq!(after[0].skill.id, skills[1]);
        let none = repo
            .skill_bundles_for_agent(agent, &[skills[0]])
            .await
            .unwrap();
        assert!(none.is_empty(), "被停用的 id 读出来是「不存在」");
        println!("ok: resolve-after-disable={}", after.len());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn add_preserves_a_disabled_binding_while_replace_resets_it() {
        let (db, _ws, agent, skills) = fixture!();
        let repo = SkillBindingRepo::new(db.clone());
        repo.add_agent_skills(agent, &[skills[0]]).await.unwrap();
        repo.set_agent_skill_enabled(agent, skills[0], false)
            .await
            .unwrap();
        // POST（追加）不能把「用户手动停用」的绑定悄悄打开。
        repo.add_agent_skills(agent, &[skills[0], skills[1]])
            .await
            .unwrap();
        let rows = repo.list_agent_skill_summaries(agent).await.unwrap();
        assert!(!rows[0].enabled, "add 不重置 enabled");
        // PUT（全量替换）会重建绑定 ⇒ 回到列默认 TRUE。
        repo.replace_agent_skills(agent, &skills).await.unwrap();
        let rows = repo.list_agent_skill_summaries(agent).await.unwrap();
        assert!(rows.iter().all(|r| r.enabled), "replace 重置 enabled");
        println!("ok: add/replace enabled semantics");
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn enable_and_remove_are_idempotent_and_report_missing_rows() {
        let (db, _ws, agent, skills) = fixture!();
        let repo = SkillBindingRepo::new(db.clone());
        // 没绑过 ⇒ 0 行受影响（route 层据此回 404，而不是「悄悄 upsert 出来一条」）。
        assert_eq!(
            repo.set_agent_skill_enabled(agent, skills[0], true)
                .await
                .unwrap(),
            0
        );
        repo.add_agent_skills(agent, &[skills[0]]).await.unwrap();
        assert_eq!(
            repo.set_agent_skill_enabled(agent, skills[0], true)
                .await
                .unwrap(),
            1
        );
        // 解绑两次都成功（幂等）。
        repo.remove_agent_skill(agent, skills[0]).await.unwrap();
        repo.remove_agent_skill(agent, skills[0]).await.unwrap();
        assert!(repo
            .list_agent_skill_summaries(agent)
            .await
            .unwrap()
            .is_empty());
        println!("ok: idempotent enable/remove");
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn plugin_owned_rows_are_flagged_for_the_resolve_source_split() {
        let (db, _ws, agent, skills) = fixture!();
        let repo = SkillBindingRepo::new(db.clone());
        repo.replace_agent_skills(agent, &skills).await.unwrap();
        sqlx::query("UPDATE skill SET plugin_installation_id = $2 WHERE id = $1")
            .bind(skills[0])
            .bind(Uuid::new_v4())
            .execute(db.pool())
            .await
            .unwrap();
        let rows = repo.skill_bundles_for_agent(agent, &skills).await.unwrap();
        assert_eq!(rows.len(), 2);
        let plugins = rows.iter().filter(|r| r.is_plugin()).count();
        assert_eq!(
            plugins, 1,
            "只有 plugin_installation_id 非空的那条是 plugin 源"
        );
        println!("ok: plugin-owned flagged={plugins}");
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn runtime_skill_override_read_modify_write_in_one_transaction() {
        let (db, _ws, agent, _skills) = fixture!();
        let repo = SkillBindingRepo::new(db.clone());
        let mut tx = repo.begin().await.unwrap();
        let state = repo
            .lock_agent_runtime_skills(&mut tx, agent)
            .await
            .unwrap()
            .expect("agent 一定存在");
        assert_eq!(state.disabled_runtime_skills, serde_json::json!([]));
        let next = serde_json::json!([{"runtime_id": "r", "root": "plugin", "key": "k"}]);
        repo.update_disabled_runtime_skills(&mut tx, agent, &next)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let read_back: Json =
            sqlx::query_scalar("SELECT disabled_runtime_skills FROM agent WHERE id = $1")
                .bind(agent.0)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(read_back, next);
        println!("ok: runtime-skill override persisted");
    }
}
