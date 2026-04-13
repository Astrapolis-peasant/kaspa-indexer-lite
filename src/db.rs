use kaspa_hashes::Hash;
use sqlx::{PgPool, Postgres, Transaction};

use crate::models::IndexBatch;
use crate::processor::hash_to_bytes;

const MAX_PARAMS: usize = 60_000; // PostgreSQL limit is 65535

pub async fn handle_reorg(pool: &PgPool, removed_hashes: &[Hash]) {
    let mut db_txn = pool.begin().await.expect("begin reorg transaction");
    for &removed_hash in removed_hashes {
        sqlx::query("UPDATE transactions SET accepted_by = NULL WHERE accepted_by = $1")
            .bind(hash_to_bytes(removed_hash))
            .execute(db_txn.as_mut())
            .await
            .expect("clear reorged acceptances");
    }
    db_txn.commit().await.expect("commit reorg transaction");
}

pub async fn load_checkpoint(pool: &PgPool) -> Option<Hash> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM vars WHERE key = 'vcp_start_hash'")
        .fetch_optional(pool)
        .await
        .expect("load checkpoint");
    row.map(|(v,)| v.parse::<Hash>().expect("invalid checkpoint hash"))
}

pub async fn save_checkpoint(db_txn: &mut Transaction<'_, Postgres>, hash: Hash) {
    sqlx::query(
        "INSERT INTO vars (key, value) VALUES ('vcp_start_hash', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(hash.to_string())
    .execute(db_txn.as_mut())
    .await
    .expect("save checkpoint");
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

pub async fn commit_index_batch(pool: &PgPool, batch: &IndexBatch, checkpoint: Hash) -> (usize, usize) {
    let mut db_txn = pool.begin().await.expect("begin transaction");

    // 1. blocks (12 cols → max 5000 rows per chunk)
    for chunk in batch.blocks.chunks(MAX_PARAMS / 12) {
        let sql = format!(
            "INSERT INTO blocks
             (hash, accepted_id_merkle_root, bits, blue_score, blue_work,
              daa_score, hash_merkle_root, nonce, pruning_point,
              timestamp, utxo_commitment, version)
             VALUES {} ON CONFLICT (hash) DO NOTHING",
            placeholders(chunk.len(), 12)
        );
        let mut query = sqlx::query(&sql);
        for b in chunk {
            query = query
                .bind(&b.hash)
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

    // 2. block_parent (2 cols → max 30000 rows)
    for chunk in batch.block_parents.chunks(MAX_PARAMS / 2) {
        let sql = format!(
            "INSERT INTO block_parent (block_hash, parent_hash) VALUES {} ON CONFLICT DO NOTHING",
            placeholders(chunk.len(), 2)
        );
        let mut query = sqlx::query(&sql);
        for bp in chunk {
            query = query.bind(&bp.block_hash).bind(&bp.parent_hash);
        }
        query.execute(db_txn.as_mut()).await.expect("insert block_parent");
    }

    // 3. transactions (11 cols, 250 rows per chunk)
    for chunk in batch.transactions.chunks(250) {
        let sql = format!(
            "INSERT INTO transactions
             (transaction_id, subnetwork_id, hash, mass, payload, block_time, version,
              block_hash, accepted_by, inputs, outputs)
             VALUES {} ON CONFLICT (transaction_id) DO UPDATE SET accepted_by = EXCLUDED.accepted_by",
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

    // 4. addresses_transactions (3 cols → max 20000 rows)
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

    save_checkpoint(&mut db_txn, checkpoint).await;
    db_txn.commit().await.expect("commit transaction");

    (batch.transactions.len(), batch.address_transactions.len())
}
