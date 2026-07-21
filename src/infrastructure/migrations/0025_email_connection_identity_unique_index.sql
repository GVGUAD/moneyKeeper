-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY email_connections_user_provider_address_unique
    ON email_connections (user_id, provider, lower(btrim(email_address)));
