mod db;
mod kaspad;
mod models;
mod processor;

use std::time::{Duration, Instant};

use clap::Parser;
use kaspa_hashes::Hash;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_rpc_core::{GetVirtualChainFromBlockV2Response, RpcDataVerbosityLevel};
use log::{debug, info, warn};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::mpsc;

use crate::db::{commit_index_batch, handle_reorg, load_checkpoint, load_tip_hash};
use crate::processor::build_index_batches;

#[derive(Parser, Debug)]
#[command(name = "indexer-lite", about = "Kaspa VCP indexer (borsh wRPC)")]
struct Args {
    /// kaspad borsh wRPC URL
    #[arg(short = 'w', long, default_value = "ws://localhost:17110")]
    kaspad_ws: String,

    /// PostgreSQL connection string
    #[arg(short = 'd', long)]
    database_url: String,

    /// Blocks to commit per DB transaction (server returns ~350, sliced client-side)
    #[arg(short = 'p', long, default_value_t = 100)]
    page_size: usize,

    /// Minimum confirmations behind tip (safety buffer)
    #[arg(short = 'c', long, default_value_t = 10)]
    min_confirmations: u64,

    /// Poll interval (ms) when caught up to tip
    #[arg(short = 'i', long, default_value_t = 1000)]
    poll_interval_ms: u64,

    /// Override starting block hash (ignores saved checkpoint)
    #[arg(short = 's', long)]
    start_hash: Option<String>,

    /// Number of DB connections in pool
    #[arg(long, default_value_t = 4)]
    db_pool_size: u32,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,sqlx=error")).init();

    let args = Args::parse();

    let pool = PgPoolOptions::new()
        .max_connections(args.db_pool_size)
        .connect(&args.database_url)
        .await
        .expect("connect to database");

    info!(
        "indexer-lite | kaspad={} | page_size={} | min_conf={}",
        args.kaspad_ws, args.page_size, args.min_confirmations
    );

    let poll_interval = Duration::from_millis(args.poll_interval_ms);

    loop {
        let client = kaspad::connect(&args.kaspad_ws).await;
        info!("Connected to kaspad");

        let start_hash = match &args.start_hash {
            Some(h) => {
                let hash = h.parse::<Hash>().expect("invalid --start-hash");
                info!("Starting from --start-hash {}", hash);
                sqlx::query(
                    "INSERT INTO vars (key, value) VALUES ('vcp_start_hash', $1)
                     ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
                )
                .bind(hash.to_string())
                .execute(&pool)
                .await
                .expect("save start hash");
                hash
            }
            None => match load_checkpoint(&pool).await {
                Some(hash) => {
                    info!("Resuming from {}", hash);
                    hash
                }
                None => {
                    let dag_info      = client.get_block_dag_info().await.expect("get_block_dag_info");
                    let pruning_point = dag_info.pruning_point_hash;
                    info!("No checkpoint — starting from pruning point {}", pruning_point);
                    sqlx::query(
                        "INSERT INTO vars (key, value) VALUES ('vcp_start_hash', $1)
                         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
                    )
                    .bind(pruning_point.to_string())
                    .execute(&pool)
                    .await
                    .expect("save pruning point");
                    pruning_point
                }
            },
        };

        let (vcp_sender, mut vcp_receiver) = mpsc::channel::<GetVirtualChainFromBlockV2Response>(5);

        let page_size = args.page_size;
        let min_conf  = args.min_confirmations;

        let fetcher_task = tokio::spawn(async move {
            let mut fetch_from = start_hash;
            loop {
                let t0 = Instant::now();
                match client
                    .get_virtual_chain_from_block_v2(
                        fetch_from,
                        Some(RpcDataVerbosityLevel::Full),
                        Some(min_conf),
                    )
                    .await
                {
                    Ok(response) => {
                        let count = response.added_chain_block_hashes.len();
                        if count > 0 {
                            fetch_from = *response.added_chain_block_hashes.last().unwrap();
                            info!(
                                "fetched {:4} blocks in {:.2}s | {}",
                                count,
                                t0.elapsed().as_secs_f64(),
                                fetch_from,
                            );
                            if vcp_sender.send(response).await.is_err() {
                                break;
                            }
                        } else {
                            debug!("at tip, sleeping");
                            tokio::time::sleep(poll_interval).await;
                        }
                    }
                    Err(e) => {
                        warn!("VCP error: {} — retrying in 5s", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
            info!("Fetcher stopped");
            drop(client);
        });

        let db_pool = pool.clone();
        let consumer_task = tokio::spawn(async move {
            // Previous chain block on the virtual chain — used so each new
            // chain block can record its selected_parent = previous one.
            let mut last_committed: Option<Hash> = Some(start_hash);
            while let Some(response) = vcp_receiver.recv().await {
                if !response.removed_chain_block_hashes.is_empty() {
                    warn!("Reorg: removing {} chain blocks", response.removed_chain_block_hashes.len());
                    handle_reorg(&db_pool, &response.removed_chain_block_hashes).await;
                    // Previous tip may have been deleted; reset pointer to
                    // the surviving max-blue_score block (the LCA) so the
                    // next added block gets the right selected_parent.
                    last_committed = load_tip_hash(&db_pool).await;
                }

                let added_hashes  = &response.added_chain_block_hashes;
                let chain_entries = &response.chain_block_accepted_transactions;

                if added_hashes.is_empty() {
                    continue;
                }

                let index_batches = build_index_batches(chain_entries, page_size, last_committed);

                for (i, index_batch) in index_batches.iter().enumerate() {
                    let t0         = Instant::now();
                    let chunk_end  = std::cmp::min((i + 1) * page_size, added_hashes.len());
                    let checkpoint = added_hashes[chunk_end - 1];
                    let (tx_count, addr_count) =
                        commit_index_batch(&db_pool, index_batch, checkpoint).await;
                    last_committed = Some(checkpoint);
                    info!(
                        "+{:4} blocks | {:6} tx | {:6} addr | {:.2}s | {}",
                        index_batch.blocks.len(),
                        tx_count,
                        addr_count,
                        t0.elapsed().as_secs_f64(),
                        checkpoint,
                    );
                }
            }
        });

        tokio::select! {
            _ = fetcher_task  => { warn!("Fetcher exited — reconnecting in 5s"); }
            _ = consumer_task => { warn!("Consumer exited — reconnecting in 5s"); }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
