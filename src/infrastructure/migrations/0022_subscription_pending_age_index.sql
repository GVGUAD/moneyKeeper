-- no-transaction
CREATE INDEX CONCURRENTLY subscription_charges_pending_age_idx
    ON subscription_charges (user_id, match_started_at)
    WHERE match_status = 'Pending';
