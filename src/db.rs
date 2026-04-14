use kaspa_hashes::Hash;
use sqlx::PgPool;

use crate::models::{BlockRow, IndexBatch};
use crate::processor::hash_to_bytes;

const MAX_PARAMS: usize = 60_000; // PostgreSQL limit is 65535

pub async fn handle_reorg(pool: &PgPool, removed_hashes: &[Hash]) {
    let removed_bytes: Vec<Vec<u8>> = removed_hashes.iter().map(|h| hash_to_bytes(*h)).collect();
    let mut db_txn = pool.begin().await.expect("begin reorg transaction");

    sqlx::query("UPDATE transactions SET accepted_by = NULL WHERE accepted_by = ANY($1)")
        .bind(&removed_bytes)
        .execute(db_txn.as_mut())
        .await
        .expect("clear reorged acceptances");

    // Keep the blocks (they're still DAG blocks); just flip the chain flag.
    sqlx::query("UPDATE blocks SET is_chain_block = false WHERE hash = ANY($1)")
        .bind(&removed_bytes)
        .execute(db_txn.as_mut())
        .await
        .expect("flip reorged chain flag");

    db_txn.commit().await.expect("commit reorg transaction");
}

/// Max-blue_score block in `blocks` regardless of chain status — used as
/// the resume checkpoint for the DAG fetcher.
pub async fn load_dag_tip(pool: &PgPool) -> Option<Hash> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT hash FROM blocks ORDER BY blue_score DESC NULLS LAST LIMIT 1")
            .fetch_optional(pool)
            .await
            .expect("load dag tip");
    row.and_then(|(bytes,)| {
        if bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Some(Hash::from_bytes(arr))
        } else {
            None
        }
    })
}

/// The virtual-chain tip as far as our DB is concerned — max-blue_score
/// chain block currently in `blocks`. Used both as the resume checkpoint
/// on startup and to reset `last_committed` after a reorg.
pub async fn load_tip_hash(pool: &PgPool) -> Option<Hash> {
    // Chain tip for VCP: the highest-blue_score chain block.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT hash FROM blocks WHERE is_chain_block ORDER BY blue_score DESC NULLS LAST LIMIT 1")
            .fetch_optional(pool)
            .await
            .expect("load tip");
    row.and_then(|(bytes,)| {
        if bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Some(Hash::from_bytes(arr))
        } else {
            None
        }
    })
}

