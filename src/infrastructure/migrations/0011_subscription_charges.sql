CREATE TABLE subscription_charges (
    id UUID PRIMARY KEY,
    subscription_id UUID NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    amount NUMERIC NOT NULL,
    currency TEXT NOT NULL,
    charged_at BIGINT NOT NULL,
    email_message_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    transaction_id UUID REFERENCES transactions(id) ON DELETE SET NULL,
    match_status TEXT NOT NULL,
    created_at BIGINT NOT NULL,

    CONSTRAINT subscription_charges_message_id_unique UNIQUE (email_message_id)
);

CREATE INDEX subscription_charges_user_charged_idx ON subscription_charges (user_id, charged_at);
CREATE INDEX subscription_charges_tx_idx ON subscription_charges (transaction_id);
CREATE INDEX subscription_charges_pending_idx
    ON subscription_charges (user_id, match_status)
    WHERE match_status IN ('Pending', 'Unmatched');
