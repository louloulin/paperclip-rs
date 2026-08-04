CREATE TABLE IF NOT EXISTS "workspace_action_log" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "workspace_id" uuid NOT NULL,
    "kind" text NOT NULL,
    "action" text NOT NULL,
    "payload" jsonb NOT NULL DEFAULT '{}'::jsonb,
    "status" text DEFAULT 'queued' NOT NULL,
    "error" text,
    "requested_by_user_id" uuid,
    "requested_by_agent_id" uuid,
    "started_at" timestamptz,
    "completed_at" timestamptz,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "workspace_action_log_ws_fk" FOREIGN KEY ("workspace_id") REFERENCES "public"."execution_workspaces"("id") ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "workspace_action_log_ws_status_idx" ON "workspace_action_log" USING btree ("workspace_id","status","created_at" DESC);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "workspace_action_log_queued_idx" ON "workspace_action_log" USING btree ("status","created_at") WHERE status = 'queued';
