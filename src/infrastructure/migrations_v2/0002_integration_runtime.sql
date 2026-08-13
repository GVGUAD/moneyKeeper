-- Finance V2 durable integration runtime. Migration 0001 owns the schema.

CREATE TABLE integration.outbox_messages (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    message_id UUID NOT NULL UNIQUE,
    event_id UUID NOT NULL UNIQUE,
    message_schema_version INTEGER NOT NULL CHECK (message_schema_version > 0),
    context_name TEXT NOT NULL CHECK (
        context_name <> ''
        AND context_name = BTRIM(context_name)
        AND octet_length(context_name) <= 200
        AND context_name !~ '[[:cntrl:]]'
    ),
    aggregate_id TEXT NOT NULL CHECK (
        aggregate_id <> ''
        AND aggregate_id = BTRIM(aggregate_id)
        AND octet_length(aggregate_id) <= 500
        AND aggregate_id !~ '[[:cntrl:]]'
    ),
    aggregate_version BIGINT NOT NULL CHECK (aggregate_version > 0),
    event_type TEXT NOT NULL CHECK (
        event_type <> ''
        AND event_type = BTRIM(event_type)
        AND octet_length(event_type) <= 200
        AND event_type !~ '[[:cntrl:]]'
    ),
    user_id UUID NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    correlation_id UUID NOT NULL,
    causation_id UUID,
    payload JSONB NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    claim_holder TEXT,
    claim_token BIGINT NOT NULL DEFAULT 0 CHECK (claim_token >= 0),
    claim_expires_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    published_at TIMESTAMPTZ,
    dead_lettered_at TIMESTAMPTZ,
    last_error VARCHAR(512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((claim_holder IS NULL) = (claim_expires_at IS NULL)),
    CHECK (claim_holder IS NULL OR (
        claim_holder <> ''
        AND claim_holder = BTRIM(claim_holder)
        AND OCTET_LENGTH(claim_holder) <= 200
        AND claim_holder !~ '[[:cntrl:]]'
        AND claim_token > 0
        AND attempts > 0
    )),
    CHECK (published_at IS NULL OR dead_lettered_at IS NULL)
);

CREATE INDEX outbox_messages_dispatch_idx
    ON integration.outbox_messages (available_at, sequence, message_id)
    WHERE published_at IS NULL AND dead_lettered_at IS NULL;

CREATE INDEX outbox_messages_claim_expiry_idx
    ON integration.outbox_messages (claim_expires_at)
    WHERE claim_expires_at IS NOT NULL
      AND published_at IS NULL
      AND dead_lettered_at IS NULL;

CREATE TABLE integration.inbox_receipts (
    consumer_name TEXT NOT NULL CHECK (
        consumer_name <> ''
        AND consumer_name = BTRIM(consumer_name)
        AND OCTET_LENGTH(consumer_name) <= 200
        AND consumer_name !~ '[[:cntrl:]]'
    ),
    message_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK (
        event_type <> ''
        AND event_type = BTRIM(event_type)
        AND OCTET_LENGTH(event_type) <= 200
        AND event_type !~ '[[:cntrl:]]'
    ),
    received_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    processed_at TIMESTAMPTZ,
    PRIMARY KEY (consumer_name, message_id)
);

CREATE TABLE integration.process_instances (
    process_name TEXT NOT NULL CHECK (
        process_name <> ''
        AND process_name = BTRIM(process_name)
        AND OCTET_LENGTH(process_name) <= 200
        AND process_name !~ '[[:cntrl:]]'
    ),
    instance_key TEXT NOT NULL CHECK (
        instance_key <> ''
        AND instance_key = BTRIM(instance_key)
        AND OCTET_LENGTH(instance_key) <= 200
        AND instance_key !~ '[[:cntrl:]]'
    ),
    state JSONB NOT NULL,
    status TEXT NOT NULL CHECK (
        status <> ''
        AND status = BTRIM(status)
        AND OCTET_LENGTH(status) <= 200
        AND status !~ '[[:cntrl:]]'
    ),
    version BIGINT NOT NULL CHECK (version > 0),
    next_wake_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (process_name, instance_key)
);

CREATE INDEX process_instances_wake_idx
    ON integration.process_instances (next_wake_at, process_name, instance_key)
    WHERE next_wake_at IS NOT NULL;

CREATE TABLE integration.process_leases (
    process_name TEXT NOT NULL CHECK (
        process_name <> ''
        AND process_name = BTRIM(process_name)
        AND OCTET_LENGTH(process_name) <= 200
        AND process_name !~ '[[:cntrl:]]'
    ),
    instance_key TEXT NOT NULL CHECK (
        instance_key <> ''
        AND instance_key = BTRIM(instance_key)
        AND OCTET_LENGTH(instance_key) <= 200
        AND instance_key !~ '[[:cntrl:]]'
    ),
    holder TEXT NOT NULL CHECK (
        holder <> ''
        AND holder = BTRIM(holder)
        AND OCTET_LENGTH(holder) <= 200
        AND holder !~ '[[:cntrl:]]'
    ),
    expires_at TIMESTAMPTZ NOT NULL,
    fencing_token BIGINT NOT NULL CHECK (fencing_token > 0),
    PRIMARY KEY (process_name, instance_key)
);

CREATE INDEX process_leases_expiry_idx
    ON integration.process_leases (expires_at, process_name, instance_key);
