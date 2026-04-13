use kaspa_hashes::Hash;
use sqlx::PgPool;

use crate::models::IndexBatch;
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

    sqlx::query("DELETE FROM blocks WHERE hash = ANY($1)")
        .bind(&removed_bytes)
        .execute(db_txn.as_mut())
        .await
        .expect("delete reorged blocks");

    db_txn.commit().await.expect("commit reorg transaction");
}

/// The virtual-chain tip as far as our DB is concerned — max-blue_score
/// block currently in `blocks`. Used both as the resume checkpoint on
/// startup and to reset `last_committed` after a reorg delete.
pub async fn load_tip_hash(pool: &PgPool) -> Option<Hash> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT hash FROM blocks ORDER BY blue_score DESC NULLS LAST LIMIT 1")
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

    // 1. blocks (13 cols → max ~4600 rows per chunk)
    for chunk in batch.blocks.chunks(MAX_PARAMS / 13) {
        let sql = format!(
            "INSERT INTO blocks
             (hash, selected_parent, accepted_id_merkle_root, bits, blue_score, blue_work,
              daa_score, hash_merkle_root, nonce, pruning_point,
              timestamp, utxo_commitment, version)
             VALUES {} ON CONFLICT (hash) DO NOTHING",
            placeholders(chunk.len(), 13)
        );
        let mut query = sqlx::query(&sql);
        for b in chunk {
            query = query
                .bind(&b.hash)
                .bind(&b.selected_parent)
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
