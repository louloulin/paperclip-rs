CREATE TABLE IF NOT EXISTS "board_chat_threads" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "company_id" uuid NOT NULL,
    "issue_id" uuid,
    "title" text NOT NULL DEFAULT 'Board Chat',
    "status" text NOT NULL DEFAULT 'active',
    "created_by_user_id" uuid,
    "last_message_at" timestamptz DEFAULT now() NOT NULL,
    "created_at" timestamptz DEFAULT now() NOT NULL,
    "updated_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "board_chat_threads_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade,
    CONSTRAINT "board_chat_threads_issue_fk" FOREIGN KEY ("issue_id") REFERENCES "public"."issues"("id") ON DELETE set null
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "board_chat_threads_company_last_msg_idx" ON "board_chat_threads" USING btree ("company_id","last_message_at" DESC);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "board_chat_messages" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    "thread_id" uuid NOT NULL,
    "company_id" uuid NOT NULL,
    "role" text NOT NULL,
    "author_user_id" uuid,
    "author_agent_id" uuid,
    "body" text NOT NULL,
    "tool_uses" jsonb NOT NULL DEFAULT '[]'::jsonb,
    "status" text NOT NULL DEFAULT 'complete',
    "created_at" timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT "board_chat_messages_thread_fk" FOREIGN KEY ("thread_id") REFERENCES "public"."board_chat_threads"("id") ON DELETE cascade,
    CONSTRAINT "board_chat_messages_company_fk" FOREIGN KEY ("company_id") REFERENCES "public"."companies"("id") ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "board_chat_messages_thread_created_idx" ON "board_chat_messages" USING btree ("thread_id","created_at");
