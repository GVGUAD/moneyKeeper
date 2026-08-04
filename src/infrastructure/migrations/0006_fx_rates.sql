CREATE TABLE fx_rates (
    rate_date     DATE    NOT NULL,
    from_currency TEXT    NOT NULL,
    to_currency   TEXT    NOT NULL,
    rate          NUMERIC NOT NULL,
    PRIMARY KEY (rate_date, from_currency, to_currency)
);

CREATE INDEX idx_fx_rates_date ON fx_rates (rate_date);
CREATE INDEX idx_fx_rates_from_currency ON fx_rates (from_currency, rate_date);
