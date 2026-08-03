CREATE TABLE IF NOT EXISTS "project_artifacts" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "project_id" uuid NOT NULL,
    "path" text NOT NULL,
    "mime_type" text DEFAULT 'application/octet-stream',
    "size_bytes" bigint DEFAULT 0,
    "metadata" jsonb DEFAULT '{}'::jsonb,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "project_artifacts_company_project_idx" ON "project_artifacts" USING btree ("company_id","project_id","created_at" DESC);
