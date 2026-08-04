CREATE TABLE IF NOT EXISTS "smoke_lab_services" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "service_key" text NOT NULL,
    "status" text NOT NULL DEFAULT 'stopped',
    "config" jsonb NOT NULL DEFAULT '{}'::jsonb,
    "pid" integer,
    "started_at" timestamptz,
    "stopped_at" timestamptz,
    "last_error" text,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "smoke_lab_services_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade,
    CONSTRAINT "smoke_lab_services_company_key_uq" UNIQUE ("company_id","service_key")
);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "smoke_lab_oauth_codes" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "code" text NOT NULL,
    "client_id" text NOT NULL,
    "redirect_uri" text,
    "scope" text,
    "expires_at" timestamptz NOT NULL,
    "consumed_at" timestamptz,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "smoke_lab_oauth_codes_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade,
    CONSTRAINT "smoke_lab_oauth_codes_code_uq" UNIQUE ("code")
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "smoke_lab_oauth_codes_company_idx" ON "smoke_lab_oauth_codes" USING btree ("company_id","expires_at" DESC);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "smoke_lab_oauth_tokens" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "user_id" uuid,
    "client_id" text NOT NULL,
    "access_token" text NOT NULL,
    "refresh_token" text,
    "scope" text,
    "expires_at" timestamptz NOT NULL,
    "revoked_at" timestamptz,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "smoke_lab_oauth_tokens_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade,
    CONSTRAINT "smoke_lab_oauth_tokens_access_uq" UNIQUE ("access_token")
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "smoke_lab_oauth_tokens_company_idx" ON "smoke_lab_oauth_tokens" USING btree ("company_id","expires_at" DESC);
