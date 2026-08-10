#![forbid(unsafe_code)]
//! `pc-board-auth` service 层 —— 编排 pc-repos 实现 Node `board-auth.ts` 全部行为。

use std::sync::Arc;

use pc_core::Timestamp;
use pc_repos::board_key::{BoardKeyRepo, BoardKeyRow};
use pc_repos::cli_challenge::{ChallengeRepo, ChallengeRow};
use pc_repos::company::CompanyRepo;
use pc_repos::company_member::CompanyMemberRepo;
use pc_repos::instance_user_role::InstanceUserRoleRepo;
use pc_repos::user_profile::UserProfileRepo;
use pc_repos::Db;
use uuid::Uuid;

use crate::types::{
    BoardAccess, BoardApiKeyCreated, BoardApiKeyListItem, BoardAuthServiceError,
    BoardAuthServiceResult, BoardMembership, BoardUserSummary, ChallengeStatus,
    CliAuthChallengeCreated, CliAuthChallengeDescription, CliAuthChallengeRow,
    CliRequestedAccess,
};

use crate::{
    board_api_key_expires_at, challenge_status_for_row, cli_auth_challenge_expires_at,
    create_board_api_token, create_cli_auth_secret, hash_bearer_token, token_hashes_match,
    BOARD_API_KEY_TTL_MS, CLI_AUTH_CHALLENGE_TTL_MS,
};

/// 时钟源 trait —— 注入用于可测的"当前时间"。
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

