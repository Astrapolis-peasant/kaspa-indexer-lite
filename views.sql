-- PostgREST API schema for kaspa-indexer-lite
-- Exposes bytea columns directly; clients strip the `\x` prefix.

CREATE SCHEMA IF NOT EXISTS api;

DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'web_anon') THEN
    CREATE ROLE web_anon NOLOGIN;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticator') THEN
    CREATE ROLE authenticator NOINHERIT LOGIN PASSWORD 'authenticator';
  END IF;
END $$;

GRANT web_anon TO authenticator;
GRANT USAGE ON SCHEMA api, public TO web_anon;

-- All DAG blocks (virtual chain + non-chain).
CREATE OR REPLACE VIEW api.blocks AS
SELECT
  hash,
  selected_parent,
  parents,
  tx_count,
  blue_score,
  daa_score,
  timestamp,
  version,
  bits,
  nonce,
  hash_merkle_root,
  accepted_id_merkle_root,
  utxo_commitment,
  pruning_point,
  blue_work
FROM public.dag_blocks;

-- Chain blocks only (virtual chain). Full header fields — no join needed.
CREATE OR REPLACE VIEW api.chain_blocks AS
SELECT
  hash,
  selected_parent,
  parents,
  blue_score,
  daa_score,
  timestamp,
  version,
  bits,
  nonce,
  hash_merkle_root,
  accepted_id_merkle_root,
  utxo_commitment,
  pruning_point,
  blue_work
FROM public.chain_blocks;

-- Transactions (flat; inputs/outputs arrays passed through)
CREATE OR REPLACE VIEW api.transactions AS
SELECT
  transaction_id,
  hash,
  block_hash,
  accepted_by,
  block_time,
  mass,
  version,
  subnetwork_id,
  payload,
  is_spam,
  COALESCE(array_length(inputs, 1), 0)  AS input_count,
  COALESCE(array_length(outputs, 1), 0) AS output_count,
  inputs,
  outputs
FROM public.transactions;

-- Address → transaction mapping
CREATE OR REPLACE VIEW api.address_txs AS
SELECT
  address,
  transaction_id,
  block_time
FROM public.addresses_transactions;

GRANT SELECT ON ALL TABLES IN SCHEMA api TO web_anon;
GRANT SELECT ON
  public.dag_blocks,
  public.chain_blocks,
  public.transactions,
  public.addresses_transactions
TO web_anon;
