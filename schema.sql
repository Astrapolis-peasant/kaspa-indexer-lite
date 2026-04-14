-- indexer-lite database schema
-- Apply with: psql -U postgres -h localhost kaspa_indexer_lite -f schema.sql

-- Composite types for transaction inputs/outputs
CREATE TYPE transactions_inputs AS (
    index                    SMALLINT,
    previous_outpoint_hash   BYTEA,
    previous_outpoint_index  SMALLINT,
    signature_script         BYTEA,
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

-- Unified block table: every DAG block kaspad returns via get_blocks.
-- `is_chain_block` marks blocks currently on the virtual chain.
-- `selected_parent` comes from verbose_data.selected_parent_hash (GHOSTDAG);
-- for chain blocks this is the previous chain block on the virtual chain.
-- `parents` is the level-0 DAG parent list (header.parents_by_level[0]).
-- `tx_ids` is the list of transaction_ids included in this DAG block.
CREATE TABLE IF NOT EXISTS blocks (
    hash                    BYTEA PRIMARY KEY,
    is_chain_block          BOOLEAN NOT NULL DEFAULT false,
    selected_parent         BYTEA,
    parents                 BYTEA[],
    tx_ids                  BYTEA[],
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
CREATE INDEX IF NOT EXISTS idx_blocks_selected_parent ON blocks (selected_parent);
CREATE INDEX IF NOT EXISTS idx_blocks_blue_score       ON blocks (blue_score);
CREATE INDEX IF NOT EXISTS idx_blocks_chain_blue_score ON blocks (blue_score) WHERE is_chain_block;

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

-- Address to transaction lookup (deduped in Rust, no PK)
CREATE TABLE IF NOT EXISTS addresses_transactions (
    address        TEXT NOT NULL,
    transaction_id BYTEA NOT NULL,
    block_time     BIGINT
);
CREATE INDEX IF NOT EXISTS idx_addresses_transactions_address
    ON addresses_transactions (address);
