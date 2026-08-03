CREATE TABLE IF NOT EXISTS "team_installs" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "catalog_id" text NOT NULL,
    "status" text DEFAULT 'queued' NOT NULL,
    "snapshot" jsonb NOT NULL DEFAULT '{}'::jsonb,
    "installed_at" timestamptz DEFAULT now() NOT NULL,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "team_installs_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade
);
--> statement-breakpoint
CREATE UNIQUE INDEX IF NOT EXISTS "team_installs_company_catalog_uq" ON "team_installs" USING btree ("company_id","catalog_id");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "team_installs_company_idx" ON "team_installs" USING btree ("company_id","installed_at" DESC);
