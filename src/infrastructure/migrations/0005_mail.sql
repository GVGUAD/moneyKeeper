-- Mail owns encrypted Gmail connections, immutable source messages and durable work.

CREATE FUNCTION mail.reject_immutable_fact_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'mail facts are immutable'; END $$;

CREATE TABLE mail.connections (
    id UUID NOT NULL, user_id UUID NOT NULL, provider TEXT NOT NULL DEFAULT 'gmail' CHECK (provider = 'gmail'),
    state TEXT NOT NULL CHECK (state IN ('pending','active','needs_reauth','disconnected')),
    credential_ciphertext BYTEA, credential_nonce BYTEA, credential_key_id TEXT,
    credential_generation BIGINT NOT NULL DEFAULT 1 CHECK (credential_generation > 0),
    sync_generation BIGINT NOT NULL DEFAULT 1 CHECK (sync_generation > 0),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id,user_id),
    CHECK ((credential_ciphertext IS NULL AND credential_nonce IS NULL AND credential_key_id IS NULL)
        OR (octet_length(credential_ciphertext)>0 AND octet_length(credential_nonce)>=12 AND credential_key_id<>'')),
    CHECK (state <> 'active' OR credential_ciphertext IS NOT NULL),
    CHECK (state <> 'disconnected' OR credential_ciphertext IS NULL)
);

CREATE TABLE mail.oauth_states (
    state_digest BYTEA PRIMARY KEY CHECK (octet_length(state_digest)=32), user_id UUID NOT NULL,
    verifier_ciphertext BYTEA NOT NULL, verifier_nonce BYTEA NOT NULL CHECK (octet_length(verifier_nonce)>=12),
    key_id TEXT NOT NULL CHECK (key_id<>''), replacement_connection_id UUID,
    expected_version BIGINT, expires_at TIMESTAMPTZ NOT NULL, consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((replacement_connection_id IS NULL)=(expected_version IS NULL))
);

CREATE TABLE mail.oauth_callback_receipts (
    state_digest BYTEA PRIMARY KEY REFERENCES mail.oauth_states(state_digest),
    code_digest BYTEA NOT NULL CHECK (octet_length(code_digest)=32), status TEXT NOT NULL CHECK(status IN ('processing','succeeded','failed')),
    http_status SMALLINT, redirect_uri TEXT, connection_id UUID, response_body JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(), completed_at TIMESTAMPTZ
);

CREATE TABLE mail.command_receipts (
    user_id UUID NOT NULL, command_scope TEXT NOT NULL CHECK(command_scope<>''), idempotency_key TEXT NOT NULL CHECK(idempotency_key<>''),
    command_name TEXT NOT NULL CHECK(command_name<>''), target_id UUID, request_hash BYTEA NOT NULL CHECK(octet_length(request_hash)=32),
    status TEXT NOT NULL CHECK(status IN ('processing','succeeded','rejected','failed')), http_status SMALLINT, response_body JSONB,
    aggregate_id UUID, aggregate_version BIGINT, created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(), completed_at TIMESTAMPTZ,
    PRIMARY KEY(user_id,command_scope,idempotency_key), CHECK((status='processing')=(completed_at IS NULL))
);

CREATE TABLE mail.sync_jobs (
    id UUID NOT NULL,user_id UUID NOT NULL,connection_id UUID NOT NULL,state TEXT NOT NULL CHECK(state IN ('requested','running','retry_due','completed','failed','cancelled')),
    connection_version BIGINT NOT NULL, credential_generation BIGINT NOT NULL, sync_generation BIGINT NOT NULL,
    cursor TEXT, attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0), next_retry_at TIMESTAMPTZ,last_error VARCHAR(500),
    lease_holder TEXT,lease_expires_at TIMESTAMPTZ,lease_token BIGINT NOT NULL DEFAULT 0 CHECK(lease_token>=0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(id,user_id),FOREIGN KEY(connection_id,user_id) REFERENCES mail.connections(id,user_id),
    CHECK((lease_holder IS NULL)=(lease_expires_at IS NULL))
);
CREATE INDEX mail_sync_jobs_due ON mail.sync_jobs(next_retry_at,created_at,id) WHERE state IN ('requested','retry_due');

CREATE TABLE mail.source_messages (
    id UUID NOT NULL,user_id UUID NOT NULL,connection_id UUID NOT NULL,provider_message_id TEXT NOT NULL CHECK(provider_message_id<>''),
    revision BIGINT NOT NULL CHECK(revision>0),payload_digest BYTEA NOT NULL CHECK(octet_length(payload_digest)=32),
    payload_ciphertext BYTEA NOT NULL,payload_nonce BYTEA NOT NULL CHECK(octet_length(payload_nonce)>=12),key_id TEXT NOT NULL CHECK(key_id<>''),
    received_at TIMESTAMPTZ NOT NULL,recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(id,user_id),UNIQUE(connection_id,provider_message_id,revision),UNIQUE(connection_id,provider_message_id,payload_digest),
    FOREIGN KEY(connection_id,user_id) REFERENCES mail.connections(id,user_id)
);

CREATE TABLE mail.fetch_attempts (
    id UUID PRIMARY KEY,user_id UUID NOT NULL,job_id UUID NOT NULL,state TEXT NOT NULL CHECK(state IN ('started','succeeded','retry_due','failed','discarded_stale')),
    page_cursor TEXT,error_code TEXT,started_at TIMESTAMPTZ NOT NULL,finished_at TIMESTAMPTZ,
    FOREIGN KEY(job_id,user_id) REFERENCES mail.sync_jobs(id,user_id)
);
CREATE TABLE mail.parse_attempts (
    id UUID PRIMARY KEY,user_id UUID NOT NULL,message_id UUID NOT NULL,parser_name TEXT NOT NULL,parser_version INTEGER NOT NULL CHECK(parser_version>0),
    state TEXT NOT NULL CHECK(state IN ('started','parsed','unsupported','malformed','panicked')),error_code TEXT,recorded_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY(message_id,user_id) REFERENCES mail.source_messages(id,user_id)
);
CREATE TABLE mail.receipt_evidence (
    id UUID NOT NULL,user_id UUID NOT NULL,message_id UUID NOT NULL,parser_name TEXT NOT NULL,parser_version INTEGER NOT NULL,
    evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('renewal','one_time','refund','cancellation')),
    merchant TEXT NOT NULL CHECK(merchant<>''),amount NUMERIC(28,8),currency VARCHAR(3),charged_at TIMESTAMPTZ,
    provenance JSONB NOT NULL,recorded_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(id,user_id),
    UNIQUE(message_id,parser_name,parser_version),FOREIGN KEY(message_id,user_id) REFERENCES mail.source_messages(id,user_id)
);

CREATE TRIGGER source_messages_immutable BEFORE UPDATE OR DELETE ON mail.source_messages FOR EACH ROW EXECUTE FUNCTION mail.reject_immutable_fact_mutation();
CREATE TRIGGER fetch_attempts_immutable BEFORE UPDATE OR DELETE ON mail.fetch_attempts FOR EACH ROW EXECUTE FUNCTION mail.reject_immutable_fact_mutation();
CREATE TRIGGER parse_attempts_immutable BEFORE UPDATE OR DELETE ON mail.parse_attempts FOR EACH ROW EXECUTE FUNCTION mail.reject_immutable_fact_mutation();
CREATE TRIGGER receipt_evidence_immutable BEFORE UPDATE OR DELETE ON mail.receipt_evidence FOR EACH ROW EXECUTE FUNCTION mail.reject_immutable_fact_mutation();
