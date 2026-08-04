CREATE TABLE IF NOT EXISTS "execution_lease" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "workspace_id" uuid NOT NULL,
    "agent_id" uuid NOT NULL,
    "run_id" uuid,
    "heartbeat_run_id" uuid,
    "state" text NOT NULL DEFAULT 'holding',
    "token" text NOT NULL,
    "acquired_at" timestamptz DEFAULT now() NOT NULL,
    "expires_at" timestamptz NOT NULL,
    "last_renewed_at" timestamptz DEFAULT now() NOT NULL,
    "released_at" timestamptz,
    "revocation_reason" text,
    CONSTRAINT "execution_lease_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade,
    CONSTRAINT "execution_lease_workspace_fk" FOREIGN KEY ("workspace_id") REFERENCES "public"."execution_workspaces"("id") ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "execution_lease_ws_state_idx" ON "execution_lease" USING btree ("workspace_id","state","expires_at");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "execution_lease_state_expires_idx" ON "execution_lease" USING btree ("state","expires_at");
