// PostgreSQL composite types (field order must match CREATE TYPE)

#[derive(sqlx::Type, Clone)]
#[sqlx(type_name = "transactions_inputs")]
pub struct TransactionInput {
    pub index:                    i16,
    pub previous_outpoint_hash:   Option<Vec<u8>>,
    pub previous_outpoint_index:  Option<i16>,
    pub sig_op_count:             Option<i16>,
    pub previous_outpoint_script: Option<Vec<u8>>,
    pub previous_outpoint_amount: Option<i64>,
}

#[derive(sqlx::Type, Clone)]
#[sqlx(type_name = "transactions_outputs")]
pub struct TransactionOutput {
    pub index:                     i16,
    pub amount:                    Option<i64>,
    pub script_public_key:         Option<Vec<u8>>,
    pub script_public_key_address: Option<String>,
}

// DB row structs

pub struct BlockRow {
    pub hash:                    Vec<u8>,
    pub is_chain_block:          bool,
    pub selected_parent:         Option<Vec<u8>>,
    pub parents:                 Option<Vec<Vec<u8>>>,
    pub accepted_id_merkle_root: Option<Vec<u8>>,
    pub bits:                    Option<i64>,
    pub blue_score:              Option<i64>,
    pub blue_work:               Option<Vec<u8>>,
    pub daa_score:               Option<i64>,
    pub hash_merkle_root:        Option<Vec<u8>>,
    pub nonce:                   Option<Vec<u8>>,
    pub pruning_point:           Option<Vec<u8>>,
    pub timestamp:               Option<i64>,
    pub utxo_commitment:         Option<Vec<u8>>,
    pub version:                 Option<i16>,
}

pub struct TransactionRow {
    pub transaction_id: Vec<u8>,
    pub subnetwork_id:  Option<Vec<u8>>,
    pub hash:           Option<Vec<u8>>,
    pub mass:           Option<i32>,
    pub payload:        Option<Vec<u8>>,
    pub block_time:     Option<i64>,
    pub version:        Option<i16>,
    pub block_hash:     Option<Vec<u8>>,
    pub accepted_by:    Vec<u8>,
    pub inputs:         Option<Vec<TransactionInput>>,
    pub outputs:        Option<Vec<TransactionOutput>>,
}

pub struct AddressTransactionRow {
    pub address:        String,
    pub transaction_id: Vec<u8>,
    pub block_time:     i64,
}

pub struct IndexBatch {
    pub blocks:                  Vec<BlockRow>,
    pub transactions:            Vec<TransactionRow>,
    pub address_transactions:    Vec<AddressTransactionRow>,
}
