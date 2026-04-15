-- indexer-lite database schema
-- Apply with: psql -U postgres -h localhost kaspa_indexer_lite -f schema.sql

-- Composite types for transaction inputs/outputs
CREATE TYPE transactions_inputs AS (
    index                    SMALLINT,
    previous_outpoint_hash   BYTEA,
    previous_outpoint_index  SMALLINT,
    sig_op_count             SMALLINT,
    previous_outpoint_script BYTEA,
    previous_outpoint_amount BIGINT
);

CREATE TYPE transactions_outputs AS (
    index                     SMALLINT,
    amount                    BIGINT,
    script_public_key         BYTEA,
    script_public_key_address TEXT
);

-- DAG blocks: every block kaspad reports via get_blocks.
-- Written ONLY by the DAG consumer. No contention with VCP.
-- selected_parent is GHOSTDAG's selected parent (verbose_data.selected_parent_hash).
-- For blocks that are on the virtual chain, selected_parent is also the
-- previous chain block — the two definitions coincide.
CREATE TABLE IF NOT EXISTS dag_blocks (
    hash                    BYTEA PRIMARY KEY,
    selected_parent         BYTEA,
    parents                 BYTEA[],
    tx_count                SMALLINT,
    accepted_id_merkle_root BYTEA,
    bits                    BIGINT,
    blue_score              BIGINT,
    blue_work               BYTEA,
    daa_score               BIGINT,
    hash_merkle_root        BYTEA,
    nonce                   BYTEA,
    pruning_point           BYTEA,
    timestamp               BIGINT,
    utxo_commitment         BYTEA,
    version                 SMALLINT
);
CREATE INDEX IF NOT EXISTS idx_dag_blocks_selected_parent ON dag_blocks (selected_parent);
CREATE INDEX IF NOT EXISTS idx_dag_blocks_blue_score      ON dag_blocks (blue_score);

-- Chain blocks: blocks currently on the virtual chain. Written ONLY by the
-- VCP consumer. No contention with DAG. Carries full header fields (same
-- shape as dag_blocks) so chain-block queries don't need a join; the ~10%
-- duplication vs dag_blocks is ~140 MB at a 2-day pruning window.
-- Reorg = DELETE the removed hashes.
CREATE TABLE IF NOT EXISTS chain_blocks (
    hash                    BYTEA PRIMARY KEY,
    selected_parent         BYTEA,
    parents                 BYTEA[],
    accepted_id_merkle_root BYTEA,
    bits                    BIGINT,
    blue_score              BIGINT,
    blue_work               BYTEA,
    daa_score               BIGINT,
    hash_merkle_root        BYTEA,
    nonce                   BYTEA,
    pruning_point           BYTEA,
    timestamp               BIGINT,
    utxo_commitment         BYTEA,
    version                 SMALLINT
);
CREATE INDEX IF NOT EXISTS idx_chain_blocks_blue_score      ON chain_blocks (blue_score);
CREATE INDEX IF NOT EXISTS idx_chain_blocks_selected_parent ON chain_blocks (selected_parent);

-- Transactions with inputs/outputs as composite type arrays.
-- block_hash: DAG block that included the tx (verbose_data.block_hash)
-- accepted_by: chain block that accepted the tx (chain_block_header.hash)
CREATE TABLE IF NOT EXISTS transactions (
    transaction_id BYTEA PRIMARY KEY,
    subnetwork_id  BYTEA,
    hash           BYTEA,
    mass           INTEGER,
    payload        BYTEA,
    block_time     BIGINT,
    version        SMALLINT,
    block_hash     BYTEA,
    accepted_by    BYTEA,
    inputs         transactions_inputs[],
    outputs        transactions_outputs[]
);
CREATE INDEX IF NOT EXISTS idx_transactions_accepted_by ON transactions (accepted_by);
CREATE INDEX IF NOT EXISTS idx_transactions_block_hash  ON transactions (block_hash) WHERE block_hash IS NOT NULL;
ALTER TABLE transactions ALTER COLUMN payload SET COMPRESSION lz4;

-- Address to transaction lookup (deduped in Rust, no PK)
CREATE TABLE IF NOT EXISTS addresses_transactions (
    address        TEXT NOT NULL,
    transaction_id BYTEA NOT NULL,
    block_time     BIGINT
);
CREATE INDEX IF NOT EXISTS idx_addresses_transactions_address
    ON addresses_transactions (address);
