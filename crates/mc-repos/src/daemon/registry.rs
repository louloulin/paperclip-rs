//! runtime / workspace / token 面仓储（R7 拆分自 `daemon.rs`）。
//!
//! 上游落点：`server/internal/handler/daemon.go`（注册/心跳/下线）、
//! `daemon_workspace.go`（`workspace_repos`）、`runtime_profile.go`（profile 台账）、
//! `middleware/daemon_auth.go`（`mdt_` token 查表）。

// 本文件的 `impl DaemonRepo` 是 `daemon.rs` 那个 impl 的续块（R7 800 行拆分）。

use super::*;

impl DaemonRepo {
    /// 该用户是否为该 workspace 的成员（upstream `requireWorkspaceMember` 的读半边）。
    ///
    /// 返回 `Ok(false)` 表示**确认不是成员**（守卫落 404，不是 403）。
    pub async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.is_some_and(|(v,)| v))
    }

    /// workspace 是否存在（upstream `GetWorkspace`，404 `workspace not found`）。
    pub async fn workspace_exists(&self, workspace_id: Id) -> Result<bool> {
        let row: Option<(bool,)> =
            sqlx::query_as("SELECT EXISTS(SELECT 1 FROM workspace WHERE id = $1)")
                .bind(workspace_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_err)?;
        Ok(row.is_some_and(|(v,)| v))
    }

    /// workspace 名（`GET /api/daemon/workspaces`）。
    pub async fn workspace_name(&self, workspace_id: Id) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT name FROM workspace WHERE id = $1")
            .bind(workspace_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(row.map(|(v,)| v))
    }

    /// 该用户可见的全部 workspace（upstream `ListDaemonWorkspaces`）。
    pub async fn list_workspaces_for_user(&self, user_id: Id) -> Result<Vec<(Id, String)>> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT w.id, w.name FROM workspace w \
             JOIN member m ON m.workspace_id = w.id \
             WHERE m.user_id = $1 ORDER BY w.created_at ASC",
        )
        .bind(user_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id, n)| (Id::from(id), n)).collect())
    }

    /// `workspace.repos` / `workspace.settings` 投影（upstream `workspaceReposResponse`）。
    pub async fn workspace_repos(&self, workspace_id: Id) -> Result<Option<WorkspaceRepos>> {
        let row: Option<(Value, Value)> =
            sqlx::query_as("SELECT repos, settings FROM workspace WHERE id = $1")
                .bind(workspace_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_err)?;
        let Some((repos_raw, settings)) = row else {
            return Ok(None);
        };
        let repos = normalize_workspace_repos(&repos_raw);
        let settings = match &settings {
            Value::Null => None,
            Value::Object(map) if map.is_empty() => None,
            other => Some(other.clone()),
        };
        Ok(Some(WorkspaceRepos {
            workspace_id: workspace_id.as_string(),
            repos_version: repos_version(&repos),
            repos,
            settings,
        }))
    }

    // ---------------------------------------------------------------- register

    /// 按 `(workspace_id, daemon_id, provider) WHERE profile_id IS NULL` upsert 内置 runtime
    /// （upstream `UpsertAgentRuntime`，`runtime.sql:63`，逐字移植）。
    ///
    /// `inserted` 来自 `(xmax = 0)`：新插入 → `true`，更新既有行 → `false`。上游只拿它
    /// 决定是否打 `runtime_registered`/`runtime_ready` 埋点与是否继承机器自定义名
    /// （MUL-4217），本地保留了后者的语义。
    pub async fn upsert_runtime(&self, input: &UpsertRuntime) -> Result<RuntimeUpsert> {
        let sql = format!(
            "INSERT INTO agent_runtime \
                (workspace_id, daemon_id, name, runtime_mode, provider, status, device_info, \
                 metadata, owner_id, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now()) \
             ON CONFLICT (workspace_id, daemon_id, provider) WHERE profile_id IS NULL \
             DO UPDATE SET \
                name = EXCLUDED.name, \
                runtime_mode = EXCLUDED.runtime_mode, \
                status = EXCLUDED.status, \
                device_info = EXCLUDED.device_info, \
                metadata = EXCLUDED.metadata, \
                owner_id = COALESCE(EXCLUDED.owner_id, agent_runtime.owner_id), \
                last_seen_at = now(), \
                updated_at = now() \
             RETURNING {AGENT_RUNTIME_COLUMNS}, (xmax = 0) AS inserted"
        );
        let raw = sqlx::query(&sql)
            .bind(input.workspace_id.as_uuid())
            .bind(&input.daemon_id)
            .bind(&input.name)
            .bind(&input.runtime_mode)
            .bind(&input.provider)
            .bind(&input.status)
            .bind(&input.device_info)
            .bind(&input.metadata)
            .bind(input.owner_id.map(Id::as_uuid))
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        let inserted: bool = raw.try_get("inserted").map_err(map_sqlx_err)?;
        let row = AgentRuntimeRow::from_row(&raw).map_err(|e| RepoError::Db(e.to_string()))?;
        Ok(RuntimeUpsert { row, inserted })
    }

    /// 自定义 runtime profile 实例的 upsert
    /// （upstream `UpsertAgentRuntimeWithProfile`，`runtime.sql:95`）。
    ///
    /// 仲裁键是 `(workspace_id, daemon_id, profile_id) WHERE profile_id IS NOT NULL`：
    /// 同一台 daemon 可以同时托管内置 provider 与任意多个同 protocol family 的自定义 profile。
    ///
    /// `profile_id` 不存在于本 workspace / 已禁用 → `Err(RepoError::NotFound)` /
    /// `Err(RepoError::Conflict)`，由调用方翻成上游的
    /// 400 `unknown runtime profile: <id>` 与 409 `runtime profile is disabled: <id>`。
    pub async fn upsert_runtime_with_profile(
        &self,
        input: &UpsertRuntime,
    ) -> Result<RuntimeUpsert> {
        let profile_id = input
            .profile_id
            .ok_or_else(|| RepoError::Db("profile_id is required for a custom runtime".into()))?;
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;

        // KEY SHARE 锁住 profile 行，与 `DeleteRuntimeProfile` 的 UPDATE 锁互斥 —— 这是
        // 上游 `LockRuntimeProfileForRegistration` 的意义：关掉「profile 刚被删、实例却写进去了」
        // 的竞争窗口。
        let profile: Option<(String, String, String, bool)> = sqlx::query_as(
            "SELECT display_name, protocol_family, runtime_type, enabled FROM runtime_profile \
             WHERE id = $1 AND workspace_id = $2 FOR KEY SHARE",
        )
        .bind(profile_id.as_uuid())
        .bind(input.workspace_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        let Some((_display_name, protocol_family, runtime_type, enabled)) = profile else {
            tx.rollback().await.ok();
            return Err(RepoError::NotFound);
        };
        if !enabled {
            tx.rollback().await.ok();
            return Err(RepoError::Conflict);
        }

        // provider 以 profile 里存的运行身份为准，不采信 daemon 自报的 type：否则
        // task routing 用的 provider 会与 profile 漂移。
        let mut input = input.clone();
        input.provider = profile_runtime_type(&runtime_type, &protocol_family);

        let sql = format!(
            "INSERT INTO agent_runtime \
                (workspace_id, daemon_id, name, runtime_mode, provider, status, device_info, \
                 metadata, owner_id, profile_id, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now()) \
             ON CONFLICT (workspace_id, daemon_id, profile_id) WHERE profile_id IS NOT NULL \
             DO UPDATE SET \
                name = EXCLUDED.name, \
                runtime_mode = EXCLUDED.runtime_mode, \
                provider = EXCLUDED.provider, \
                status = EXCLUDED.status, \
                device_info = EXCLUDED.device_info, \
                metadata = EXCLUDED.metadata, \
                owner_id = COALESCE(EXCLUDED.owner_id, agent_runtime.owner_id), \
                last_seen_at = now(), \
                updated_at = now() \
             RETURNING {AGENT_RUNTIME_COLUMNS}, (xmax = 0) AS inserted"
        );
        let raw = sqlx::query(&sql)
            .bind(input.workspace_id.as_uuid())
            .bind(&input.daemon_id)
            .bind(&input.name)
            .bind(&input.runtime_mode)
            .bind(&input.provider)
            .bind(&input.status)
            .bind(&input.device_info)
            .bind(&input.metadata)
            .bind(input.owner_id.map(Id::as_uuid))
            .bind(profile_id.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        let inserted: bool = raw.try_get("inserted").map_err(map_sqlx_err)?;
        let row = AgentRuntimeRow::from_row(&raw).map_err(|e| RepoError::Db(e.to_string()))?;

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(RuntimeUpsert { row, inserted })
    }

    /// 机器级共享自定义名（upstream `sharedDaemonCustomName` + `ListDaemonCustomNames`）。
    ///
    /// 全部名字都得非空且一致才算「有机器名」；否则回 `None`（不回退到 hostname）。
    pub async fn shared_daemon_custom_name(
        &self,
        workspace_id: Id,
        daemon_id: &str,
        exclude_id: Id,
    ) -> Result<Option<String>> {
        let names: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT custom_name FROM agent_runtime \
             WHERE workspace_id = $1 AND daemon_id = $2 AND id <> $3",
        )
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .bind(exclude_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        if names.is_empty() {
            return Ok(None);
        }
        let mut shared: Option<String> = None;
        for name in &names {
            let Some(value) = name.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
                return Ok(None);
            };
            match &shared {
                None => shared = Some(value.to_string()),
                Some(first) if first != value => return Ok(None),
                Some(_) => {}
            }
        }
        Ok(shared)
    }

    /// 给刚插入的 runtime 继承机器共享名（upstream `inheritMachineCustomName`，MUL-4217）。
    pub async fn set_runtime_custom_name(&self, runtime_id: Id, custom_name: &str) -> Result<()> {
        sqlx::query("UPDATE agent_runtime SET custom_name = $2, updated_at = now() WHERE id = $1")
            .bind(runtime_id.as_uuid())
            .bind(custom_name)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 把旧 hostname 派生 `daemon_id` 上的 agents/tasks 折进新 runtime 行，再删旧行。
    ///
    /// 上游是带 workspace fence 的单事务（`mergeLegacyRuntime`）。本地保留同一事务边界，
    /// 但**不实现** workspace teardown fence（本仓 workspace 拆除面不属本切片），
    /// 登记在 `docs/32` 偏离表。
    ///
    /// 返回实际合并掉的旧 runtime id；找不到匹配时不报错。
    pub async fn merge_legacy_runtimes(
        &self,
        workspace_id: Id,
        provider: &str,
        new_runtime_id: Id,
        legacy_ids: &[String],
    ) -> Result<Vec<Id>> {
        let mut merged: Vec<Id> = Vec::new();
        for legacy in legacy_ids {
            let legacy = legacy.trim();
            if legacy.is_empty() {
                continue;
            }
            // 大小写不敏感且返回**所有**匹配行：历史上同名大小写漂移可能已经铸出重复行。
            let matches: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM agent_runtime \
                 WHERE workspace_id = $1 AND provider = $2 AND LOWER(daemon_id) = LOWER($3)",
            )
            .bind(workspace_id.as_uuid())
            .bind(provider)
            .bind(legacy)
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx_err)?;

            for old in matches {
                let old_id = Id::from(old);
                if old_id == new_runtime_id || merged.contains(&old_id) {
                    continue;
                }
                let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
                sqlx::query(
                    "UPDATE agent_task_queue SET runtime_id = $1, updated_at = now() \
                     WHERE runtime_id = $2",
                )
                .bind(new_runtime_id.as_uuid())
                .bind(old_id.as_uuid())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
                sqlx::query("UPDATE agent SET runtime_id = $1 WHERE runtime_id = $2")
                    .bind(new_runtime_id.as_uuid())
                    .bind(old_id.as_uuid())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                sqlx::query("UPDATE agent_runtime SET legacy_daemon_id = $2 WHERE id = $1")
                    .bind(new_runtime_id.as_uuid())
                    .bind(legacy)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                sqlx::query("DELETE FROM agent_runtime WHERE id = $1")
                    .bind(old_id.as_uuid())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                tx.commit().await.map_err(map_sqlx_err)?;
                merged.push(old_id);
            }
        }
        Ok(merged)
    }

    // ------------------------------------------------------------- daemon token

    /// 按 `token_hash` 查 daemon token（upstream `GetDaemonTokenByHash`）。
    ///
    /// 过期行**仍会返回**（`expires_at` 由调用方判定），因为上游把「未知 token」与
    /// 「已过期 token」都归到 401 `invalid daemon token`；这里保留区分只为便于日志。
    pub async fn lookup_daemon_token(&self, token_hash: &str) -> Result<Option<DaemonTokenRow>> {
        sqlx::query_as::<_, DaemonTokenRow>(
            "SELECT id, workspace_id, daemon_id, expires_at FROM daemon_token \
             WHERE token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 写一条 daemon token（`mdt_` 凭据的签发面；上游 `CreateDaemonToken`）。
    pub async fn insert_daemon_token(
        &self,
        token_hash: &str,
        workspace_id: Id,
        daemon_id: &str,
        expires_in_secs: i64,
    ) -> Result<Id> {
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO daemon_token (token_hash, workspace_id, daemon_id, expires_at) \
             VALUES ($1, $2, $3, now() + make_interval(secs => $4::double precision)) \
             RETURNING id",
        )
        .bind(token_hash)
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .bind(super::secs_f64(expires_in_secs))
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(Id::from(id))
    }

    // ---------------------------------------------------------------- heartbeat

    /// 心跳：刷新 `last_seen_at` 并把 runtime 置回在线。
    ///
    /// `Ok(None)` = 确认查无此 runtime（上游据此 404 `runtime not found`）；
    /// `Err(_)` = 基础设施故障（上游落 500 `failed to load runtime` / `heartbeat failed`）。
    pub async fn touch_runtime_heartbeat(&self, runtime_id: Id) -> Result<Option<AgentRuntimeRow>> {
        let sql = format!(
            "UPDATE agent_runtime SET status = 'online', last_seen_at = now(), updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_RUNTIME_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRuntimeRow>(&sql)
            .bind(runtime_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_err)
    }

    /// 按 id 读 runtime（上游 `GetAgentRuntime`）。
    pub async fn runtime_by_id(&self, runtime_id: Id) -> Result<Option<AgentRuntimeRow>> {
        let sql = format!("SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime WHERE id = $1");
        sqlx::query_as::<_, AgentRuntimeRow>(&sql)
            .bind(runtime_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_err)
    }

    /// 把指定 runtime 置离线，返回真正被改动的 id（upstream `SetAgentRuntimeOffline`）。
    ///
    /// 不带原因的那一支（daemon 正常停机 / 机器睡了）。带原因的用
    /// [`DaemonRepo::set_runtime_offline_with_reason`]。
    pub async fn set_runtimes_offline(
        &self,
        workspace_id: Id,
        runtime_ids: &[Id],
    ) -> Result<Vec<Id>> {
        if runtime_ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<Uuid> = runtime_ids.iter().map(|id| id.as_uuid()).collect();
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "UPDATE agent_runtime SET status = 'offline', updated_at = now() \
             WHERE workspace_id = $1 AND id = ANY($2) RETURNING id",
        )
        .bind(workspace_id.as_uuid())
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id::from(id)).collect())
    }

    /// 置离线**并记因**（upstream `SetAgentRuntimeOfflineWithReason`，`runtime.sql`）。
    ///
    /// 原因不占独立列，而是像上游那样合并进 `agent_runtime.metadata`
    /// （`metadata || jsonb_build_object('offline_reason', $3::jsonb)`）—— 注册时
    /// `metadata` 会被整体覆写，所以「机器自己修好了、重新注册」自然把原因抹掉。
    /// 只有「用户必须去修」的停机才带原因（MUL-6164），正常停机的 daemon 不带。
    ///
    /// 返回真正被改动的 id（与 [`DaemonRepo::set_runtimes_offline`] 同形）。
    pub async fn set_runtime_offline_with_reason(
        &self,
        workspace_id: Id,
        runtime_id: Id,
        reason: &Value,
    ) -> Result<Vec<Id>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "UPDATE agent_runtime \
             SET status = 'offline', \
                 metadata = metadata || jsonb_build_object('offline_reason', $3::jsonb), \
                 updated_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING id",
        )
        .bind(workspace_id.as_uuid())
        .bind(runtime_id.as_uuid())
        .bind(reason)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id::from(id)).collect())
    }

    /// `register` 的失败-profile 分支需要 profile 的展示名与命令名（用于组装 name / metadata）。
    ///
    /// 返回 `(display_name, command_name)`；profile 不存在或不属于该 workspace 时 `None`。
    pub async fn runtime_profile_meta(
        &self,
        workspace_id: Id,
        profile_id: Id,
    ) -> Result<Option<(String, String)>> {
        sqlx::query_as(
            "SELECT display_name, command_name FROM runtime_profile \
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(profile_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `GET /api/daemon/workspaces/{id}/runtime-profiles`：workspace 的 profile 列表。
    pub async fn list_runtime_profiles(&self, workspace_id: Id) -> Result<Vec<Value>> {
        let rows: Vec<(Value,)> = sqlx::query_as(
            "SELECT to_jsonb(p) FROM runtime_profile p \
             WHERE p.workspace_id = $1 ORDER BY p.created_at ASC",
        )
        .bind(workspace_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(v,)| v).collect())
    }

    // ---------------------------------------------------------------- tasks

    /// ws 握手用：某 daemon 在该 workspace 下已登记的全部 runtime id。
    ///
    /// 上游在 upgrade 时按 daemon token 批量鉴权并把这些 runtime 放进连接租约；本地
    /// 直接用这个列表构造 `ClientIdentity::runtime_ids`（无租约缓存，见 `docs/32`）。
    pub async fn runtime_ids_for_daemon(
        &self,
        workspace_id: Id,
        daemon_id: &str,
    ) -> Result<Vec<Id>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM agent_runtime WHERE workspace_id = $1 AND daemon_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id::from(id)).collect())
    }

    /// 批量按 id 读 runtime（批量 claim 的 `getAgentRuntimes` 替代；返回顺序未定义）。
    pub async fn list_runtimes_by_ids(
        &self,
        ids: &[Id],
    ) -> Result<Vec<crate::runtime::AgentRuntimeRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<Uuid> = ids.iter().map(|id| id.as_uuid()).collect();
        sqlx::query_as::<_, crate::runtime::AgentRuntimeRow>(&format!(
            "SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime WHERE id = ANY($1)"
        ))
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }
}
