CREATE TABLE "issue_checkout_locks" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"issue_id" uuid NOT NULL,
	"run_id" uuid NOT NULL,
	"actor_type" text NOT NULL,
	"actor_id" text,
	"strategy" text NOT NULL DEFAULT 'merge',
	"status" text NOT NULL DEFAULT 'active',
	"created_at" timestamp with time zone NOT NULL DEFAULT now(),
	"released_at" timestamp with time zone,
	CONSTRAINT "issue_checkout_locks_issue_run_uq" UNIQUE("issue_id","run_id")
);
---> statement-breakpoint
CREATE INDEX "issue_checkout_locks_issue_idx" ON "issue_checkout_locks" USING btree ("issue_id");
---> statement-breakpoint
CREATE INDEX "issue_checkout_locks_run_idx" ON "issue_checkout_locks" USING btree ("run_id");
---> statement-breakpoint
ALTER TABLE "issue_checkout_locks" ADD CONSTRAINT "issue_checkout_locks_issue_id_issues_id_fk" FOREIGN KEY ("issue_id") REFERENCES "issues"("id") ON DELETE CASCADE;
