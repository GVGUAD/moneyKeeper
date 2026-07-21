-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY subscription_charges_gmail_message_unique
    ON subscription_charges (source_connection_id, provider_message_id)
    WHERE source = 'gmail'
      AND source_connection_id IS NOT NULL
      AND provider_message_id IS NOT NULL;
