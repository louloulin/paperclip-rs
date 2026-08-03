//! `routine` 域。

use futures_util::future::TryFutureExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_issue_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    pub priority: String,
    pub status: String,
    pub concurrency_policy: String,
    pub catch_up_policy: String,
    pub activity_gate_policy: String,
    pub activity_gate_scope: String,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub variables: serde_json::Value,
    pub env: Option<serde_json::Value>,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub last_triggered_at: Option<Timestamp>,
    pub last_enqueued_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRevisionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub routine_id: Uuid,
    pub revision_number: i32,
    pub title: String,
    pub description: Option<String>,
    pub snapshot: serde_json::Value,
    pub change_summary: Option<String>,
    pub restored_from_revision_id: Option<Uuid>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRunRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub routine_id: Uuid,
    pub trigger_id: Option<Uuid>,
    pub source: String,
    pub status: String,
    pub triggered_at: Timestamp,
    pub routine_revision_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub trigger_payload: Option<serde_json::Value>,
    pub dispatch_fingerprint: Option<String>,
    pub linked_issue_id: Option<Uuid>,
    pub coalesced_into_run_id: Option<Uuid>,
    pub failure_reason: Option<String>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDescriptionDocumentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub routine_id: Uuid,
    pub key: String,
    pub title: Option<String>,
    pub format: String,
    pub body: String,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineTriggerRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub routine_id: Uuid,
    pub kind: String,
    pub label: Option<String>,
    pub enabled: bool,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub next_run_at: Option<Timestamp>,
    pub last_fired_at: Option<Timestamp>,
    pub public_id: Option<String>,
    pub secret_id: Option<Uuid>,
    pub signing_mode: Option<String>,
    pub replay_window_sec: Option<i32>,
    pub last_rotated_at: Option<Timestamp>,
    pub last_result: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
