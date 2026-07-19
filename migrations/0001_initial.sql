-- Durable state for one network-pinned x402 NEAR facilitator deployment.
--
-- Monetary values are NUMERIC rather than BIGINT because NEAR balances are
-- denominated in yoctoNEAR and may exceed PostgreSQL's signed 64-bit range.

CREATE TABLE api_clients (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    environment TEXT NOT NULL CHECK (environment IN ('mainnet', 'testnet')),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    daily_budget_yocto_near NUMERIC(40, 0) NOT NULL
        CHECK (daily_budget_yocto_near >= 0),
    verify_rate_per_minute INTEGER NOT NULL DEFAULT 60
        CHECK (verify_rate_per_minute > 0),
    settle_rate_per_minute INTEGER NOT NULL DEFAULT 10
        CHECK (settle_rate_per_minute > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ
);

CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    client_id UUID NOT NULL REFERENCES api_clients(id) ON DELETE CASCADE,
    key_prefix TEXT NOT NULL UNIQUE,
    key_digest BYTEA NOT NULL CHECK (octet_length(key_digest) = 32),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ
);

CREATE INDEX api_keys_client_active_idx
    ON api_keys (client_id)
    WHERE status = 'active';

CREATE TABLE api_client_payees (
    client_id UUID NOT NULL REFERENCES api_clients(id) ON DELETE CASCADE,
    network TEXT NOT NULL,
    asset TEXT NOT NULL,
    pay_to TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (client_id, network, asset, pay_to)
);

CREATE TABLE relayers (
    network TEXT NOT NULL,
    account_id TEXT NOT NULL,
    public_key TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'quarantined', 'disabled')),
    quarantine_reason TEXT,
    last_observed_nonce NUMERIC(20, 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (network, account_id, public_key)
);

CREATE TABLE daily_global_sponsorship (
    usage_date DATE PRIMARY KEY,
    reserved_yocto_near NUMERIC(40, 0) NOT NULL DEFAULT 0
        CHECK (reserved_yocto_near >= 0),
    spent_yocto_near NUMERIC(40, 0) NOT NULL DEFAULT 0
        CHECK (spent_yocto_near >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE daily_client_sponsorship (
    usage_date DATE NOT NULL,
    client_id UUID NOT NULL REFERENCES api_clients(id) ON DELETE CASCADE,
    reserved_yocto_near NUMERIC(40, 0) NOT NULL DEFAULT 0
        CHECK (reserved_yocto_near >= 0),
    spent_yocto_near NUMERIC(40, 0) NOT NULL DEFAULT 0
        CHECK (spent_yocto_near >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (usage_date, client_id)
);

CREATE TABLE settlements (
    id UUID PRIMARY KEY,
    api_client_id UUID NOT NULL REFERENCES api_clients(id),
    payment_identifier TEXT CHECK (
        payment_identifier IS NULL
        OR payment_identifier ~ '^[A-Za-z0-9_-]{16,128}$'
    ),
    payment_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(payment_hash) = 32),
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    state TEXT NOT NULL CHECK (
        state IN ('reserved', 'prepared', 'submitted', 'succeeded', 'failed')
    ),
    x402_version SMALLINT NOT NULL,
    scheme TEXT NOT NULL,
    network TEXT NOT NULL,
    asset TEXT NOT NULL,
    pay_to TEXT NOT NULL,
    amount NUMERIC(40, 0) NOT NULL CHECK (amount >= 0),
    payer TEXT NOT NULL,
    delegate_public_key TEXT NOT NULL,
    delegate_nonce NUMERIC(20, 0) NOT NULL CHECK (delegate_nonce >= 0),
    delegate_max_block_height NUMERIC(20, 0) NOT NULL
        CHECK (delegate_max_block_height >= 0),
    policy_snapshot JSONB NOT NULL,
    reservation_date DATE NOT NULL,
    reserved_yocto_near NUMERIC(40, 0) NOT NULL
        CHECK (reserved_yocto_near >= 0),
    relayer_account_id TEXT,
    relayer_public_key TEXT,
    relayer_nonce NUMERIC(20, 0),
    outer_transaction_bytes BYTEA,
    outer_transaction_hash TEXT,
    terminal_http_status SMALLINT,
    terminal_response_bytes BYTEA,
    error_code TEXT,
    error_detail TEXT,
    gas_burnt NUMERIC(40, 0),
    tokens_burnt NUMERIC(40, 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    prepared_at TIMESTAMPTZ,
    submitted_at TIMESTAMPTZ,
    finalized_at TIMESTAMPTZ,
    last_reconciled_at TIMESTAMPTZ,
    reconciliation_attempts INTEGER NOT NULL DEFAULT 0
        CHECK (reconciliation_attempts >= 0),
    CHECK (
        state IN ('reserved', 'failed')
        OR (
            relayer_account_id IS NOT NULL
            AND relayer_public_key IS NOT NULL
            AND relayer_nonce IS NOT NULL
            AND outer_transaction_bytes IS NOT NULL
            AND outer_transaction_hash IS NOT NULL
        )
    ),
    CHECK (
        state NOT IN ('succeeded', 'failed')
        OR (
            terminal_http_status IS NOT NULL
            AND terminal_response_bytes IS NOT NULL
            AND finalized_at IS NOT NULL
        )
    )
);

CREATE UNIQUE INDEX settlements_client_payment_identifier_idx
    ON settlements (api_client_id, payment_identifier)
    WHERE payment_identifier IS NOT NULL;

CREATE UNIQUE INDEX settlements_relayer_nonce_idx
    ON settlements (
        network,
        relayer_account_id,
        relayer_public_key,
        relayer_nonce
    )
    WHERE relayer_nonce IS NOT NULL;

CREATE INDEX settlements_nonterminal_idx
    ON settlements (state, created_at)
    WHERE state IN ('reserved', 'prepared', 'submitted');

CREATE TABLE settlement_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    settlement_id UUID NOT NULL REFERENCES settlements(id) ON DELETE CASCADE,
    from_state TEXT,
    to_state TEXT NOT NULL,
    code TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX settlement_events_settlement_idx
    ON settlement_events (settlement_id, id);
