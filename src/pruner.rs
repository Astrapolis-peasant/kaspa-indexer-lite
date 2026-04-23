use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::info;
use sqlx::PgPool;

const PRUNE_SQL: &str = "
WITH expired_dag AS (
    SELECT hash, timestamp FROM dag_blocks
    WHERE timestamp < $1
    ORDER BY blue_score ASC LIMIT 1000
),
time_bounds AS (
    SELECT min(timestamp) AS lo, max(timestamp) AS hi FROM expired_dag
),
spam_txs AS (
    SELECT transaction_id FROM transactions
    WHERE block_hash IN (SELECT hash FROM expired_dag)
      AND is_spam = true
),
del_addr AS (
    DELETE FROM addresses_transactions at
    USING time_bounds tb
    WHERE at.block_time >= tb.lo AND at.block_time <= tb.hi
      AND at.transaction_id IN (SELECT transaction_id FROM spam_txs)
    RETURNING 1
),
del_tx AS (
    DELETE FROM transactions
    WHERE transaction_id IN (SELECT transaction_id FROM spam_txs)
    RETURNING 1
),
del_dag AS (
    DELETE FROM dag_blocks
    WHERE hash IN (SELECT hash FROM expired_dag)
    RETURNING 1
)
SELECT
    (SELECT count(*) FROM del_dag)  AS dag_pruned,
    (SELECT count(*) FROM del_tx)   AS tx_pruned,
    (SELECT count(*) FROM del_addr) AS addr_pruned
";

const RETENTION_MS: i64 = 7 * 86_400 * 1_000;

pub async fn run(pool: PgPool) {
    loop {
        prune_expired(&pool).await;
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

async fn prune_expired(pool: &PgPool) {
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let cutoff_ms = now_ms - RETENTION_MS;
    let mut total_dag: i64 = 0;
    let mut total_tx: i64 = 0;
    let mut total_addr: i64 = 0;
    loop {
        let row: (i64, i64, i64) = sqlx::query_as(PRUNE_SQL)
            .bind(cutoff_ms)
            .fetch_one(pool)
            .await
            .expect("prune_expired");
        total_dag += row.0;
        total_tx += row.1;
        total_addr += row.2;
        if row.0 == 0 { break; }
    }
    if total_dag + total_tx > 0 {
        info!("pruned {} dag blocks, {} spam txs, {} addr_txs", total_dag, total_tx, total_addr);
    }
}
