-- no-transaction
-- RFC Message-ID is a replay bridge only; Gmail provider message IDs remain
-- the durable source identity.
CREATE INDEX CONCURRENTLY subscription_charges_user_rfc_message_idx
    ON subscription_charges (user_id, rfc_message_id)
    WHERE rfc_message_id IS NOT NULL;
