-- Expand the former combined event-policy cursor into independently durable
-- Recurring and Reporting receipts. The historical receipts are deliberately
-- retained for auditability and rollback to the expand release.

INSERT INTO integration.inbox_receipts (
    consumer_name,
    message_id,
    event_type,
    received_at,
    processed_at
)
SELECT
    'recurring-event-policy-v1',
    message_id,
    event_type,
    received_at,
    processed_at
FROM integration.inbox_receipts
WHERE consumer_name = 'finance-v2-phase4-router'
ON CONFLICT (consumer_name, message_id) DO NOTHING;

INSERT INTO integration.inbox_receipts (
    consumer_name,
    message_id,
    event_type,
    received_at,
    processed_at
)
SELECT
    'reporting-projections-v1',
    message_id,
    event_type,
    received_at,
    processed_at
FROM integration.inbox_receipts
WHERE consumer_name = 'finance-v2-phase4-router'
ON CONFLICT (consumer_name, message_id) DO NOTHING;
