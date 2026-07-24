-- Migration 0002: multi-chain (eip155 / EVM) settlement columns.
--
-- Additive and back-compatible for NEAR. Every existing NEAR row continues to
-- satisfy every constraint: the new columns are nullable (or carry a NEAR
-- default), the delegate identity is re-required for NEAR rows by a conditional
-- CHECK, and the non-terminal integrity CHECK becomes chain-conditional so a
-- future EVM row (signer / authorization columns) is accepted while NEAR rows
-- keep requiring the delegate / relayer / outer-transaction set.
--
-- No EVM row is written until the EVM provider ships; on a NEAR instance these
-- columns stay NULL and chain_kind stays 'near'. One superset schema is applied
-- to every instance database so the migration checksum is identical across the
-- fleet (schema_compatible() requires every embedded migration on every DB).
--
-- api_clients.environment stays IN ('mainnet','testnet'): environment is the
-- deployment tier (prod/test), chain-agnostic, and each instance owns its own
-- database, so no widening is needed there.

-- Chain discriminator. Existing rows and NEAR inserts default to 'near'; the EVM
-- provider sets 'eip155' explicitly.
ALTER TABLE settlements
    ADD COLUMN chain_kind TEXT NOT NULL DEFAULT 'near'
        CHECK (chain_kind IN ('near', 'eip155'));

-- EVM (eip155) authorization + submission columns; all nullable so NEAR rows
-- leave them NULL. Atomic-unit / count columns are non-negative when present.
ALTER TABLE settlements
    ADD COLUMN evm_authorization JSONB,
    ADD COLUMN signer_address TEXT,
    ADD COLUMN signer_account_nonce NUMERIC(20, 0)
        CHECK (signer_account_nonce IS NULL OR signer_account_nonce >= 0),
    ADD COLUMN submitted_tx_rlp BYTEA,
    ADD COLUMN submitted_tx_hash TEXT,
    ADD COLUMN mined_block_number NUMERIC(20, 0)
        CHECK (mined_block_number IS NULL OR mined_block_number >= 0),
    ADD COLUMN mined_block_hash TEXT,
    ADD COLUMN confirmations INTEGER
        CHECK (confirmations IS NULL OR confirmations >= 0),
    ADD COLUMN required_confirmations INTEGER
        CHECK (required_confirmations IS NULL OR required_confirmations >= 0);

-- The delegate identity is NEAR-specific; make it nullable so EVM rows omit it.
-- NEAR rows are held to the same requirement by the conditional CHECK below.
ALTER TABLE settlements
    ALTER COLUMN delegate_public_key DROP NOT NULL,
    ALTER COLUMN delegate_nonce DROP NOT NULL,
    ALTER COLUMN delegate_max_block_height DROP NOT NULL;

-- Authorization identity by chain: NEAR rows carry the full delegate identity;
-- EVM rows carry the EIP-3009 authorization + signer address instead.
ALTER TABLE settlements
    ADD CONSTRAINT settlements_chain_authorization_check CHECK (
        (chain_kind = 'near'
            AND delegate_public_key IS NOT NULL
            AND delegate_nonce IS NOT NULL
            AND delegate_max_block_height IS NOT NULL)
        OR (chain_kind = 'eip155'
            AND evm_authorization IS NOT NULL
            AND signer_address IS NOT NULL)
    );

-- Relax the non-terminal integrity CHECK to be chain-conditional. NEAR rows keep
-- requiring the relayer / outer-transaction set; EVM rows require the signer +
-- submitted-transaction set. The original constraint was created unnamed by
-- 0001, so it is located by its definition (the only settlements CHECK that
-- references relayer_account_id) and dropped; a fail-loud guard fires if the
-- expected constraint is absent rather than silently leaving the old rule.
DO $$
DECLARE
    target text;
BEGIN
    SELECT conname INTO target
    FROM pg_constraint
    WHERE conrelid = 'settlements'::regclass
      AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%relayer_account_id IS NOT NULL%';
    IF target IS NULL THEN
        RAISE EXCEPTION 'settlements non-terminal CHECK (referencing relayer_account_id) not found';
    END IF;
    EXECUTE format('ALTER TABLE settlements DROP CONSTRAINT %I', target);
END $$;

ALTER TABLE settlements
    ADD CONSTRAINT settlements_nonterminal_submission_check CHECK (
        state IN ('reserved', 'failed')
        OR (chain_kind = 'near'
            AND relayer_account_id IS NOT NULL
            AND relayer_public_key IS NOT NULL
            AND relayer_nonce IS NOT NULL
            AND outer_transaction_bytes IS NOT NULL
            AND outer_transaction_hash IS NOT NULL)
        OR (chain_kind = 'eip155'
            AND signer_address IS NOT NULL
            AND signer_account_nonce IS NOT NULL
            AND submitted_tx_rlp IS NOT NULL
            AND submitted_tx_hash IS NOT NULL)
    );
