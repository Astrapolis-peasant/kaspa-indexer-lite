use kaspa_hashes::Hash;
use sqlx::PgPool;

use crate::models::{DagBlockRow, IndexBatch};
use crate::processor::hash_to_bytes;

const MAX_PARAMS: usize = 60_000; // PostgreSQL limit is 65535

fn bytes_to_hash(bytes: Vec<u8>) -> Option<Hash> {
    if bytes.len() == 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(Hash::from_bytes(arr))
    } else {
        None
    }
}

pub async fn handle_reorg(pool: &PgPool, removed_hashes: &[Hash]) {
    let removed_bytes: Vec<Vec<u8>> = removed_hashes.iter().map(|h| hash_to_bytes(*h)).collect();
    let mut db_txn = pool.begin().await.expect("begin reorg transaction");

    sqlx::query("UPDATE transactions SET accepted_by = NULL WHERE accepted_by = ANY($1)")
        .bind(&removed_bytes)
        .execute(db_txn.as_mut())
        .await
        .expect("clear reorged acceptances");

    // Drop the reorged hashes from chain_blocks. dag_blocks is untouched —
    // those are still valid DAG blocks, they just aren't on the virtual chain.
    sqlx::query("DELETE FROM chain_blocks WHERE hash = ANY($1)")
        .bind(&removed_bytes)
        .execute(db_txn.as_mut())
        .await
        .expect("delete reorged chain_blocks");

    db_txn.commit().await.expect("commit reorg transaction");
}

/// Max-blue_score block in `dag_blocks` — resume checkpoint for the DAG fetcher.
pub async fn load_dag_tip(pool: &PgPool) -> Option<Hash> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT hash FROM dag_blocks ORDER BY blue_score DESC NULLS LAST LIMIT 1")
            .fetch_optional(pool)
            .await
            .expect("load dag tip");
    row.and_then(|(bytes,)| bytes_to_hash(bytes))
}

/// Max-blue_score chain block — resume checkpoint for the VCP fetcher and
/// the `last_committed` reset target after a reorg.
pub async fn load_tip_hash(pool: &PgPool) -> Option<Hash> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT hash FROM chain_blocks ORDER BY blue_score DESC NULLS LAST LIMIT 1")
            .fetch_optional(pool)
            .await
            .expect("load chain tip");
    row.and_then(|(bytes,)| bytes_to_hash(bytes))
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

    // 1. chain_blocks (15 cols → max ~4000 rows per chunk)
    for chunk in batch.chain_blocks.chunks(MAX_PARAMS / 15) {
        let sql = format!(
            "INSERT INTO chain_blocks
             (hash, selected_parent, parents, tx_count, accepted_id_merkle_root, bits,
              blue_score, blue_work, daa_score, hash_merkle_root, nonce,
              pruning_point, timestamp, utxo_commitment, version)
             VALUES {} ON CONFLICT (hash) DO NOTHING",
            placeholders(chunk.len(), 15)
        );
        let mut query = sqlx::query(&sql);
        for c in chunk {
            query = query
                .bind(&c.hash)
                .bind(&c.selected_parent)
                .bind(&c.parents)
                .bind(c.tx_count)
                .bind(&c.accepted_id_merkle_root)
                .bind(c.bits)
                .bind(c.blue_score)
                .bind(&c.blue_work)
                .bind(c.daa_score)
                .bind(&c.hash_merkle_root)
                .bind(&c.nonce)
                .bind(&c.pruning_point)
                .bind(c.timestamp)
                .bind(&c.utxo_commitment)
                .bind(c.version);
        }
        query.execute(db_txn.as_mut()).await.expect("insert chain_blocks");
    }

    // 2. transactions (12 cols, 250 rows per chunk)
    for chunk in batch.transactions.chunks(250) {
        let sql = format!(
            "INSERT INTO transactions
             (transaction_id, subnetwork_id, hash, mass, payload, block_time, version,
              block_hash, accepted_by, is_spam, inputs, outputs)
             VALUES {} ON CONFLICT (transaction_id) DO UPDATE SET
                 accepted_by = EXCLUDED.accepted_by,
                 block_hash  = EXCLUDED.block_hash,
                 block_time  = EXCLUDED.block_time,
                 is_spam     = EXCLUDED.is_spam",
            placeholders(chunk.len(), 12)
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
                .bind(t.is_spam)
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

/// Insert DAG block rows into `dag_blocks`. Sole writer to that table — no
/// contention with the VCP consumer.
pub async fn commit_dag_blocks(pool: &PgPool, rows: &[DagBlockRow]) -> usize {
    if rows.is_empty() { return 0; }
    let mut db_txn = pool.begin().await.expect("begin dag txn");
    for chunk in rows.chunks(MAX_PARAMS / 15) {
        let sql = format!(
            "INSERT INTO dag_blocks
             (hash, selected_parent, parents, tx_count,
              accepted_id_merkle_root, bits, blue_score, blue_work,
              daa_score, hash_merkle_root, nonce, pruning_point,
              timestamp, utxo_commitment, version)
             VALUES {} ON CONFLICT (hash) DO NOTHING",
            placeholders(chunk.len(), 15)
        );
        let mut query = sqlx::query(&sql);
        for b in chunk {
            query = query
                .bind(&b.hash)
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
        query.execute(db_txn.as_mut()).await.expect("insert dag_blocks");
    }
    db_txn.commit().await.expect("commit dag txn");
    rows.len()
}
