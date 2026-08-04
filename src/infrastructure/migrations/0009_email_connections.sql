CREATE TABLE email_connections (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    provider TEXT NOT NULL,
    email_address TEXT NOT NULL,
    oauth_access_token TEXT NOT NULL,
    oauth_refresh_token TEXT NOT NULL,
    access_token_expires_at BIGINT NOT NULL,
    status TEXT NOT NULL,
    last_synced_at BIGINT,
    last_history_id TEXT,
    created_at BIGINT NOT NULL
);

CREATE INDEX email_connections_user_id_idx ON email_connections (user_id);
CREATE INDEX email_connections_status_idx ON email_connections (status);
