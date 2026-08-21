-- Immutable external exchange-rate observations and durable source cursors.
CREATE FUNCTION reference_data.reject_fx_mutation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'FX observations are immutable'; END $$;
CREATE TABLE reference_data.fx_observations (
 id UUID PRIMARY KEY,source TEXT NOT NULL,source_revision TEXT NOT NULL,base_currency VARCHAR(3) NOT NULL,quote_currency VARCHAR(3) NOT NULL,
 rate NUMERIC(28,12) NOT NULL CHECK(rate>0),effective_at TIMESTAMPTZ NOT NULL,observed_at TIMESTAMPTZ NOT NULL,recorded_at TIMESTAMPTZ NOT NULL,
 content_digest BYTEA NOT NULL CHECK(octet_length(content_digest)=32),source_priority SMALLINT NOT NULL DEFAULT 100,sequence BIGINT GENERATED ALWAYS AS IDENTITY,
 UNIQUE(source,source_revision,base_currency,quote_currency),CHECK(base_currency<>quote_currency),CHECK(observed_at<=recorded_at)
);
CREATE INDEX fx_observations_as_of ON reference_data.fx_observations(base_currency,quote_currency,effective_at DESC,source_priority,observed_at DESC,sequence DESC,id DESC);
CREATE TABLE reference_data.fx_conflicts (
 id UUID PRIMARY KEY,source TEXT NOT NULL,source_revision TEXT NOT NULL,conflicting_digest BYTEA NOT NULL CHECK(octet_length(conflicting_digest)=32),reason TEXT NOT NULL,recorded_at TIMESTAMPTZ NOT NULL,
 UNIQUE(source,source_revision,conflicting_digest)
);
CREATE TABLE reference_data.fx_sync_state (
 source TEXT PRIMARY KEY,state TEXT NOT NULL CHECK(state IN ('idle','running','retry_due','failed')),date_cursor DATE,backfill_days INTEGER NOT NULL CHECK(backfill_days>=0),
 lease_holder TEXT,lease_expires_at TIMESTAMPTZ,fencing_token BIGINT NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_retry_at TIMESTAMPTZ,last_error VARCHAR(500),updated_at TIMESTAMPTZ NOT NULL,
 CHECK((lease_holder IS NULL)=(lease_expires_at IS NULL))
);
CREATE TRIGGER fx_observations_immutable BEFORE UPDATE OR DELETE ON reference_data.fx_observations FOR EACH ROW EXECUTE FUNCTION reference_data.reject_fx_mutation();
CREATE TRIGGER fx_conflicts_immutable BEFORE UPDATE OR DELETE ON reference_data.fx_conflicts FOR EACH ROW EXECUTE FUNCTION reference_data.reject_fx_mutation();
