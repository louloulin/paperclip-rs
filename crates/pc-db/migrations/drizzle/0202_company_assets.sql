CREATE TABLE IF NOT EXISTS "company_assets" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "kind" text NOT NULL,
    "key" text NOT NULL,
    "content_type" text DEFAULT 'application/octet-stream',
    "size_bytes" bigint DEFAULT 0,
    "sha256" text,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "company_assets_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "company_assets_company_kind_idx" ON "company_assets" USING btree ("company_id","kind","created_at" DESC);
