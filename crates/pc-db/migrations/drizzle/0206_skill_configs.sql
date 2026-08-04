CREATE TABLE IF NOT EXISTS "company_skill_configs" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "skill_id" uuid NOT NULL,
    "value" jsonb NOT NULL DEFAULT '{}'::jsonb,
    "updated_by_user_id" uuid,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "company_skill_configs_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade,
    CONSTRAINT "company_skill_configs_skill_fk" FOREIGN KEY ("skill_id") REFERENCES "public"."company_skills"("id") ON DELETE cascade,
    CONSTRAINT "company_skill_configs_skill_uq" UNIQUE ("company_id","skill_id")
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "company_skill_configs_company_idx" ON "company_skill_configs" USING btree ("company_id","updated_at" DESC);