fn placeholders(rows: usize, cols: usize) -> String {
    (0..rows)
        .map(|r| {
            let params = (0..cols)
                .map(|c| format!("${}", r * cols + c + 1))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({params})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub async fn commit_index_batch(pool: &PgPool, batch: &IndexBatch) -> (usize, usize) {
    let mut db_txn = pool.begin().await.expect("begin transaction");

    // 1. blocks (16 cols → max ~3700 rows per chunk)
    // ON CONFLICT: flip is_chain_block=true; fill in selected_parent/parents/
    // tx_count if DAG writer hasn't arrived yet.
    for chunk in batch.blocks.chunks(MAX_PARAMS / 16) {
        let sql = format!(
            "INSERT INTO blocks
             (hash, is_chain_block, selected_parent, parents, tx_count,
              accepted_id_merkle_root, bits, blue_score, blue_work,
              daa_score, hash_merkle_root, nonce, pruning_point,
              timestamp, utxo_commitment, version)
             VALUES {} ON CONFLICT (hash) DO UPDATE SET
                 is_chain_block  = true,
                 selected_parent = COALESCE(blocks.selected_parent, EXCLUDED.selected_parent),
                 parents         = COALESCE(blocks.parents,         EXCLUDED.parents),
                 tx_count        = COALESCE(blocks.tx_count,        EXCLUDED.tx_count)",
            placeholders(chunk.len(), 16)
        );
        let mut query = sqlx::query(&sql);
        for b in chunk {
            query = query
                .bind(&b.hash)
                .bind(b.is_chain_block)
                .bind(&b.selected_parent)
                .bind(&b.parents)
                .bind(b.tx_count)
                .bind(&b.accepted_id_merkle_root)
                .bind(b.bits)
                .bind(b.blue_score)
                .bind(&b.blue_work)
                .bind(b.daa_score)
                .bind(&b.hash_merkle_root)
                .bind(&b.nonce)
                .bind(&b.pruning_point)
                .bind(b.timestamp)
                .bind(&b.utxo_commitment)
                .bind(b.version);
        }
        query.execute(db_txn.as_mut()).await.expect("insert blocks");
    }

    // 2. transactions (11 cols, 250 rows per chunk)
    for chunk in batch.transactions.chunks(250) {
        let sql = format!(
            "INSERT INTO transactions
             (transaction_id, subnetwork_id, hash, mass, payload, block_time, version,
              block_hash, accepted_by, inputs, outputs)
             VALUES {} ON CONFLICT (transaction_id) DO UPDATE SET
                 accepted_by = EXCLUDED.accepted_by,
                 block_hash  = EXCLUDED.block_hash,
                 block_time  = EXCLUDED.block_time",
            placeholders(chunk.len(), 11)
        );
        let mut query = sqlx::query(&sql);
        for t in chunk {
            query = query
                .bind(&t.transaction_id)
                .bind(&t.subnetwork_id)
                .bind(&t.hash)
                .bind(t.mass)
                .bind(&t.payload)
                .bind(t.block_time)
                .bind(t.version)
                .bind(&t.block_hash)
                .bind(&t.accepted_by)
                .bind(&t.inputs)
                .bind(&t.outputs);
        }
        query.execute(db_txn.as_mut()).await.expect("insert transactions");
    }

    // 3. addresses_transactions (3 cols → max 20000 rows)
    for chunk in batch.address_transactions.chunks(MAX_PARAMS / 3) {
        let sql = format!(
            "INSERT INTO addresses_transactions (address, transaction_id, block_time)
             VALUES {}",
            placeholders(chunk.len(), 3)
        );
        let mut query = sqlx::query(&sql);
        for at in chunk {
            query = query.bind(&at.address).bind(&at.transaction_id).bind(at.block_time);
        }
        query.execute(db_txn.as_mut()).await.expect("insert addresses_transactions");
    }

    db_txn.commit().await.expect("commit transaction");

    (batch.transactions.len(), batch.address_transactions.len())
}

/// Insert DAG block rows into the unified `blocks` table. The DAG side
/// owns all header fields + `parents`; it respects whatever
/// `is_chain_block` the VCP side may have already set (true wins true).
pub async fn commit_dag_blocks(pool: &PgPool, rows: &[BlockRow]) -> usize {
    if rows.is_empty() { return 0; }
    let mut db_txn = pool.begin().await.expect("begin dag txn");
    for chunk in rows.chunks(MAX_PARAMS / 16) {
        let sql = format!(
            "INSERT INTO blocks
             (hash, is_chain_block, selected_parent, parents, tx_count,
              accepted_id_merkle_root, bits, blue_score, blue_work,
              daa_score, hash_merkle_root, nonce, pruning_point,
              timestamp, utxo_commitment, version)
             VALUES {} ON CONFLICT (hash) DO UPDATE SET
                 is_chain_block  = blocks.is_chain_block OR EXCLUDED.is_chain_block,
                 selected_parent = COALESCE(EXCLUDED.selected_parent, blocks.selected_parent),
                 parents         = COALESCE(EXCLUDED.parents,         blocks.parents),
                 tx_count        = COALESCE(EXCLUDED.tx_count,        blocks.tx_count)",
            placeholders(chunk.len(), 16)
        );
        let mut query = sqlx::query(&sql);
        for b in chunk {
            query = query
                .bind(&b.hash)
                .bind(b.is_chain_block)
                .bind(&b.selected_parent)
                .bind(&b.parents)
                .bind(b.tx_count)
                .bind(&b.accepted_id_merkle_root)
                .bind(b.bits)
                .bind(b.blue_score)
                .bind(&b.blue_work)
                .bind(b.daa_score)
                .bind(&b.hash_merkle_root)
                .bind(&b.nonce)
                .bind(&b.pruning_point)
                .bind(b.timestamp)
                .bind(&b.utxo_commitment)
                .bind(b.version);
        }
        query.execute(db_txn.as_mut()).await.expect("insert dag blocks");
    }
    db_txn.commit().await.expect("commit dag txn");
    rows.len()
}
