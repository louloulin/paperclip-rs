ALTER TABLE issue_tree_holds ADD COLUMN IF NOT EXISTS scope text;
--> statement-breakpoint
ALTER TABLE issue_tree_holds ADD COLUMN IF NOT EXISTS release_reason_note text;
