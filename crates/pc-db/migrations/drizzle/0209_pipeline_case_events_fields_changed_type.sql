-- Add 'fields_changed' to pipeline_case_events.type CHECK constraint.
-- This type is used by insert_fields_changed_event (automation retry path).
-- Without this, the INSERT fails with: new row violates check constraint
-- "pipeline_case_events_type_check"
ALTER TABLE "pipeline_case_events" DROP CONSTRAINT "pipeline_case_events_type_check";
ALTER TABLE "pipeline_case_events" ADD CONSTRAINT "pipeline_case_events_type_check"
    CHECK ("pipeline_case_events"."type" IN (
        'ingested',
        'updated',
        'claimed',
        'lease_released',
        'lease_expired',
        'transitioned',
        'transition_suggested',
        'suggestion_resolved',
        'review_decided',
        'conversation_opened',
        'issue_linked',
        'automation_executed',
        'automation_failed',
        'blockers_set',
        'blockers_resolved',
        'children_terminal',
        'fields_changed'
    ));
