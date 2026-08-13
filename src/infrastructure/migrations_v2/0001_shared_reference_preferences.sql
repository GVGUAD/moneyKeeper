CREATE SCHEMA shared_kernel;
CREATE SCHEMA reference_data;
CREATE SCHEMA classification;
CREATE SCHEMA preferences;
CREATE SCHEMA integration;
CREATE SCHEMA ledger;
CREATE SCHEMA banking;
CREATE SCHEMA mail;
CREATE SCHEMA recurring;
CREATE SCHEMA reporting;
CREATE SCHEMA sharing;
CREATE SCHEMA loans;
CREATE SCHEMA portfolio;

CREATE TABLE shared_kernel.database_lineage (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    lineage TEXT NOT NULL CHECK (lineage = 'finance-v2'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

INSERT INTO shared_kernel.database_lineage (singleton, lineage)
VALUES (TRUE, 'finance-v2');

CREATE FUNCTION shared_kernel.reject_database_lineage_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Finance V2 database lineage is immutable';
END;
$$;

CREATE TRIGGER database_lineage_is_immutable
BEFORE UPDATE OR DELETE ON shared_kernel.database_lineage
FOR EACH ROW
EXECUTE FUNCTION shared_kernel.reject_database_lineage_mutation();

CREATE TABLE reference_data.currencies (
    code VARCHAR(3) PRIMARY KEY,
    numeric_code VARCHAR(3) UNIQUE,
    name TEXT NOT NULL CHECK (
        name = btrim(name) AND name <> '' AND char_length(name) <= 100
    ),
    minor_unit SMALLINT NOT NULL CHECK (minor_unit BETWEEN 0 AND 8),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT currency_code_is_uppercase_iso_shaped
        CHECK (code COLLATE "C" ~ '^[A-Z]{3}$'),
    CONSTRAINT currency_numeric_code_is_iso_shaped
        CHECK (numeric_code IS NULL OR numeric_code COLLATE "C" ~ '^[0-9]{3}$'),
    CONSTRAINT currency_code_enabled_key UNIQUE (code, enabled)
);

INSERT INTO reference_data.currencies (code, numeric_code, name, minor_unit)
VALUES
    ('UAH', '980', 'Ukrainian hryvnia', 2),
    ('USD', '840', 'United States dollar', 2),
    ('EUR', '978', 'Euro', 2);

CREATE TABLE classification.categories (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (
        name = btrim(name) AND name <> '' AND char_length(name) <= 100
    ),
    kind TEXT NOT NULL CHECK (kind IN ('income', 'expense', 'both')),
    lifecycle TEXT NOT NULL DEFAULT 'active'
        CHECK (lifecycle IN ('active', 'archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id)
);

CREATE UNIQUE INDEX categories_active_name_unique
    ON classification.categories (user_id, lower(name))
    WHERE lifecycle = 'active';

CREATE INDEX categories_user_lifecycle_name
    ON classification.categories (user_id, lifecycle, lower(name), id);

CREATE TABLE preferences.user_preferences (
    user_id UUID PRIMARY KEY,
    base_currency VARCHAR(3) NOT NULL,
    base_currency_enabled BOOLEAN NOT NULL DEFAULT TRUE CHECK (base_currency_enabled),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT preference_enabled_currency_fk
        FOREIGN KEY (base_currency, base_currency_enabled)
        REFERENCES reference_data.currencies (code, enabled)
);
