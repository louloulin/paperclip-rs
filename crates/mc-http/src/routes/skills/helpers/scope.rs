use super::*;

/// 一次请求的「workspace + 调用者 + repo」三元组（上游 `resolveWorkspaceID` 家族）。
///
/// **角色不在 `resolve` 里查**：上游只有 `canManageSkill` 查成员身份，`ListSkills` /
/// `GetSkill` / `ListSkillFiles` 连成员都不查（只按 `workspace_id` 收窄）——提前查会把
/// 「非成员读列表」从 200 变成 404。
pub(crate) struct SkillScope {
    pub(crate) workspace_id: Id,
    pub(crate) user_id: Id,
    pub(crate) repo: SkillRepo,
    state: std::sync::Arc<AppState>,
}

impl SkillScope {
    pub(crate) fn resolve(
        state: &std::sync::Arc<AppState>,
        user: &crate::routes::auth_user::AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            workspace_id: resolve_workspace_id(headers, query)?,
            user_id: user.id(),
            repo: SkillRepo::new(state.db.clone()),
            state: state.clone(),
        })
    }

    /// 上游 `loadSkillForUser`：workspace 收窄 + 主键取值；取不到 ⇒ 404 `skill not found`。
    ///
    /// 偏离（已登记 `docs/32` §9.6）：上游把 `GetSkillInWorkspace` 的**任何**错误都折成
    /// 404，本仓按既有切片口径区分为 `Db` ⇒ 500（拿不到库不等于不存在）。
    pub(crate) async fn load_skill(&self, raw_id: &str) -> Result<SkillRow, Error> {
        let skill_id = Id(parse_uuid(raw_id, "skill id")?);
        self.repo
            .get_in_workspace(self.workspace_id, skill_id)
            .await
            .map_err(|e| repo_err(e, "skill"))
    }

    /// 上游 `canManageSkill`：非成员 ⇒ 404，owner/admin 放行，创建者放行，其余 403。
    /// 上游白名单 `("owner","admin","member")` 含 `member` ⇒ 它实际只校验「是不是成员」，
    /// 403 那条才是真正的语义。
    pub(crate) async fn require_can_manage(&self, skill: &SkillRow) -> Result<(), Error> {
        let role: Option<String> =
            sqlx::query_scalar("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
                .bind(self.workspace_id.0)
                .bind(self.user_id.0)
                .fetch_optional(self.state.db.pool())
                .await
                .map_err(|e| Error::Database(e.to_string()))?;

        match role.as_deref() {
            // 非成员：上游 requireWorkspaceRole 用 notFoundMsg="skill not found" ⇒ 404。
            None => Err(not_found("skill")),
            Some("owner" | "admin") => Ok(()),
            Some(_) if skill.created_by == Some(self.user_id.0) => Ok(()),
            Some(_) => Err(forbidden("only the skill creator can manage this skill")),
        }
    }
}
