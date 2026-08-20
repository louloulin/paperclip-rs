-- R818c: company_skills soft-delete + audit columns expected by
-- `crates/pc-repos/src/skill.rs::CompanySkillRow` (SKILL_COLS). The
-- source code already references `deleted_at`, `archived_at`,
-- `created_by_agent_id`, `created_by_user_id`, `updated_by_agent_id`,
-- `updated_by_user_id`; without this migration any SELECT through the
-- list/get/categories path returned 500 with "column deleted_at does
-- not exist". Backfills existing rows so all NOT NULL defaults hold.
ALTER TABLE "company_skills" ADD COLUMN IF NOT EXISTS "deleted_at" timestamp with time zone;
ALTER TABLE "company_skills" ADD COLUMN IF NOT EXISTS "archived_at" timestamp with time zone;
ALTER TABLE "company_skills" ADD COLUMN IF NOT EXISTS "created_by_agent_id" uuid;
ALTER TABLE "company_skills" ADD COLUMN IF NOT EXISTS "created_by_user_id" text;
ALTER TABLE "company_skills" ADD COLUMN IF NOT EXISTS "updated_by_agent_id" uuid;
ALTER TABLE "company_skills" ADD COLUMN IF NOT EXISTS "updated_by_user_id" text;
CREATE INDEX IF NOT EXISTS "company_skills_company_archived_idx" ON "company_skills" ("company_id","archived_at");
CREATE INDEX IF NOT EXISTS "company_skills_company_deleted_idx" ON "company_skills" ("company_id","deleted_at");