const COLS: &str = "id, company_id, project_id, folder_id, goal_id, parent_issue_id, \
    title, description, assignee_agent_id, priority, status, \
    concurrency_policy, catch_up_policy, activity_gate_policy, activity_gate_scope, \
    origin_kind, origin_id, variables, env, latest_revision_id, latest_revision_number, \
    created_by_agent_id, created_by_user_id, responsible_user_id, updated_by_agent_id, \
    updated_by_user_id, last_triggered_at, last_enqueued_at, created_at, updated_at";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineSnapshotRecord {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_issue_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    pub priority: String,
    pub status: String,
    pub concurrency_policy: String,
    pub catch_up_policy: String,
    pub activity_gate_policy: String,
    pub activity_gate_scope: String,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub variables: serde_json::Value,
    pub env: Option<serde_json::Value>,
    pub responsible_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineTriggerSnapshotRecord {
    pub id: Uuid,
    pub kind: String,
    pub label: Option<String>,
    pub enabled: bool,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub public_id: Option<String>,
    pub signing_mode: Option<String>,
    pub replay_window_sec: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRevisionSnapshotRecord {
    pub version: i32,
    pub routine: RoutineSnapshotRecord,
    pub triggers: Vec<RoutineTriggerSnapshotRecord>,
}

#[derive(Debug, Clone)]
pub struct CreateRoutineRecord {
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_issue_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    pub priority: String,
    pub status: String,
    pub concurrency_policy: String,
    pub catch_up_policy: String,
    pub activity_gate_policy: String,
    pub activity_gate_scope: String,
    pub variables: serde_json::Value,
    pub env: Option<serde_json::Value>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct RunRoutineRecord {
    pub trigger_id: Option<Uuid>,
    pub source: String,
    pub payload: Option<serde_json::Value>,
    pub variables: Option<serde_json::Value>,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub assignee_agent_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub execution_workspace_id: Option<Uuid>,
    pub execution_workspace_preference: Option<String>,
    pub execution_workspace_settings: Option<serde_json::Value>,
    pub actor_agent_id: Option<Uuid>,
    pub actor_user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DispatchedRoutineRun {
    pub run: RoutineRunRow,
    pub heartbeat_run_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineIssueSummary {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRunTriggerSummary {
    pub id: Uuid,
    pub kind: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineRunSummary {
    #[serde(flatten)]
    pub run: RoutineRunRow,
    #[serde(rename = "linkedIssue")]
    pub linked_issue: Option<RoutineIssueSummary>,
    pub trigger: Option<RoutineRunTriggerSummary>,
}

#[derive(Debug, Clone)]
pub struct CreateRoutineTriggerRecord {
    pub kind: String,
    pub label: Option<String>,
    pub enabled: bool,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub next_run_at: Option<Timestamp>,
    pub public_id: Option<String>,
    pub secret_id: Option<Uuid>,
    pub signing_mode: Option<String>,
    pub replay_window_sec: Option<i32>,
    pub actor_agent_id: Option<Uuid>,
    pub actor_user_id: Option<String>,
    pub actor_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateRoutineTriggerRecord {
    pub label: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub cron_expression: Option<Option<String>>,
    pub timezone: Option<Option<String>>,
    pub next_run_at: Option<Option<Timestamp>>,
    pub signing_mode: Option<Option<String>>,
    pub replay_window_sec: Option<Option<i32>>,
    pub actor_agent_id: Option<Uuid>,
    pub actor_user_id: Option<String>,
    pub actor_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineTriggerMutationResult {
    pub trigger: RoutineTriggerRow,
    pub secret_material: Option<serde_json::Value>,
    pub revision: RoutineRevisionRow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineTriggerSecretMaterial {
    #[serde(rename = "webhookUrl")]
    pub webhook_url: String,
    #[serde(rename = "webhookSecret")]
    pub webhook_secret: String,
}

#[derive(Debug, Clone, Default)]
pub struct CreateWebhookSecretInput {
    pub kind: String,
    pub label: Option<String>,
    pub enabled: bool,
    pub signing_mode: Option<String>,
    pub replay_window_sec: Option<i32>,
    pub api_base_url: String,
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct FiredRoutineTrigger {
    pub run: RoutineRunRow,
    pub secret_material: Option<RoutineTriggerSecretMaterial>,
}

#[derive(Debug, Clone, Default)]
pub struct FireTriggerInput {
    pub authorization_header: Option<String>,
    pub signature_header: Option<String>,
    pub hub_signature_header: Option<String>,
    pub timestamp_header: Option<String>,
    pub idempotency_key: Option<String>,
    pub raw_body: Option<Vec<u8>>,
    pub payload: Option<serde_json::Value>,
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRestoreResult {
    pub routine: RoutineRow,
    pub revision: RoutineRevisionRow,
    pub restored_from_revision_id: Uuid,
    pub restored_from_revision_number: i32,
    pub secret_materials: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateRoutineRecord {
    pub project_id: Option<Option<Uuid>>,
    pub folder_id: Option<Option<Uuid>>,
    pub goal_id: Option<Option<Uuid>>,
    pub parent_issue_id: Option<Option<Uuid>>,
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub assignee_agent_id: Option<Option<Uuid>>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub concurrency_policy: Option<String>,
    pub catch_up_policy: Option<String>,
    pub activity_gate_policy: Option<String>,
    pub activity_gate_scope: Option<String>,
    pub variables: Option<serde_json::Value>,
    pub env: Option<Option<serde_json::Value>>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
}

pub struct RoutineRepo<'a> {
    pub db: &'a Db,
}

fn revision_snapshot(
    routine: &RoutineRow,
    triggers: &[RoutineTriggerRow],
) -> RoutineRevisionSnapshotRecord {
    RoutineRevisionSnapshotRecord {
        version: 1,
        routine: RoutineSnapshotRecord {
            id: routine.id,
            company_id: routine.company_id,
            project_id: routine.project_id,
            folder_id: routine.folder_id,
            goal_id: routine.goal_id,
            parent_issue_id: routine.parent_issue_id,
            title: routine.title.clone(),
            description: routine.description.clone(),
            assignee_agent_id: routine.assignee_agent_id,
            priority: routine.priority.clone(),
            status: routine.status.clone(),
            concurrency_policy: routine.concurrency_policy.clone(),
            catch_up_policy: routine.catch_up_policy.clone(),
            activity_gate_policy: routine.activity_gate_policy.clone(),
            activity_gate_scope: routine.activity_gate_scope.clone(),
            origin_kind: routine.origin_kind.clone(),
            origin_id: routine.origin_id.clone(),
            variables: routine.variables.clone(),
            env: routine.env.clone(),
            responsible_user_id: routine.responsible_user_id.clone(),
        },
        triggers: triggers
            .iter()
            .map(|trigger| RoutineTriggerSnapshotRecord {
                id: trigger.id,
                kind: trigger.kind.clone(),
                label: trigger.label.clone(),
                enabled: trigger.enabled,
                cron_expression: trigger.cron_expression.clone(),
                timezone: trigger.timezone.clone(),
                public_id: trigger.public_id.clone(),
                signing_mode: trigger.signing_mode.clone(),
                replay_window_sec: trigger.replay_window_sec,
            })
            .collect(),
    }
}

async fn transaction_triggers(
    connection: &mut PgConnection,
    routine_id: Uuid,
) -> sqlx::Result<Vec<RoutineTriggerRow>> {
    sqlx::query_as::<_, RoutineTriggerRow>(
        "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at \
         FROM routine_triggers WHERE routine_id = $1 ORDER BY created_at ASC, id ASC",
    )
    .bind(routine_id)
    .fetch_all(connection)
    .await
}

async fn sync_description_document(
    connection: &mut PgConnection,
    routine: &RoutineRow,
    change_summary: &str,
    actor_agent_id: Option<Uuid>,
    actor_user_id: Option<&str>,
    actor_run_id: Option<Uuid>,
) -> sqlx::Result<()> {
    let existing: Option<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT d.id, d.latest_body, d.latest_revision_number \
         FROM routine_documents rd INNER JOIN documents d ON d.id = rd.document_id \
         WHERE rd.routine_id = $1 AND rd.key = 'description' FOR UPDATE",
    )
    .bind(routine.id)
    .fetch_optional(&mut *connection)
    .await?;
    let body = routine.description.as_deref().unwrap_or_default();
    if let Some((document_id, latest_body, latest_revision_number)) = existing {
        if latest_body == body {
            return Ok(());
        }
        let next_revision_number = latest_revision_number + 1;
        let revision_id: Uuid = sqlx::query_scalar(
            "INSERT INTO document_revisions (company_id, document_id, revision_number, title, \
                format, body, change_summary, created_by_agent_id, created_by_user_id, created_by_run_id) \
             VALUES ($1,$2,$3,'Routine description','markdown',$4,$5,$6,$7,$8) RETURNING id",
        )
        .bind(routine.company_id)
        .bind(document_id)
        .bind(next_revision_number)
        .bind(body)
        .bind(change_summary)
        .bind(actor_agent_id)
        .bind(actor_user_id)
        .bind(actor_run_id)
        .fetch_one(&mut *connection)
        .await?;
        sqlx::query(
            "UPDATE documents SET title='Routine description', format='markdown', latest_body=$2, \
                latest_revision_id=$3, latest_revision_number=$4, updated_by_agent_id=$5, \
                updated_by_user_id=$6, updated_at=now() WHERE id=$1",
        )
        .bind(document_id)
        .bind(body)
        .bind(revision_id)
        .bind(next_revision_number)
        .bind(actor_agent_id)
        .bind(actor_user_id)
        .execute(&mut *connection)
        .await?;
        sqlx::query("UPDATE routine_documents SET updated_at=now() WHERE document_id=$1")
            .bind(document_id)
            .execute(connection)
            .await?;
    }
    Ok(())
}

async fn append_current_routine_revision(
    connection: &mut PgConnection,
    routine: &RoutineRow,
    change_summary: &str,
    restored_from_revision_id: Option<Uuid>,
    actor_agent_id: Option<Uuid>,
    actor_user_id: Option<&str>,
    actor_run_id: Option<Uuid>,
) -> pc_errors::Result<(RoutineRow, RoutineRevisionRow)> {
    let triggers = transaction_triggers(connection, routine.id)
        .await
        .map_err(|error| pc_errors::internal(format!("load revision triggers: {error}")))?;
    let snapshot = revision_snapshot(routine, &triggers);
    let next_revision_number = routine.latest_revision_number + 1;
    let revision = sqlx::query_as::<_, RoutineRevisionRow>(
        "INSERT INTO routine_revisions (company_id, routine_id, revision_number, title, \
            description, snapshot, change_summary, restored_from_revision_id, created_by_agent_id, \
            created_by_user_id, created_by_run_id, responsible_user_id) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
         RETURNING id, company_id, routine_id, revision_number, title, description, snapshot, \
            change_summary, restored_from_revision_id, created_by_agent_id, created_by_user_id, \
            created_by_run_id, created_at",
    )
    .bind(routine.company_id)
    .bind(routine.id)
    .bind(next_revision_number)
    .bind(&routine.title)
    .bind(routine.description.as_deref())
    .bind(
        serde_json::to_value(snapshot)
            .map_err(|error| pc_errors::internal(format!("serialize routine revision: {error}")))?,
    )
    .bind(change_summary)
    .bind(restored_from_revision_id)
    .bind(actor_agent_id)
    .bind(actor_user_id)
    .bind(actor_run_id)
    .bind(routine.responsible_user_id.as_deref())
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| pc_errors::internal(format!("append routine revision: {error}")))?;
    let pointer_sql = format!(
        "UPDATE routines SET latest_revision_id=$2, latest_revision_number=$3, updated_at=now() \
         WHERE id=$1 RETURNING {COLS}"
    );
    let updated = sqlx::query_as::<_, RoutineRow>(&pointer_sql)
        .bind(routine.id)
        .bind(revision.id)
        .bind(next_revision_number)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| pc_errors::internal(format!("advance routine revision: {error}")))?;
    sync_description_document(
        connection,
        &updated,
        change_summary,
        actor_agent_id,
        actor_user_id,
        actor_run_id,
    )
    .await
    .map_err(|error| pc_errors::internal(format!("sync routine revision document: {error}")))?;
    Ok((updated, revision))
}

impl<'a> RoutineRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<RoutineRow>> {
        let sql = format!(
            "SELECT {COLS} FROM routines WHERE company_id = $1 ORDER BY created_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_all(&self, limit: i64) -> sqlx::Result<Vec<RoutineRow>> {
        let sql = format!(
            "SELECT {COLS} FROM routines ORDER BY updated_at DESC LIMIT $1"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_by_company_filtered(
        &self,
        company_id: Uuid,
        project_id: Option<Uuid>,
    ) -> sqlx::Result<Vec<RoutineRow>> {
        let sql = format!(
            "SELECT {COLS} FROM routines \
             WHERE company_id = $1 AND ($2::uuid IS NULL OR project_id = $2) \
             ORDER BY updated_at DESC, title ASC LIMIT 200"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(company_id)
            .bind(project_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<RoutineRow>> {
        let sql = format!("SELECT {COLS} FROM routines WHERE id = $1");
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn get_description_document(
        &self,
        routine_id: Uuid,
    ) -> sqlx::Result<Option<RoutineDescriptionDocumentRow>> {
        sqlx::query_as::<_, RoutineDescriptionDocumentRow>(
            "SELECT d.id, d.company_id, rd.routine_id, rd.key, d.title, d.format, \
                    d.latest_body AS body, d.latest_revision_id, d.latest_revision_number, \
                    d.created_by_agent_id, d.created_by_user_id, d.updated_by_agent_id, \
                    d.updated_by_user_id, d.created_at, d.updated_at \
             FROM routine_documents rd \
             INNER JOIN documents d ON d.id = rd.document_id \
             WHERE rd.routine_id = $1 AND rd.key = 'description'",
        )
        .bind(routine_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_with_initial_revision(
        &self,
        input: &CreateRoutineRecord,
    ) -> sqlx::Result<RoutineRow> {
        let mut transaction = self.db.pool().begin().await?;
        let insert_sql = format!(
            "INSERT INTO routines (company_id, project_id, folder_id, goal_id, parent_issue_id, \
                title, description, assignee_agent_id, priority, status, concurrency_policy, \
                catch_up_policy, activity_gate_policy, activity_gate_scope, variables, env, \
                created_by_agent_id, created_by_user_id, responsible_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) \
             RETURNING {COLS}"
        );
        let created = sqlx::query_as::<_, RoutineRow>(&insert_sql)
            .bind(input.company_id)
            .bind(input.project_id)
            .bind(input.folder_id)
            .bind(input.goal_id)
            .bind(input.parent_issue_id)
            .bind(&input.title)
            .bind(input.description.as_deref())
            .bind(input.assignee_agent_id)
            .bind(&input.priority)
            .bind(&input.status)
            .bind(&input.concurrency_policy)
            .bind(&input.catch_up_policy)
            .bind(&input.activity_gate_policy)
            .bind(&input.activity_gate_scope)
            .bind(&input.variables)
            .bind(input.env.as_ref())
            .bind(input.created_by_agent_id)
            .bind(input.created_by_user_id.as_deref())
            .bind(input.responsible_user_id.as_deref())
            .fetch_one(&mut *transaction)
            .await?;

        let snapshot = serde_json::json!({
            "version": 1,
            "routine": {
                "id": created.id,
                "companyId": created.company_id,
                "projectId": created.project_id,
                "folderId": created.folder_id,
                "goalId": created.goal_id,
                "parentIssueId": created.parent_issue_id,
                "title": created.title,
                "description": created.description,
                "assigneeAgentId": created.assignee_agent_id,
                "priority": created.priority,
                "status": created.status,
                "concurrencyPolicy": created.concurrency_policy,
                "catchUpPolicy": created.catch_up_policy,
                "activityGatePolicy": created.activity_gate_policy,
                "activityGateScope": created.activity_gate_scope,
                "originKind": created.origin_kind,
                "originId": created.origin_id,
                "variables": created.variables,
                "env": created.env,
                "responsibleUserId": created.responsible_user_id,
            },
            "triggers": [],
        });
        let revision_id: Uuid = sqlx::query_scalar(
            "INSERT INTO routine_revisions (company_id, routine_id, revision_number, title, \
                description, snapshot, change_summary, created_by_agent_id, created_by_user_id, \
                created_by_run_id, responsible_user_id) \
             VALUES ($1,$2,1,$3,$4,$5,'Created routine',$6,$7,$8,$9) RETURNING id",
        )
        .bind(created.company_id)
        .bind(created.id)
        .bind(&created.title)
        .bind(created.description.as_deref())
        .bind(&snapshot)
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .bind(input.created_by_run_id)
        .bind(input.responsible_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await?;

        let update_sql = format!(
            "UPDATE routines SET latest_revision_id = $2, latest_revision_number = 1, \
                updated_at = now() WHERE id = $1 RETURNING {COLS}"
        );
        let updated = sqlx::query_as::<_, RoutineRow>(&update_sql)
            .bind(created.id)
            .bind(revision_id)
            .fetch_one(&mut *transaction)
            .await?;

        let document_id: Uuid = sqlx::query_scalar(
            "INSERT INTO documents (company_id, title, format, latest_body, latest_revision_number, \
                created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id) \
             VALUES ($1,'Routine description','markdown',$2,1,$3,$4,$3,$4) RETURNING id",
        )
        .bind(updated.company_id)
        .bind(updated.description.as_deref().unwrap_or_default())
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await?;
        let document_revision_id: Uuid = sqlx::query_scalar(
            "INSERT INTO document_revisions (company_id, document_id, revision_number, title, \
                format, body, change_summary, created_by_agent_id, created_by_user_id, created_by_run_id) \
             VALUES ($1,$2,1,'Routine description','markdown',$3,'Created routine',$4,$5,$6) \
             RETURNING id",
        )
        .bind(updated.company_id)
        .bind(document_id)
        .bind(updated.description.as_deref().unwrap_or_default())
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .bind(input.created_by_run_id)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query("UPDATE documents SET latest_revision_id = $2 WHERE id = $1")
            .bind(document_id)
            .bind(document_revision_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO routine_documents (company_id, routine_id, document_id, key) \
             VALUES ($1,$2,$3,'description')",
        )
        .bind(updated.company_id)
        .bind(updated.id)
        .bind(document_id)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        description: Option<&str>,
        assignee_agent_id: Option<Uuid>,
    ) -> sqlx::Result<RoutineRow> {
        let sql = format!(
            "INSERT INTO routines (company_id, title, description, assignee_agent_id) \
             VALUES ($1,$2,$3,$4) RETURNING {COLS}"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(company_id)
            .bind(title)
            .bind(description)
            .bind(assignee_agent_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update_with_revision(
        &self,
        id: Uuid,
        patch: &UpdateRoutineRecord,
    ) -> pc_errors::Result<Option<RoutineRow>> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin routine update: {error}")))?;
        let select_sql = format!("SELECT {COLS} FROM routines WHERE id = $1 FOR UPDATE");
        let Some(mut candidate) = sqlx::query_as::<_, RoutineRow>(&select_sql)
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("load routine for update: {error}")))?
        else {
            return Ok(None);
        };

        if let Some(value) = patch.project_id {
            candidate.project_id = value;
        }
        if let Some(value) = patch.folder_id {
            candidate.folder_id = value;
        }
        if let Some(value) = patch.goal_id {
            candidate.goal_id = value;
        }
        if let Some(value) = patch.parent_issue_id {
            candidate.parent_issue_id = value;
        }
        if let Some(value) = &patch.title {
            candidate.title = value.clone();
        }
        if let Some(value) = &patch.description {
            candidate.description = value.clone();
        }
        if let Some(value) = patch.assignee_agent_id {
            candidate.assignee_agent_id = value;
        }
        if let Some(value) = &patch.priority {
            candidate.priority = value.clone();
        }
        if let Some(value) = &patch.status {
            candidate.status = value.clone();
        }
        if let Some(value) = &patch.concurrency_policy {
            candidate.concurrency_policy = value.clone();
        }
        if let Some(value) = &patch.catch_up_policy {
            candidate.catch_up_policy = value.clone();
        }
        if let Some(value) = &patch.activity_gate_policy {
            candidate.activity_gate_policy = value.clone();
        }
        if let Some(value) = &patch.activity_gate_scope {
            candidate.activity_gate_scope = value.clone();
        }
        if let Some(value) = &patch.variables {
            candidate.variables = value.clone();
        }
        if let Some(value) = &patch.env {
            candidate.env = value.clone();
        }

        let update_sql = format!(
            "UPDATE routines SET project_id=$2, folder_id=$3, goal_id=$4, parent_issue_id=$5, \
                title=$6, description=$7, assignee_agent_id=$8, priority=$9, status=$10, \
                concurrency_policy=$11, catch_up_policy=$12, activity_gate_policy=$13, \
                activity_gate_scope=$14, variables=$15, env=$16, updated_by_agent_id=$17, \
                updated_by_user_id=$18, updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        let updated = sqlx::query_as::<_, RoutineRow>(&update_sql)
            .bind(id)
            .bind(candidate.project_id)
            .bind(candidate.folder_id)
            .bind(candidate.goal_id)
            .bind(candidate.parent_issue_id)
            .bind(&candidate.title)
            .bind(candidate.description.as_deref())
            .bind(candidate.assignee_agent_id)
            .bind(&candidate.priority)
            .bind(&candidate.status)
            .bind(&candidate.concurrency_policy)
            .bind(&candidate.catch_up_policy)
            .bind(&candidate.activity_gate_policy)
            .bind(&candidate.activity_gate_scope)
            .bind(&candidate.variables)
            .bind(candidate.env.as_ref())
            .bind(patch.updated_by_agent_id)
            .bind(patch.updated_by_user_id.as_deref())
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("update routine: {error}")))?;
        let triggers = transaction_triggers(&mut transaction, id)
            .await
            .map_err(|error| pc_errors::internal(format!("load routine triggers: {error}")))?;
        let snapshot = revision_snapshot(&updated, &triggers);
        let next_revision_number = updated.latest_revision_number + 1;
        let revision_id: Uuid =
            sqlx::query_scalar(
                "INSERT INTO routine_revisions (company_id, routine_id, revision_number, title, \
                description, snapshot, change_summary, created_by_agent_id, created_by_user_id, \
                created_by_run_id, responsible_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,'Updated routine',$7,$8,$9,$10) RETURNING id",
            )
            .bind(updated.company_id)
            .bind(updated.id)
            .bind(next_revision_number)
            .bind(&updated.title)
            .bind(updated.description.as_deref())
            .bind(serde_json::to_value(snapshot).map_err(|error| {
                pc_errors::internal(format!("serialize routine snapshot: {error}"))
            })?)
            .bind(patch.updated_by_agent_id)
            .bind(patch.updated_by_user_id.as_deref())
            .bind(patch.created_by_run_id)
            .bind(updated.responsible_user_id.as_deref())
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("append routine revision: {error}")))?;
        let pointer_sql = format!(
            "UPDATE routines SET latest_revision_id=$2, latest_revision_number=$3, updated_at=now() \
             WHERE id=$1 RETURNING {COLS}"
        );
        let updated = sqlx::query_as::<_, RoutineRow>(&pointer_sql)
            .bind(id)
            .bind(revision_id)
            .bind(next_revision_number)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("advance routine revision: {error}")))?;
        sync_description_document(
            &mut transaction,
            &updated,
            "Updated routine",
            patch.updated_by_agent_id,
            patch.updated_by_user_id.as_deref(),
            patch.created_by_run_id,
        )
        .await
        .map_err(|error| pc_errors::internal(format!("sync routine description: {error}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit routine update: {error}")))?;
        Ok(Some(updated))
    }

    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<RoutineRow>> {
        let sql = format!(
            "UPDATE routines SET title=COALESCE($2,title), description=COALESCE($3,description), \
             status=COALESCE($4,status), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(id)
            .bind(title)
            .bind(description)
            .bind(status)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn trigger(&self, id: Uuid) -> sqlx::Result<Option<RoutineRow>> {
        let sql = format!(
            "UPDATE routines SET last_triggered_at=now(), last_enqueued_at=now(), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM routines WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Revisions
    // =========================================================================

    pub async fn list_revisions(&self, routine_id: Uuid) -> sqlx::Result<Vec<RoutineRevisionRow>> {
        sqlx::query_as::<_, RoutineRevisionRow>(
            "SELECT id, company_id, routine_id, revision_number, title, description, snapshot, \
                    change_summary, restored_from_revision_id, created_by_agent_id, \
                    created_by_user_id, created_by_run_id, created_at \
             FROM routine_revisions WHERE routine_id = $1 ORDER BY revision_number DESC",
        )
        .bind(routine_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_revision(
        &self,
        company_id: Uuid,
        routine_id: Uuid,
        revision_number: i32,
        title: &str,
        description: Option<&str>,
        snapshot: &serde_json::Value,
        change_summary: Option<&str>,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<RoutineRevisionRow> {
        sqlx::query_as::<_, RoutineRevisionRow>(
            "INSERT INTO routine_revisions \
                (company_id, routine_id, revision_number, title, description, snapshot, \
                 change_summary, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             RETURNING id, company_id, routine_id, revision_number, title, description, snapshot, \
                    change_summary, restored_from_revision_id, created_by_agent_id, \
                    created_by_user_id, created_by_run_id, created_at",
        )
        .bind(company_id)
        .bind(routine_id)
        .bind(revision_number)
        .bind(title)
        .bind(description)
        .bind(snapshot)
        .bind(change_summary)
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await
    }

    /// 恢复到指定 revision：写入新 revision（内容=旧 revision），更新 routine.latest_revision
    pub async fn restore_revision_by_id(
        &self,
        routine_id: Uuid,
        revision_id: Uuid,
        actor_agent_id: Option<Uuid>,
        actor_user_id: Option<&str>,
        actor_run_id: Option<Uuid>,
    ) -> pc_errors::Result<RoutineRestoreResult> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin routine restore: {error}")))?;
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("load routine for restore: {error}")))?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        let target = sqlx::query_as::<_, RoutineRevisionRow>(
            "SELECT id, company_id, routine_id, revision_number, title, description, snapshot, \
                    change_summary, restored_from_revision_id, created_by_agent_id, \
                    created_by_user_id, created_by_run_id, created_at \
             FROM routine_revisions WHERE id=$1 AND routine_id=$2 AND company_id=$3",
        )
        .bind(revision_id)
        .bind(routine_id)
        .bind(routine.company_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("load routine revision: {error}")))?
        .ok_or_else(|| pc_errors::not_found("Routine revision not found"))?;
        if routine.latest_revision_id == Some(target.id) {
            return Err(pc_errors::conflict(
                "Selected revision is already the latest revision",
            ));
        }
        let snapshot: RoutineRevisionSnapshotRecord =
            serde_json::from_value(target.snapshot.clone()).map_err(|error| {
                pc_errors::unprocessable(format!("Invalid routine revision snapshot: {error}"))
            })?;
        if snapshot.version != 1
            || snapshot.routine.id != routine.id
            || snapshot.routine.company_id != routine.company_id
        {
            return Err(pc_errors::unprocessable(
                "Routine revision snapshot does not match the target routine",
            ));
        }
        let restored_sql = format!(
            "UPDATE routines SET project_id=$2, folder_id=$3, goal_id=$4, parent_issue_id=$5, \
                title=$6, description=$7, assignee_agent_id=$8, priority=$9, status=$10, \
                concurrency_policy=$11, catch_up_policy=$12, activity_gate_policy=$13, \
                activity_gate_scope=$14, variables=$15, env=$16, responsible_user_id=$17, \
                updated_by_agent_id=$18, updated_by_user_id=$19, updated_at=now() \
             WHERE id=$1 RETURNING {COLS}"
        );
        let restored = sqlx::query_as::<_, RoutineRow>(&restored_sql)
            .bind(routine.id)
            .bind(snapshot.routine.project_id)
            .bind(snapshot.routine.folder_id)
            .bind(snapshot.routine.goal_id)
            .bind(snapshot.routine.parent_issue_id)
            .bind(&snapshot.routine.title)
            .bind(snapshot.routine.description.as_deref())
            .bind(snapshot.routine.assignee_agent_id)
            .bind(&snapshot.routine.priority)
            .bind(&snapshot.routine.status)
            .bind(&snapshot.routine.concurrency_policy)
            .bind(&snapshot.routine.catch_up_policy)
            .bind(&snapshot.routine.activity_gate_policy)
            .bind(&snapshot.routine.activity_gate_scope)
            .bind(&snapshot.routine.variables)
            .bind(snapshot.routine.env.as_ref())
            .bind(snapshot.routine.responsible_user_id.as_deref())
            .bind(actor_agent_id)
            .bind(actor_user_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("restore routine snapshot: {error}")))?;

        let trigger_ids: Vec<Uuid> = snapshot.triggers.iter().map(|trigger| trigger.id).collect();
        if trigger_ids.is_empty() {
            sqlx::query("DELETE FROM routine_triggers WHERE company_id=$1 AND routine_id=$2")
                .bind(restored.company_id)
                .bind(restored.id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    pc_errors::internal(format!("remove restored routine triggers: {error}"))
                })?;
        } else {
            sqlx::query(
                "DELETE FROM routine_triggers WHERE company_id=$1 AND routine_id=$2 \
                 AND NOT (id = ANY($3::uuid[]))",
            )
            .bind(restored.company_id)
            .bind(restored.id)
            .bind(&trigger_ids)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                pc_errors::internal(format!("prune restored routine triggers: {error}"))
            })?;
        }
        for trigger in &snapshot.triggers {
            let existing: Option<(Option<Uuid>, Option<String>)> = sqlx::query_as(
                "SELECT secret_id, public_id FROM routine_triggers \
                 WHERE id=$1 AND company_id=$2 AND routine_id=$3",
            )
            .bind(trigger.id)
            .bind(restored.company_id)
            .bind(restored.id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("load restored trigger: {error}")))?;
            let retained_secret_id = existing.as_ref().and_then(|entry| entry.0);
            let retained_public_id = existing
                .as_ref()
                .and_then(|entry| entry.1.clone())
                .or_else(|| trigger.public_id.clone());
            if existing.is_some() {
                sqlx::query(
                    "UPDATE routine_triggers SET kind=$2, label=$3, enabled=$4, cron_expression=$5, \
                        timezone=$6, next_run_at=NULL, public_id=$7, secret_id=$8, signing_mode=$9, \
                        replay_window_sec=$10, updated_by_agent_id=$11, updated_by_user_id=$12, \
                        updated_at=now() WHERE id=$1",
                )
                .bind(trigger.id)
                .bind(&trigger.kind)
                .bind(trigger.label.as_deref())
                .bind(trigger.enabled)
                .bind(trigger.cron_expression.as_deref())
                .bind(trigger.timezone.as_deref())
                .bind(retained_public_id.as_deref())
                .bind(retained_secret_id)
                .bind(trigger.signing_mode.as_deref())
                .bind(trigger.replay_window_sec)
                .bind(actor_agent_id)
                .bind(actor_user_id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| pc_errors::internal(format!("update restored trigger: {error}")))?;
            } else {
                sqlx::query(
                    "INSERT INTO routine_triggers (id, company_id, routine_id, kind, label, enabled, \
                        cron_expression, timezone, public_id, secret_id, signing_mode, replay_window_sec, \
                        created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$13,$14)",
                )
                .bind(trigger.id)
                .bind(restored.company_id)
                .bind(restored.id)
                .bind(&trigger.kind)
                .bind(trigger.label.as_deref())
                .bind(trigger.enabled)
                .bind(trigger.cron_expression.as_deref())
                .bind(trigger.timezone.as_deref())
                .bind(retained_public_id.as_deref())
                .bind(retained_secret_id)
                .bind(trigger.signing_mode.as_deref())
                .bind(trigger.replay_window_sec)
                .bind(actor_agent_id)
                .bind(actor_user_id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| pc_errors::internal(format!("create restored trigger: {error}")))?;
            }
        }

        let restored_triggers = transaction_triggers(&mut transaction, routine_id)
            .await
            .map_err(|error| pc_errors::internal(format!("load restored triggers: {error}")))?;
        let restored_snapshot = revision_snapshot(&restored, &restored_triggers);
        let next_revision_number = routine.latest_revision_number + 1;
        let change_summary = format!("Restored from revision {}", target.revision_number);
        let revision = sqlx::query_as::<_, RoutineRevisionRow>(
            "INSERT INTO routine_revisions (company_id, routine_id, revision_number, title, \
                description, snapshot, change_summary, restored_from_revision_id, \
                created_by_agent_id, created_by_user_id, created_by_run_id, responsible_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             RETURNING id, company_id, routine_id, revision_number, title, description, snapshot, \
                change_summary, restored_from_revision_id, created_by_agent_id, created_by_user_id, \
                created_by_run_id, created_at",
        )
        .bind(restored.company_id)
        .bind(restored.id)
        .bind(next_revision_number)
        .bind(&restored.title)
        .bind(restored.description.as_deref())
        .bind(serde_json::to_value(restored_snapshot).map_err(|error| {
            pc_errors::internal(format!("serialize restored routine snapshot: {error}"))
        })?)
        .bind(&change_summary)
        .bind(target.id)
        .bind(actor_agent_id)
        .bind(actor_user_id)
        .bind(actor_run_id)
        .bind(restored.responsible_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("append restored revision: {error}")))?;
        let pointer_sql = format!(
            "UPDATE routines SET latest_revision_id=$2, latest_revision_number=$3, updated_at=now() \
             WHERE id=$1 RETURNING {COLS}"
        );
        let restored = sqlx::query_as::<_, RoutineRow>(&pointer_sql)
            .bind(restored.id)
            .bind(revision.id)
            .bind(next_revision_number)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| {
                pc_errors::internal(format!("advance restored routine revision: {error}"))
            })?;
        sync_description_document(
            &mut transaction,
            &restored,
            &change_summary,
            actor_agent_id,
            actor_user_id,
            actor_run_id,
        )
        .await
        .map_err(|error| pc_errors::internal(format!("sync restored description: {error}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit routine restore: {error}")))?;
        Ok(RoutineRestoreResult {
            routine: restored,
            revision,
            restored_from_revision_id: target.id,
            restored_from_revision_number: target.revision_number,
            secret_materials: Vec::new(),
        })
    }

    pub async fn restore_revision(
        &self,
        routine_id: Uuid,
        revision_number: i32,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<Option<RoutineRevisionRow>> {
        let target: Option<RoutineRevisionRow> = sqlx::query_as::<_, RoutineRevisionRow>(
            "SELECT id, company_id, routine_id, revision_number, title, description, snapshot, \
                    change_summary, restored_from_revision_id, created_by_agent_id, \
                    created_by_user_id, created_by_run_id, created_at \
             FROM routine_revisions WHERE routine_id = $1 AND revision_number = $2",
        )
        .bind(routine_id)
        .bind(revision_number)
        .fetch_optional(self.db.pool())
        .await?;
        let target = match target {
            Some(t) => t,
            None => return Ok(None),
        };
        let current: Option<i32> =
            sqlx::query_scalar("SELECT latest_revision_number FROM routines WHERE id = $1")
                .bind(routine_id)
                .fetch_optional(self.db.pool())
                .await?;
        let new_num = current.unwrap_or(0) + 1;
        let new_id = Uuid::new_v4();
        let new_rev: RoutineRevisionRow = sqlx::query_as::<_, RoutineRevisionRow>(
            "INSERT INTO routine_revisions \
                (id, company_id, routine_id, revision_number, title, description, snapshot, \
                 change_summary, restored_from_revision_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) \
             RETURNING id, company_id, routine_id, revision_number, title, description, snapshot, \
                    change_summary, restored_from_revision_id, created_by_agent_id, \
                    created_by_user_id, created_by_run_id, created_at",
        )
        .bind(new_id)
        .bind(target.company_id)
        .bind(routine_id)
        .bind(new_num)
        .bind(&target.title)
        .bind(target.description.as_deref())
        .bind(&target.snapshot)
        .bind(format!("Restored from revision {}", revision_number))
        .bind(target.id)
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await?;
        let s = format!(
            "UPDATE routines SET latest_revision_id = $1, latest_revision_number = $2, \
                title = $3, description = $4, updated_at = now() WHERE id = $5"
        );
        sqlx::query(&s)
            .bind(new_id)
            .bind(new_num)
            .bind(&target.title)
            .bind(target.description.as_deref())
            .bind(routine_id)
            .execute(self.db.pool())
            .await?;
        Ok(Some(new_rev))
    }

    // =========================================================================
    // Runs
    // =========================================================================

    pub async fn get_active_issue(
        &self,
        routine_id: Uuid,
    ) -> sqlx::Result<Option<RoutineIssueSummary>> {
        sqlx::query_as::<_, (Uuid, Option<String>, String, String, String, Timestamp)>(
            "SELECT id, identifier, title, status, priority, updated_at FROM issues \
             WHERE origin_kind='routine_execution' AND origin_id=$1 AND hidden_at IS NULL \
               AND status IN ('backlog','todo','in_progress','in_review','blocked') \
             ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(routine_id.to_string())
        .fetch_optional(self.db.pool())
        .await
        .map(|row| {
            row.map(
                |(id, identifier, title, status, priority, updated_at)| RoutineIssueSummary {
                    id,
                    identifier,
                    title,
                    status,
                    priority,
                    updated_at,
                },
            )
        })
    }

    pub async fn list_run_summaries(
        &self,
        routine_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<RoutineRunSummary>> {
        let runs = self.list_runs(routine_id, limit).await?;
        let mut summaries = Vec::with_capacity(runs.len());
        for run in runs {
            let linked_issue = if let Some(issue_id) = run.linked_issue_id {
                sqlx::query_as::<_, (Uuid, Option<String>, String, String, String, Timestamp)>(
                    "SELECT id, identifier, title, status, priority, updated_at FROM issues WHERE id=$1",
                )
                .bind(issue_id)
                .fetch_optional(self.db.pool())
                .await?
                .map(|(id, identifier, title, status, priority, updated_at)| RoutineIssueSummary {
                    id,
                    identifier,
                    title,
                    status,
                    priority,
                    updated_at,
                })
            } else {
                None
            };
            let trigger = if let Some(trigger_id) = run.trigger_id {
                sqlx::query_as::<_, (Uuid, String, Option<String>)>(
                    "SELECT id, kind, label FROM routine_triggers WHERE id=$1",
                )
                .bind(trigger_id)
                .fetch_optional(self.db.pool())
                .await?
                .map(|(id, kind, label)| RoutineRunTriggerSummary { id, kind, label })
            } else {
                None
            };
            summaries.push(RoutineRunSummary {
                run,
                linked_issue,
                trigger,
            });
        }
        Ok(summaries)
    }

    pub async fn dispatch_run(
        &self,
        routine_id: Uuid,
        input: &RunRoutineRecord,
    ) -> pc_errors::Result<DispatchedRoutineRun> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin routine dispatch: {error}")))?;
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("load routine for dispatch: {error}")))?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        if routine.status == "archived" {
            return Err(pc_errors::conflict("Routine is archived"));
        }
        let assignee_agent_id = input
            .assignee_agent_id
            .or(routine.assignee_agent_id)
            .ok_or_else(|| pc_errors::unprocessable("Default agent required"))?;
        let assignable: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM agents WHERE id=$1 AND company_id=$2 AND status <> 'terminated')",
        )
        .bind(assignee_agent_id)
        .bind(routine.company_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("validate routine assignee: {error}")))?;
        if !assignable {
            return Err(pc_errors::unprocessable("Assignee agent is not available"));
        }
        if let Some(trigger_id) = input.trigger_id {
            let trigger: Option<(Uuid, bool)> =
                sqlx::query_as("SELECT routine_id, enabled FROM routine_triggers WHERE id=$1")
                    .bind(trigger_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|error| {
                        pc_errors::internal(format!("validate routine trigger: {error}"))
                    })?;
            let Some((trigger_routine_id, enabled)) = trigger else {
                return Err(pc_errors::not_found("Routine trigger not found"));
            };
            if trigger_routine_id != routine.id {
                return Err(pc_errors::forbidden("Trigger does not belong to routine"));
            }
            if !enabled {
                return Err(pc_errors::conflict("Routine trigger is not active"));
            }
        }
        if let Some(idempotency_key) = input.idempotency_key.as_deref() {
            let existing = sqlx::query_as::<_, RoutineRunRow>(
                "SELECT id, company_id, routine_id, trigger_id, source, status, triggered_at, \
                    routine_revision_id, responsible_user_id, idempotency_key, trigger_payload, \
                    dispatch_fingerprint, linked_issue_id, coalesced_into_run_id, failure_reason, \
                    completed_at, created_at, updated_at FROM routine_runs \
                 WHERE company_id=$1 AND routine_id=$2 AND source=$3 AND idempotency_key=$4 \
                   AND (($5::uuid IS NULL AND trigger_id IS NULL) OR trigger_id=$5) \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(routine.company_id)
            .bind(routine.id)
            .bind(&input.source)
            .bind(idempotency_key)
            .bind(input.trigger_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("check routine idempotency: {error}")))?;
            if let Some(run) = existing {
                let heartbeat_run_id: Option<Uuid> = if let Some(issue_id) = run.linked_issue_id {
                    sqlx::query_scalar("SELECT execution_run_id FROM issues WHERE id=$1")
                        .bind(issue_id)
                        .fetch_optional(&mut *transaction)
                        .await
                        .map_err(|error| {
                            pc_errors::internal(format!("load idempotent heartbeat: {error}"))
                        })?
                        .flatten()
                } else {
                    None
                };
                transaction.commit().await.map_err(|error| {
                    pc_errors::internal(format!("commit idempotent dispatch: {error}"))
                })?;
                return Ok(DispatchedRoutineRun {
                    run,
                    heartbeat_run_id: heartbeat_run_id.unwrap_or_else(Uuid::nil),
                });
            }
        }
        let project_id = input.project_id.or(routine.project_id);
        let responsible_user_id = input
            .actor_user_id
            .clone()
            .or_else(|| routine.responsible_user_id.clone());
        let dispatch_fingerprint = format!("routine:{}:{}", routine.id, Uuid::new_v4());
        let run = sqlx::query_as::<_, RoutineRunRow>(
            "INSERT INTO routine_runs (company_id, routine_id, trigger_id, source, status, \
                routine_revision_id, responsible_user_id, idempotency_key, trigger_payload, \
                dispatch_fingerprint) VALUES ($1,$2,$3,$4,'received',$5,$6,$7,$8,$9) \
             RETURNING id, company_id, routine_id, trigger_id, source, status, triggered_at, \
                routine_revision_id, responsible_user_id, idempotency_key, trigger_payload, \
                dispatch_fingerprint, linked_issue_id, coalesced_into_run_id, failure_reason, \
                completed_at, created_at, updated_at",
        )
        .bind(routine.company_id)
        .bind(routine.id)
        .bind(input.trigger_id)
        .bind(&input.source)
        .bind(routine.latest_revision_id)
        .bind(responsible_user_id.as_deref())
        .bind(input.idempotency_key.as_deref())
        .bind(input.payload.as_ref())
        .bind(&dispatch_fingerprint)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("create routine run: {error}")))?;
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issues (company_id, project_id, project_workspace_id, title, description, \
                status, priority, assignee_agent_id, created_by_agent_id, created_by_user_id, \
                responsible_user_id, origin_kind, origin_id, origin_run_id, origin_fingerprint, \
                execution_workspace_id, execution_workspace_preference, execution_workspace_settings) \
             VALUES ($1,$2,$3,$4,$5,'todo',$6,$7,$8,$9,$10,'routine_execution',$11,$12,$13,$14,$15,$16) \
             RETURNING id",
        )
        .bind(routine.company_id)
        .bind(project_id)
        .bind(input.project_workspace_id)
        .bind(&routine.title)
        .bind(routine.description.as_deref())
        .bind(&routine.priority)
        .bind(assignee_agent_id)
        .bind(input.actor_agent_id)
        .bind(input.actor_user_id.as_deref())
        .bind(responsible_user_id.as_deref())
        .bind(routine.id.to_string())
        .bind(run.id.to_string())
        .bind(&dispatch_fingerprint)
        .bind(input.execution_workspace_id)
        .bind(input.execution_workspace_preference.as_deref())
        .bind(input.execution_workspace_settings.as_ref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("create routine execution issue: {error}")))?;
        let wakeup_request_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_wakeup_requests (company_id, agent_id, source, trigger_detail, reason, \
                payload, status, requested_by_actor_type, requested_by_actor_id, idempotency_key) \
             VALUES ($1,$2,'routine.dispatch',$3,'issue_assigned',$4,'queued',$5,$6,$7) RETURNING id",
        )
        .bind(routine.company_id)
        .bind(assignee_agent_id)
        .bind(format!("routine:{}:issue:{}", routine.id, issue_id))
        .bind(serde_json::json!({
            "issueId": issue_id,
            "routineId": routine.id,
            "routineRunId": run.id,
        }))
        .bind(if input.actor_agent_id.is_some() { "agent" } else { "user" })
        .bind(input.actor_agent_id.map(|id| id.to_string()).or_else(|| input.actor_user_id.clone()))
        .bind(input.idempotency_key.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("queue routine wakeup: {error}")))?;
        let heartbeat_run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO heartbeat_runs (company_id, agent_id, invocation_source, trigger_detail, \
                responsible_user_id, wakeup_request_id, context_snapshot, status) \
             VALUES ($1,$2,'on_demand',$3,$4,$5,$6,'queued') RETURNING id",
        )
        .bind(routine.company_id)
        .bind(assignee_agent_id)
        .bind(format!("routine:{}", routine.id))
        .bind(responsible_user_id.as_deref())
        .bind(wakeup_request_id)
        .bind(serde_json::json!({
            "issueId": issue_id,
            "routineId": routine.id,
            "routineRunId": run.id,
        }))
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("create routine heartbeat: {error}")))?;
        sqlx::query("UPDATE agent_wakeup_requests SET run_id=$2, updated_at=now() WHERE id=$1")
            .bind(wakeup_request_id)
            .bind(heartbeat_run_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("link routine wakeup: {error}")))?;
        sqlx::query("UPDATE issues SET execution_run_id=$2, updated_at=now() WHERE id=$1")
            .bind(issue_id)
            .bind(heartbeat_run_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("link routine heartbeat: {error}")))?;
        let updated_run = sqlx::query_as::<_, RoutineRunRow>(
            "UPDATE routine_runs SET status='issue_created', linked_issue_id=$2, updated_at=now() \
             WHERE id=$1 RETURNING id, company_id, routine_id, trigger_id, source, status, \
                triggered_at, routine_revision_id, responsible_user_id, idempotency_key, \
                trigger_payload, dispatch_fingerprint, linked_issue_id, coalesced_into_run_id, \
                failure_reason, completed_at, created_at, updated_at",
        )
        .bind(run.id)
        .bind(issue_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("finalize routine run: {error}")))?;
        sqlx::query(
            "UPDATE routines SET last_triggered_at=now(), last_enqueued_at=now(), updated_at=now() \
             WHERE id=$1",
        )
        .bind(routine.id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("touch dispatched routine: {error}")))?;
        if let Some(trigger_id) = input.trigger_id {
            sqlx::query(
                "UPDATE routine_triggers SET last_fired_at=now(), last_result='issue_created', \
                    updated_at=now() WHERE id=$1",
            )
            .bind(trigger_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("touch routine trigger: {error}")))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit routine dispatch: {error}")))?;
        Ok(DispatchedRoutineRun {
            run: updated_run,
            heartbeat_run_id,
        })
    }

    pub async fn list_runs(
        &self,
        routine_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<RoutineRunRow>> {
        sqlx::query_as::<_, RoutineRunRow>(
            "SELECT id, company_id, routine_id, trigger_id, source, status, triggered_at, \
                    routine_revision_id, responsible_user_id, idempotency_key, trigger_payload, \
                    dispatch_fingerprint, linked_issue_id, coalesced_into_run_id, failure_reason, \
                    completed_at, created_at, updated_at \
             FROM routine_runs WHERE routine_id = $1 ORDER BY triggered_at DESC LIMIT $2",
        )
        .bind(routine_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_run(
        &self,
        company_id: Uuid,
        routine_id: Uuid,
        trigger_id: Option<Uuid>,
        source: &str,
        trigger_payload: Option<&serde_json::Value>,
    ) -> sqlx::Result<RoutineRunRow> {
        sqlx::query_as::<_, RoutineRunRow>(
            "INSERT INTO routine_runs (company_id, routine_id, trigger_id, source, trigger_payload) \
             VALUES ($1,$2,$3,$4,$5) \
             RETURNING id, company_id, routine_id, trigger_id, source, status, triggered_at, \
                    routine_revision_id, responsible_user_id, idempotency_key, trigger_payload, \
                    dispatch_fingerprint, linked_issue_id, coalesced_into_run_id, failure_reason, \
                    completed_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(routine_id)
        .bind(trigger_id)
        .bind(source)
        .bind(trigger_payload)
        .fetch_one(self.db.pool())
        .await
    }

    // =========================================================================
    // Triggers
    // =========================================================================

    pub async fn list_triggers(&self, routine_id: Uuid) -> sqlx::Result<Vec<RoutineTriggerRow>> {
        sqlx::query_as::<_, RoutineTriggerRow>(
            "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                    next_run_at, last_fired_at, public_id, secret_id, signing_mode, \
                    replay_window_sec, last_rotated_at, last_result, created_by_agent_id, \
                    created_by_user_id, updated_by_agent_id, updated_by_user_id, \
                    created_at, updated_at \
             FROM routine_triggers WHERE routine_id = $1 ORDER BY created_at ASC",
        )
        .bind(routine_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_trigger(&self, id: Uuid) -> sqlx::Result<Option<RoutineTriggerRow>> {
        sqlx::query_as::<_, RoutineTriggerRow>(
            "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                    next_run_at, last_fired_at, public_id, secret_id, signing_mode, \
                    replay_window_sec, last_rotated_at, last_result, created_by_agent_id, \
                    created_by_user_id, updated_by_agent_id, updated_by_user_id, \
                    created_at, updated_at \
             FROM routine_triggers WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn get_trigger_by_public_id(
        &self,
        public_id: &str,
    ) -> sqlx::Result<Option<RoutineTriggerRow>> {
        sqlx::query_as::<_, RoutineTriggerRow>(
            "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                    next_run_at, last_fired_at, public_id, secret_id, signing_mode, \
                    replay_window_sec, last_rotated_at, last_result, created_by_agent_id, \
                    created_by_user_id, updated_by_agent_id, updated_by_user_id, \
                    created_at, updated_at \
             FROM routine_triggers WHERE public_id = $1",
        )
        .bind(public_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_webhook_trigger(
        &self,
        routine_id: Uuid,
        input: &CreateWebhookSecretInput,
        provider: &(dyn pc_secrets::provider::SecretProvider + Send + Sync),
        _secret_repository: &crate::secret::SecretRepositoryRef,
    ) -> pc_errors::Result<RoutineTriggerMutationResult> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin webhook trigger: {error}")))?;
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| {
                pc_errors::internal(format!("load routine for webhook trigger: {error}"))
            })?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        let public_id = Uuid::new_v4().simple().to_string()[..24].to_owned();
        let secret_value = format!("whsec_{}", Uuid::new_v4().simple());
        let write_context = pc_secrets::provider::SecretProviderWriteContext {
            company_id: routine.company_id,
            secret_key: format!("routine_trigger:{}:{}", routine.id, public_id),
            secret_name: format!("Routine webhook {public_id}"),
            version: 1,
        };
        let prepared = provider
            .create_secret(secret_value.clone(), &write_context)
            .await
            .map_err(|error| pc_errors::internal(format!("encrypt webhook secret: {error}")))?;
        let secret_id: Uuid = sqlx::query_scalar(
            "INSERT INTO company_secrets (company_id, name, key, provider, latest_version, \
                status, managed_mode, scope) VALUES ($1,$2,$3,'local_encrypted',1,'active', \
                'paperclip_managed','company') RETURNING id",
        )
        .bind(routine.company_id)
        .bind(write_context.secret_name)
        .bind(write_context.secret_key)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("create webhook secret: {error}")))?;
        let value_sha = hex_digest(secret_value.as_bytes());
        let fingerprint_sha = prepared
            .fingerprint_sha256
            .clone()
            .unwrap_or_else(|| value_sha.clone());
        sqlx::query(
            "INSERT INTO company_secret_versions (secret_id, version, material, value_sha256, \
                fingerprint_sha256, status, created_by_agent_id, created_by_user_id) \
             VALUES ($1,1,$2,$3,$4,'current',$5,$6)",
        )
        .bind(secret_id)
        .bind(&prepared.material)
        .bind(&value_sha)
        .bind(&fingerprint_sha)
        .bind(input.agent_id)
        .bind(input.user_id.as_deref())
        .execute(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("persist webhook secret version: {error}")))?;
        let trigger = sqlx::query_as::<_, RoutineTriggerRow>(
            "INSERT INTO routine_triggers (company_id, routine_id, kind, label, enabled, \
                public_id, secret_id, signing_mode, replay_window_sec, last_rotated_at, \
                created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id) \
             VALUES ($1,$2,'webhook',$3,true,$4,$5,$6,$7,now(),$8,$9,$8,$9) \
             RETURNING id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at",
        )
        .bind(routine.company_id)
        .bind(routine.id)
        .bind(input.label.as_deref())
        .bind(&public_id)
        .bind(secret_id)
        .bind(input.signing_mode.as_deref())
        .bind(input.replay_window_sec)
        .bind(input.agent_id)
        .bind(input.user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("create webhook trigger: {error}")))?;
        let triggers = transaction_triggers(&mut transaction, routine.id)
            .await
            .map_err(|error| {
                pc_errors::internal(format!("load webhook trigger snapshot: {error}"))
            })?;
        let snapshot = revision_snapshot(&routine, &triggers);
        let next_revision_number = routine.latest_revision_number + 1;
        let revision = sqlx::query_as::<_, RoutineRevisionRow>(
            "INSERT INTO routine_revisions (company_id, routine_id, revision_number, title, \
                description, snapshot, change_summary, created_by_agent_id, created_by_user_id, \
                created_by_run_id, responsible_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,'Created webhook trigger',$7,$8,$9,$10) \
             RETURNING id, company_id, routine_id, revision_number, title, description, snapshot, \
                change_summary, restored_from_revision_id, created_by_agent_id, created_by_user_id, \
                created_by_run_id, created_at",
        )
        .bind(routine.company_id)
        .bind(routine.id)
        .bind(next_revision_number)
        .bind(&routine.title)
        .bind(routine.description.as_deref())
        .bind(serde_json::to_value(snapshot).map_err(|error| {
            pc_errors::internal(format!("serialize webhook snapshot: {error}"))
        })?)
        .bind(input.agent_id)
        .bind(input.user_id.as_deref())
        .bind(input.run_id)
        .bind(routine.responsible_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("append webhook revision: {error}")))?;
        sqlx::query("UPDATE routines SET latest_revision_id=$2, latest_revision_number=$3, updated_at=now() WHERE id=$1")
            .bind(routine.id)
            .bind(revision.id)
            .bind(next_revision_number)
            .execute(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("advance webhook revision: {error}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit webhook trigger: {error}")))?;
        let _ = _secret_repository;
        let material_value = serde_json::to_value(RoutineTriggerSecretMaterial {
            webhook_url: format!(
                "{}/api/routine-triggers/public/{}/fire",
                input.api_base_url.trim_end_matches('/'),
                public_id
            ),
            webhook_secret: secret_value,
        })
        .map_err(|error| pc_errors::internal(format!("encode webhook secret: {error}")))?;
        Ok(RoutineTriggerMutationResult {
            trigger,
            secret_material: Some(material_value),
            revision,
        })
    }

    pub async fn fire_public_trigger(
        &self,
        public_id: &str,
        input: &FireTriggerInput,
        provider: &(dyn pc_secrets::provider::SecretProvider + Send + Sync),
    ) -> pc_errors::Result<FiredRoutineTrigger> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin webhook fire: {error}")))?;
        let trigger = sqlx::query_as::<_, RoutineTriggerRow>(
            "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at \
             FROM routine_triggers WHERE public_id=$1 AND kind='webhook' FOR UPDATE",
        )
        .bind(public_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("load webhook trigger: {error}")))?
        .ok_or_else(|| pc_errors::not_found("Routine trigger not found"))?;
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(trigger.routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("lock routine for fire: {error}")))?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        if !trigger.enabled || routine.status != "active" {
            return Err(pc_errors::conflict("Routine trigger is not active"));
        }
        let resolved_secret = if matches!(trigger.signing_mode.as_deref(), Some("none")) {
            None
        } else {
            let secret_id = trigger
                .secret_id
                .ok_or_else(|| pc_errors::conflict("Trigger secret is not provisioned"))?;
            let material: Value = sqlx::query_scalar(
                "SELECT csv.material FROM company_secret_versions csv \
                 WHERE csv.secret_id=$1 ORDER BY csv.version DESC LIMIT 1",
            )
            .bind(secret_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("load trigger secret: {error}")))?
            .ok_or_else(|| pc_errors::conflict("Trigger secret material missing"))?;
            let context = pc_secrets::provider::SecretProviderRuntimeContext {
                company_id: routine.company_id,
                secret_id,
                secret_key: format!("routine_trigger:{}:{}", routine.id, public_id),
                version: 1,
            };
            Some(
                provider
                    .resolve_version(material, &context)
                    .await
                    .map_err(|error| {
                        pc_errors::internal(format!("decrypt trigger secret: {error}"))
                    })?,
            )
        };
        let raw_body = input.raw_body.clone().unwrap_or_else(|| {
            input
                .payload
                .as_ref()
                .map(|value| serde_json::to_vec(value).unwrap_or_default())
                .unwrap_or_default()
        });
        verify_webhook_signature(&trigger, resolved_secret.as_deref(), input, &raw_body)?;
        if let Some(idempotency_key) = input.idempotency_key.as_deref() {
            let existing = sqlx::query_as::<_, RoutineRunRow>(
                "SELECT id, company_id, routine_id, trigger_id, source, status, triggered_at, \
                    routine_revision_id, responsible_user_id, idempotency_key, trigger_payload, \
                    dispatch_fingerprint, linked_issue_id, coalesced_into_run_id, failure_reason, \
                    completed_at, created_at, updated_at FROM routine_runs \
                 WHERE company_id=$1 AND routine_id=$2 AND trigger_id=$3 AND source='webhook' AND idempotency_key=$4 \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(routine.company_id)
            .bind(routine.id)
            .bind(trigger.id)
            .bind(idempotency_key)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("check webhook idempotency: {error}")))?;
            if let Some(run) = existing {
                transaction.commit().await.map_err(|error| {
                    pc_errors::internal(format!("commit webhook idempotent: {error}"))
                })?;
                return Ok(FiredRoutineTrigger {
                    run,
                    secret_material: None,
                });
            }
        }
        let assignee_agent_id = routine.assignee_agent_id.ok_or_else(|| {
            pc_errors::conflict("Routine requires an assigned agent before webhook dispatch")
        })?;
        transaction.commit().await.map_err(|error| {
            pc_errors::internal(format!("commit webhook pre-dispatch: {error}"))
        })?;
        let dispatched = self
            .dispatch_run(
                routine.id,
                &RunRoutineRecord {
                    trigger_id: Some(trigger.id),
                    source: "webhook".into(),
                    payload: input.payload.clone(),
                    variables: None,
                    project_id: routine.project_id,
                    project_workspace_id: None,
                    assignee_agent_id: Some(assignee_agent_id),
                    idempotency_key: input.idempotency_key.clone(),
                    execution_workspace_id: None,
                    execution_workspace_preference: None,
                    execution_workspace_settings: None,
                    actor_agent_id: input.agent_id,
                    actor_user_id: input
                        .user_id
                        .clone()
                        .or_else(|| routine.responsible_user_id.clone()),
                },
            )
            .await?;
        Ok(FiredRoutineTrigger {
            run: dispatched.run,
            secret_material: None,
        })
    }

    pub async fn create_trigger_with_revision(
        &self,
        routine_id: Uuid,
        input: &CreateRoutineTriggerRecord,
    ) -> pc_errors::Result<RoutineTriggerMutationResult> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin trigger creation: {error}")))?;
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("load routine for trigger: {error}")))?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        let trigger = sqlx::query_as::<_, RoutineTriggerRow>(
            "INSERT INTO routine_triggers (company_id, routine_id, kind, label, enabled, \
                cron_expression, timezone, next_run_at, public_id, secret_id, signing_mode, \
                replay_window_sec, last_rotated_at, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, \
                CASE WHEN $3='webhook' THEN now() ELSE NULL END,$13,$14,$13,$14) \
             RETURNING id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at",
        )
        .bind(routine.company_id)
        .bind(routine.id)
        .bind(&input.kind)
        .bind(input.label.as_deref())
        .bind(input.enabled)
        .bind(input.cron_expression.as_deref())
        .bind(input.timezone.as_deref())
        .bind(input.next_run_at)
        .bind(input.public_id.as_deref())
        .bind(input.secret_id)
        .bind(input.signing_mode.as_deref())
        .bind(input.replay_window_sec)
        .bind(input.actor_agent_id)
        .bind(input.actor_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("create routine trigger: {error}")))?;
        let triggers = transaction_triggers(&mut transaction, routine.id)
            .await
            .map_err(|error| pc_errors::internal(format!("load trigger snapshot: {error}")))?;
        let snapshot = revision_snapshot(&routine, &triggers);
        let next_revision_number = routine.latest_revision_number + 1;
        let change_summary = format!("Created {} trigger", input.kind);
        let revision = sqlx::query_as::<_, RoutineRevisionRow>(
            "INSERT INTO routine_revisions (company_id, routine_id, revision_number, title, \
                description, snapshot, change_summary, created_by_agent_id, created_by_user_id, \
                created_by_run_id, responsible_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) \
             RETURNING id, company_id, routine_id, revision_number, title, description, snapshot, \
                change_summary, restored_from_revision_id, created_by_agent_id, created_by_user_id, \
                created_by_run_id, created_at",
        )
        .bind(routine.company_id)
        .bind(routine.id)
        .bind(next_revision_number)
        .bind(&routine.title)
        .bind(routine.description.as_deref())
        .bind(serde_json::to_value(snapshot).map_err(|error| {
            pc_errors::internal(format!("serialize trigger revision: {error}"))
        })?)
        .bind(&change_summary)
        .bind(input.actor_agent_id)
        .bind(input.actor_user_id.as_deref())
        .bind(input.actor_run_id)
        .bind(routine.responsible_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("append trigger revision: {error}")))?;
        sqlx::query(
            "UPDATE routines SET latest_revision_id=$2, latest_revision_number=$3, updated_at=now() \
             WHERE id=$1",
        )
        .bind(routine.id)
        .bind(revision.id)
        .bind(next_revision_number)
        .execute(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("advance trigger revision: {error}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit trigger creation: {error}")))?;
        Ok(RoutineTriggerMutationResult {
            trigger,
            secret_material: None,
            revision,
        })
    }

    pub async fn create_trigger(
        &self,
        company_id: Uuid,
        routine_id: Uuid,
        kind: &str,
        label: Option<&str>,
        cron_expression: Option<&str>,
        timezone: Option<&str>,
        public_id: Option<&str>,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<RoutineTriggerRow> {
        sqlx::query_as::<_, RoutineTriggerRow>(
            "INSERT INTO routine_triggers \
                (company_id, routine_id, kind, label, cron_expression, timezone, public_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             RETURNING id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                    next_run_at, last_fired_at, public_id, secret_id, signing_mode, \
                    replay_window_sec, last_rotated_at, last_result, created_by_agent_id, \
                    created_by_user_id, updated_by_agent_id, updated_by_user_id, \
                    created_at, updated_at",
        )
        .bind(company_id)
        .bind(routine_id)
        .bind(kind)
        .bind(label)
        .bind(cron_expression)
        .bind(timezone)
        .bind(public_id)
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update_trigger_with_revision(
        &self,
        id: Uuid,
        patch: &UpdateRoutineTriggerRecord,
    ) -> pc_errors::Result<Option<RoutineTriggerMutationResult>> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin trigger update: {error}")))?;
        let Some(mut trigger) = sqlx::query_as::<_, RoutineTriggerRow>(
            "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at \
             FROM routine_triggers WHERE id=$1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("load trigger for update: {error}")))?
        else {
            return Ok(None);
        };
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(trigger.routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| {
                pc_errors::internal(format!("lock routine for trigger update: {error}"))
            })?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        if let Some(value) = &patch.label {
            trigger.label = value.clone();
        }
        if let Some(value) = patch.enabled {
            trigger.enabled = value;
        }
        if let Some(value) = &patch.cron_expression {
            trigger.cron_expression = value.clone();
        }
        if let Some(value) = &patch.timezone {
            trigger.timezone = value.clone();
        }
        if let Some(value) = &patch.next_run_at {
            trigger.next_run_at = value.clone();
        }
        if let Some(value) = &patch.signing_mode {
            trigger.signing_mode = value.clone();
        }
        if let Some(value) = patch.replay_window_sec {
            trigger.replay_window_sec = value;
        }
        let updated = sqlx::query_as::<_, RoutineTriggerRow>(
            "UPDATE routine_triggers SET label=$2, enabled=$3, cron_expression=$4, timezone=$5, \
                next_run_at=$6, signing_mode=$7, replay_window_sec=$8, updated_by_agent_id=$9, \
                updated_by_user_id=$10, updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at",
        )
        .bind(trigger.id)
        .bind(trigger.label.as_deref())
        .bind(trigger.enabled)
        .bind(trigger.cron_expression.as_deref())
        .bind(trigger.timezone.as_deref())
        .bind(trigger.next_run_at.clone())
        .bind(trigger.signing_mode.as_deref())
        .bind(trigger.replay_window_sec)
        .bind(patch.actor_agent_id)
        .bind(patch.actor_user_id.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("update routine trigger: {error}")))?;
        let (_, revision) = append_current_routine_revision(
            &mut transaction,
            &routine,
            &format!("Updated {} trigger", trigger.kind),
            None,
            patch.actor_agent_id,
            patch.actor_user_id.as_deref(),
            patch.actor_run_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit trigger update: {error}")))?;
        Ok(Some(RoutineTriggerMutationResult {
            trigger: updated,
            secret_material: None,
            revision,
        }))
    }

    pub async fn delete_trigger_with_revision(
        &self,
        id: Uuid,
        actor_agent_id: Option<Uuid>,
        actor_user_id: Option<&str>,
        actor_run_id: Option<Uuid>,
    ) -> pc_errors::Result<Option<RoutineRevisionRow>> {
        let mut transaction = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| pc_errors::internal(format!("begin trigger deletion: {error}")))?;
        let Some(trigger) = sqlx::query_as::<_, RoutineTriggerRow>(
            "SELECT id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                next_run_at, last_fired_at, public_id, secret_id, signing_mode, replay_window_sec, \
                last_rotated_at, last_result, created_by_agent_id, created_by_user_id, \
                updated_by_agent_id, updated_by_user_id, created_at, updated_at \
             FROM routine_triggers WHERE id=$1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| pc_errors::internal(format!("load trigger for deletion: {error}")))?
        else {
            return Ok(None);
        };
        let routine_sql = format!("SELECT {COLS} FROM routines WHERE id=$1 FOR UPDATE");
        let routine = sqlx::query_as::<_, RoutineRow>(&routine_sql)
            .bind(trigger.routine_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| {
                pc_errors::internal(format!("lock routine for trigger deletion: {error}"))
            })?
            .ok_or_else(|| pc_errors::not_found("Routine not found"))?;
        sqlx::query("DELETE FROM routine_triggers WHERE id=$1")
            .bind(trigger.id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| pc_errors::internal(format!("delete routine trigger: {error}")))?;
        let (_, revision) = append_current_routine_revision(
            &mut transaction,
            &routine,
            &format!("Deleted {} trigger", trigger.kind),
            None,
            actor_agent_id,
            actor_user_id,
            actor_run_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| pc_errors::internal(format!("commit trigger deletion: {error}")))?;
        Ok(Some(revision))
    }

    pub async fn update_trigger(
        &self,
        id: Uuid,
        label: Option<&str>,
        enabled: Option<bool>,
        cron_expression: Option<&str>,
    ) -> sqlx::Result<Option<RoutineTriggerRow>> {
        sqlx::query_as::<_, RoutineTriggerRow>(
            "UPDATE routine_triggers SET \
                label = COALESCE($2, label), enabled = COALESCE($3, enabled), \
                cron_expression = COALESCE($4, cron_expression), updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, routine_id, kind, label, enabled, cron_expression, timezone, \
                    next_run_at, last_fired_at, public_id, secret_id, signing_mode, \
                    replay_window_sec, last_rotated_at, last_result, created_by_agent_id, \
                    created_by_user_id, updated_by_agent_id, updated_by_user_id, \
                    created_at, updated_at",
        )
        .bind(id)
        .bind(label)
        .bind(enabled)
        .bind(cron_expression)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn delete_trigger(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM routine_triggers WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn verify_webhook_signature(
    trigger: &RoutineTriggerRow,
    resolved_secret: Option<&str>,
    input: &FireTriggerInput,
    raw_body: &[u8],
) -> pc_errors::Result<()> {
    let mode = trigger.signing_mode.as_deref().unwrap_or("none");
    match mode {
        "none" => Ok(()),
        "bearer" => {
            let secret = resolved_secret
                .ok_or_else(|| pc_errors::unauthorized("Routine trigger is not active"))?;
            let expected = format!("Bearer {secret}");
            let provided = input
                .authorization_header
                .as_deref()
                .map(str::trim)
                .unwrap_or("");
            if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                return Err(pc_errors::unauthorized(
                    "Routine trigger authentication failed",
                ));
            }
            Ok(())
        }
        "github_hmac" | "hmac_sha256" => {
            let secret = resolved_secret
                .ok_or_else(|| pc_errors::unauthorized("Routine trigger is not active"))?;
            let header_value = input
                .hub_signature_header
                .as_deref()
                .or(input.signature_header.as_deref())
                .map(str::trim)
                .unwrap_or("");
            if header_value.is_empty() {
                return Err(pc_errors::unauthorized("Routine trigger signature missing"));
            }
            let normalized = header_value.trim_start_matches("sha256=");
            let expected = pc_secrets::hmac_sha256(secret.as_bytes(), raw_body);
            if !constant_time_eq(normalized.as_bytes(), expected.as_bytes()) {
                return Err(pc_errors::unauthorized(
                    "Routine trigger signature mismatch",
                ));
            }
            if mode == "hmac_sha256" {
                let timestamp = input
                    .timestamp_header
                    .as_deref()
                    .map(str::trim)
                    .ok_or_else(|| pc_errors::unauthorized("Routine trigger timestamp missing"))?;
                let ts: i64 = timestamp
                    .parse()
                    .map_err(|_| pc_errors::unauthorized("Routine trigger timestamp invalid"))?;
                let replay_window = trigger.replay_window_sec.unwrap_or(300);
                let delta = (chrono::Utc::now().timestamp() - ts).abs();
                if delta > replay_window as i64 {
                    return Err(pc_errors::unauthorized(
                        "Routine trigger timestamp outside replay window",
                    ));
                }
            }
            Ok(())
        }
        other => Err(pc_errors::conflict(format!(
            "Unsupported trigger signing mode: {other}"
        ))),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