#[derive(Clone)]
pub struct BoardAuthService {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl BoardAuthService {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            clock: Arc::new(SystemClock),
        }
    }

    pub fn with_clock(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// 解析用户的 board 访问上下文。
    pub async fn resolve_board_access(&self, user_id: &str) -> BoardAuthServiceResult<BoardAccess> {
        let user_repo = UserProfileRepo::new(&self.db);
        let member_repo = CompanyMemberRepo::new(&self.db);
        let admin_repo = InstanceUserRoleRepo::new(&self.db);

        let user_summary = user_repo
            .find_summary_by_id(user_id)
            .await?
            .map(|row| BoardUserSummary {
                id: row.id,
                name: row.name,
                email: row.email,
            });

        let member_pairs = member_repo
            .list_active_for_principal_user(user_id)
            .await?;
        let memberships: Vec<BoardMembership> = member_pairs
            .into_iter()
            .map(|(company_id, role)| BoardMembership {
                company_id: Uuid::parse_str(&company_id).unwrap_or_else(|_| Uuid::nil()),
                membership_role: Some(role),
                status: "active".to_string(),
            })
            .collect();
        let company_ids: Vec<Uuid> = memberships.iter().map(|m| m.company_id).collect();

        let is_instance_admin = admin_repo.is_admin(user_id).await?;

        Ok(BoardAccess {
            user: user_summary,
            company_ids,
            memberships,
            is_instance_admin,
        })
    }

    /// 解析用户在 board 上可"活动"的 company 集合。
    ///
    /// 优先级：
    /// 1. membership（active）→ 用户的 company
    /// 2. 若空但 `requested_company_id` 提供 → 加上它
    /// 3. 若空但 `board_api_key_id` 提供 → 加上该 key 对应的 challenge 中所有 requested_company_id
    /// 4. 若空且用户是 instance admin → 加上所有 company
    pub async fn resolve_board_activity_company_ids(
        &self,
        user_id: &str,
        requested_company_id: Option<&str>,
        board_api_key_id: Option<&str>,
    ) -> BoardAuthServiceResult<Vec<Uuid>> {
        let access = self.resolve_board_access(user_id).await?;
        let mut company_ids: std::collections::BTreeSet<Uuid> =
            access.company_ids.iter().copied().collect();

        if company_ids.is_empty() {
            if let Some(rid) = requested_company_id.map(str::trim).filter(|s| !s.is_empty()) {
                if let Ok(id) = Uuid::parse_str(rid) {
                    company_ids.insert(id);
                }
            }
        }

        if company_ids.is_empty() {
            if let Some(bid) = board_api_key_id.map(str::trim).filter(|s| !s.is_empty()) {
                if let Ok(id) = Uuid::parse_str(bid) {
                    let challenge_repo = ChallengeRepo::new(&self.db);
                    let companies = challenge_repo
                        .list_requested_company_ids_by_board_key(id)
                        .await?;
                    for c in companies {
                        company_ids.insert(c);
                    }
                }
            }
        }

        if company_ids.is_empty() && access.is_instance_admin {
            let company_repo = CompanyRepo::new(&self.db);
            let all = company_repo.list_ids().await?;
            for c in all {
                company_ids.insert(c);
            }
        }

        Ok(company_ids.into_iter().collect())
    }

    /// 按 token 明文查找 board api key（先 hash，再查 DB）。
    ///
    /// 返回 `Some(row)` 仅当：未撤销，且 `expires_at` 为 None 或 > now。
    pub async fn find_board_api_key_by_token(
        &self,
        token: &str,
    ) -> BoardAuthServiceResult<Option<BoardKeyRow>> {
        let key_hash = hash_bearer_token(token);
        let repo = BoardKeyRepo::new(&self.db);
        let row = repo.find_by_token_hash(&key_hash).await?;
        let now_ms = self.now_ms();
        Ok(row.filter(|r| {
            r.revoked_at.is_none()
                && r.expires_at
                    .map(|e| e.timestamp_millis() > now_ms)
                    .unwrap_or(true)
        }))
    }

    /// 标记 board api key 最近被使用。
    pub async fn touch_board_api_key(&self, key_id: Uuid) -> BoardAuthServiceResult<()> {
        let repo = BoardKeyRepo::new(&self.db);
        repo.touch(key_id).await?;
        Ok(())
    }

    /// 撤销 board api key（仅在 user_id 匹配时）。
    ///
    /// 返回被撤销的行（如果之前未撤销）。
    pub async fn revoke_board_api_key(
        &self,
        key_id: Uuid,
        user_id: &str,
    ) -> BoardAuthServiceResult<Option<BoardKeyRow>> {
        let repo = BoardKeyRepo::new(&self.db);
        let existing = repo.find_by_id_and_user(key_id, user_id).await?;
        if existing.is_none() {
            return Ok(None);
        }
        repo.revoke(key_id, user_id).await?;
        // 重新读取以返回最新状态
        let updated = repo.find_by_id_and_user(key_id, user_id).await?;
        Ok(updated)
    }

    /// 创建一条命名 board api key。
    pub async fn create_named_board_api_key(
        &self,
        user_id: &str,
        name: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> BoardAuthServiceResult<BoardApiKeyCreated> {
        let token = create_board_api_token();
        let key_hash = hash_bearer_token(&token);
        let exp = match expires_at {
            Some(t) => Some(pc_core::Timestamp::from_dt(t)),
            None => Some(pc_core::Timestamp::from_timestamp_millis(
                self.now_ms() + BOARD_API_KEY_TTL_MS,
            )),
        };
        let repo = BoardKeyRepo::new(&self.db);
        let row = repo
            .create(user_id, name.trim(), &key_hash, exp)
            .await?;
        Ok(BoardApiKeyCreated {
            id: row.id,
            name: row.name,
            token,
            created_at: row.created_at.as_datetime(),
            last_used_at: row.last_used_at.map(|t| t.as_datetime()),
            revoked_at: row.revoked_at.map(|t| t.as_datetime()),
            expires_at: row.expires_at.map(|t| t.as_datetime()),
        })
    }

    /// 列出 board api key。
    ///
    /// `include_inactive=false` 时排除 revoked + 已过期；`true` 时全列。
    pub async fn list_board_api_keys(
        &self,
        user_id: &str,
        include_inactive: bool,
    ) -> BoardAuthServiceResult<Vec<BoardApiKeyListItem>> {
        let repo = BoardKeyRepo::new(&self.db);
        let rows = repo.list_by_user(user_id, include_inactive).await?;
        Ok(rows
            .into_iter()
            .map(|r| BoardApiKeyListItem {
                id: r.id,
                name: r.name,
                created_at: r.created_at.as_datetime(),
                last_used_at: r.last_used_at.map(|t| t.as_datetime()),
                revoked_at: r.revoked_at.map(|t| t.as_datetime()),
                expires_at: r.expires_at.map(|t| t.as_datetime()),
            })
            .collect())
    }

    /// 按 id + user_id 查找一个 board api key（assertCurrentBoardKey 路径）。
    pub async fn get_board_api_key_for_user(
        &self,
        key_id: Uuid,
        user_id: &str,
    ) -> BoardAuthServiceResult<Option<BoardKeyRow>> {
        let repo = BoardKeyRepo::new(&self.db);
        let row = repo.find_by_id_and_user(key_id, user_id).await?;
        Ok(row)
    }

    /// 创建 CLI auth challenge（含明文 secret + pending board token）。
    pub async fn create_cli_auth_challenge(
        &self,
        command: &str,
        client_name: Option<&str>,
        requested_access: CliRequestedAccess,
        requested_company_id: Option<Uuid>,
    ) -> BoardAuthServiceResult<CliAuthChallengeCreated> {
        let challenge_secret = create_cli_auth_secret();
        let pending_board_token = create_board_api_token();
        let expires_at = pc_core::Timestamp::from_timestamp_millis(
            self.now_ms() + CLI_AUTH_CHALLENGE_TTL_MS,
        );
        let label_base = client_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("paperclipai cli");
        let pending_key_name = match requested_access {
            CliRequestedAccess::InstanceAdminRequired => {
                format!("{label_base} (instance admin)")
            }
            CliRequestedAccess::Board => format!("{label_base} (board)"),
        };

        let repo = ChallengeRepo::new(&self.db);
        let row: ChallengeRow = repo
            .create(
                &hash_bearer_token(&challenge_secret),
                command.trim(),
                client_name.map(str::trim).filter(|s| !s.is_empty()),
                requested_access.as_str(),
                requested_company_id,
                &hash_bearer_token(&pending_board_token),
                &pending_key_name,
                expires_at,
            )
            .await?;
        Ok(CliAuthChallengeCreated {
            challenge: row_to_challenge_row(row),
            challenge_secret,
            pending_board_token,
        })
    }

    /// 按 id 找 CLI auth challenge（不过滤状态）。
    pub async fn get_cli_auth_challenge(
        &self,
        id: Uuid,
    ) -> BoardAuthServiceResult<Option<CliAuthChallengeRow>> {
        let repo = ChallengeRepo::new(&self.db);
        Ok(repo.find_by_id(id).await?.map(row_to_challenge_row))
    }

    /// 按 id + secret token 校验（hash 比对），返回匹配的行。
    pub async fn get_cli_auth_challenge_by_secret(
        &self,
        id: Uuid,
        token: &str,
    ) -> BoardAuthServiceResult<Option<CliAuthChallengeRow>> {
        let challenge = self.get_cli_auth_challenge(id).await?;
        let Some(challenge) = challenge else {
            return Ok(None);
        };
        let expected = hash_bearer_token(token);
        if !token_hashes_match(&challenge.secret_hash, &expected) {
            return Ok(None);
        }
        Ok(Some(challenge))
    }

    /// 描述 CLI auth challenge（含状态 + 公司 + 审批人）。
    pub async fn describe_cli_auth_challenge(
        &self,
        id: Uuid,
        token: &str,
    ) -> BoardAuthServiceResult<Option<CliAuthChallengeDescription>> {
        let challenge = self.get_cli_auth_challenge_by_secret(id, token).await?;
        let Some(challenge) = challenge else {
            return Ok(None);
        };

        let user_repo = UserProfileRepo::new(&self.db);
        let company_repo = CompanyRepo::new(&self.db);

        let (company_name, approved_by) = futures::try_join!(
            async {
                Ok::<_, BoardAuthServiceError>(
                    if let Some(cid) = challenge.requested_company_id {
                        company_repo.find_name_by_id(cid).await?
                    } else {
                        None
                    },
                )
            },
            async {
                Ok::<_, BoardAuthServiceError>(
                    if let Some(uid) = challenge.approved_by_user_id.clone() {
                        user_repo
                            .find_summary_by_id(&uid)
                            .await?
                            .map(|u| BoardUserSummary {
                                id: u.id,
                                name: u.name,
                                email: u.email,
                            })
                    } else {
                        None
                    },
                )
            },
        )?;

        Ok(Some(CliAuthChallengeDescription {
            id: challenge.id,
            status: challenge_status_for_row(&challenge, self.now_ms()),
            command: challenge.command,
            client_name: challenge.client_name,
            requested_access: challenge.requested_access,
            requested_company_id: challenge.requested_company_id,
            requested_company_name: company_name,
            approved_at: challenge.approved_at,
            cancelled_at: challenge.cancelled_at,
            expires_at: challenge.expires_at,
            approved_by_user: approved_by,
        }))
    }

    /// 批准 CLI auth challenge（事务内创建 board key 并回填）。
    pub async fn approve_cli_auth_challenge(
        &self,
        id: Uuid,
        token: &str,
        user_id: &str,
    ) -> BoardAuthServiceResult<(ChallengeStatus, CliAuthChallengeRow)> {
        let challenge = self
            .get_cli_auth_challenge_by_secret(id, token)
            .await?
            .ok_or_else(|| {
                BoardAuthServiceError::NotFound("CLI auth challenge not found".into())
            })?;

        let status = challenge_status_for_row(&challenge, self.now_ms());
        if matches!(
            status,
            ChallengeStatus::Expired | ChallengeStatus::Cancelled | ChallengeStatus::Approved
        ) {
            return Ok((status, challenge));
        }

        // instance_admin_required 检查
        if challenge.requested_access == CliRequestedAccess::InstanceAdminRequired.as_str() {
            let access = self.resolve_board_access(user_id).await?;
            if !access.is_instance_admin {
                return Err(BoardAuthServiceError::Forbidden(
                    "Instance admin required".into(),
                ));
            }
        }

        // 在事务中：先创建 board key（如还没有），再原子回填 challenge
        let mut tx = self.db.pool().begin().await?;
        let challenge_repo = ChallengeRepo::new(&self.db);
        let board_repo = BoardKeyRepo::new(&self.db);

        let mut board_key_id = challenge.board_api_key_id;
        if board_key_id.is_none() {
            let expires_at = pc_core::Timestamp::from_timestamp_millis(
                self.now_ms() + BOARD_API_KEY_TTL_MS,
            );
            let row = sqlx::query_as::<_, BoardKeyRow>(
                "INSERT INTO board_api_keys (user_id, name, key_hash, expires_at) \
                 VALUES ($1, $2, $3, $4) \
                 RETURNING id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at",
            )
            .bind(user_id)
            .bind(&challenge.pending_key_name)
            .bind(&challenge.pending_key_hash)
            .bind(expires_at)
            .fetch_one(&mut *tx)
            .await?;
            board_key_id = Some(row.id);
        }

        // 原子回填 approved + board_api_key_id
        let updated: ChallengeRow = sqlx::query_as(
            "UPDATE cli_auth_challenges SET \
                approved_by_user_id = $2, \
                board_api_key_id = $3, \
                approved_at = COALESCE(approved_at, now()), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id, \
                       pending_key_hash, pending_key_name, approved_by_user_id, approved_at, \
                       cancelled_at, expires_at, created_at, board_api_key_id",
        )
        .bind(id)
        .bind(user_id)
        .bind(board_key_id)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        // 抑制 unused warning
        let _ = (challenge_repo, board_repo);

        Ok((ChallengeStatus::Approved, row_to_challenge_row(updated)))
    }

    /// 取消 CLI auth challenge（仅 secret 匹配且未过期/未取消）。
    pub async fn cancel_cli_auth_challenge(
        &self,
        id: Uuid,
        token: &str,
    ) -> BoardAuthServiceResult<(ChallengeStatus, CliAuthChallengeRow)> {
        let challenge = self
            .get_cli_auth_challenge_by_secret(id, token)
            .await?
            .ok_or_else(|| {
                BoardAuthServiceError::NotFound("CLI auth challenge not found".into())
            })?;

        let status = challenge_status_for_row(&challenge, self.now_ms());
        if !matches!(status, ChallengeStatus::Pending) {
            return Ok((status, challenge));
        }

        let mut tx = self.db.pool().begin().await?;
        let updated: ChallengeRow = sqlx::query_as(
            "UPDATE cli_auth_challenges SET cancelled_at = now(), updated_at = now() \
             WHERE id = $1 \
             RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id, \
                       pending_key_hash, pending_key_name, approved_by_user_id, approved_at, \
                       cancelled_at, expires_at, created_at, board_api_key_id",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok((ChallengeStatus::Cancelled, row_to_challenge_row(updated)))
    }

    /// 断言当前请求的 board api key 仍有效（未撤销）。
    pub async fn assert_current_board_key(
        &self,
        key_id: Option<Uuid>,
        user_id: Option<&str>,
    ) -> BoardAuthServiceResult<BoardKeyRow> {
        let key_id = key_id.ok_or_else(|| {
            BoardAuthServiceError::Conflict("Board API key context is required".into())
        })?;
        let user_id = user_id.ok_or_else(|| {
            BoardAuthServiceError::Conflict("Board API key context is required".into())
        })?;
        let repo = BoardKeyRepo::new(&self.db);
        let key = repo
            .find_by_id_and_user(key_id, user_id)
            .await?
            .ok_or_else(|| {
                BoardAuthServiceError::NotFound("Board API key not found".into())
            })?;
        if key.revoked_at.is_some() {
            return Err(BoardAuthServiceError::NotFound(
                "Board API key not found".into(),
            ));
        }
        Ok(key)
    }
}

/// 工厂函数 —— 与 Node `boardAuthService(db)` 1:1 对齐。
pub fn board_auth_service(db: Db) -> BoardAuthService {
    BoardAuthService::new(db)
}

/// 把 repo 返回的 `ChallengeRow` 转成 service 层的 `CliAuthChallengeRow`。
fn row_to_challenge_row(row: ChallengeRow) -> CliAuthChallengeRow {
    CliAuthChallengeRow {
        id: row.id,
        secret_hash: row.secret_hash,
        command: row.command,
        client_name: row.client_name,
        requested_access: row.requested_access,
        requested_company_id: row.requested_company_id,
        pending_key_hash: row.pending_key_hash,
        pending_key_name: row.pending_key_name,
        approved_by_user_id: row.approved_by_user_id,
        approved_at: row.approved_at.map(|t| t.as_datetime()),
        cancelled_at: row.cancelled_at.map(|t| t.as_datetime()),
        expires_at: row.expires_at.as_datetime(),
        created_at: row.created_at.as_datetime(),
        board_api_key_id: row.board_api_key_id,
    }
}

// 抑制 unused 警告：BOARD_API_KEY_TTL_MS / CLI_AUTH_CHALLENGE_TTL_MS 常量
// 在 lib.rs 中已使用（作为 expires_at 计算的基线），这里仅保留引用
// 让 rustc 知道本模块知道这些常量。
#[allow(dead_code)]
const _BOARD_API_KEY_TTL_MS: i64 = BOARD_API_KEY_TTL_MS;
#[allow(dead_code)]
const _CLI_AUTH_CHALLENGE_TTL_MS: i64 = CLI_AUTH_CHALLENGE_TTL_MS;
