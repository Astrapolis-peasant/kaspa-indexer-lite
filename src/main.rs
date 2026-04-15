mod db;
mod kaspad;
mod models;
mod processor;

use std::time::{Duration, Instant};

use clap::Parser;
use kaspa_hashes::Hash;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_rpc_core::{GetVirtualChainFromBlockV2Response, RpcBlock, RpcDataVerbosityLevel};
use log::{debug, info, warn};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::mpsc;

use crate::db::{commit_dag_blocks, commit_index_batch, handle_reorg, load_dag_tip, load_tip_hash};
use crate::processor::{build_dag_block_rows, build_index_batches};

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
        let vcp_client = kaspad::connect(&args.kaspad_ws).await;
        let dag_client = kaspad::connect(&args.kaspad_ws).await;
        info!("Connected to kaspad (2 wRPC connections)");

        let start_hash = match args.start_hash.as_deref() {
            Some(h) => {
                let hash = h.parse::<Hash>().expect("invalid --start-hash");
                info!("Starting from --start-hash {}", hash);
                hash
            }
            None => match load_tip_hash(&pool).await {
                Some(hash) => {
                    info!("Resuming from DB tip {}", hash);
                    hash
                }
                None => {
                    let pp = vcp_client.get_block_dag_info().await.expect("get_block_dag_info").pruning_point_hash;
                    info!("Fresh DB — starting from pruning point {}", pp);
                    pp
                }
            },
        };

        // DAG fetcher startup point: resume from max-blue_score block in `blocks`,
        // else pruning point.
        let dag_start = match load_dag_tip(&pool).await {
            Some(h) => { info!("DAG resuming from {}", h); h }
            None    => {
                let pp = dag_client.get_block_dag_info().await.expect("get_block_dag_info").pruning_point_hash;
                info!("DAG fresh — starting from pruning point {}", pp);
                pp
            }
        };

        let (vcp_sender, mut vcp_receiver) = mpsc::channel::<GetVirtualChainFromBlockV2Response>(5);
        let (dag_sender, mut dag_receiver) = mpsc::channel::<Vec<RpcBlock>>(5);

        let page_size    = args.page_size;
        let min_conf     = args.min_confirmations;

        let fetcher_task = tokio::spawn(async move {
            let client = vcp_client;
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
                    let tip        = added_hashes[chunk_end - 1];
                    let (tx_count, addr_count) = commit_index_batch(&db_pool, index_batch).await;
                    last_committed = Some(tip);
                    info!(
                        "+{:4} blocks | {:6} tx | {:6} addr | {:.2}s | {}",
                        index_batch.chain_blocks.len(),
                        tx_count,
                        addr_count,
                        t0.elapsed().as_secs_f64(),
                        tip,
                    );
                }
            }
        });

        let dag_pool = pool.clone();
        let dag_fetcher_task = tokio::spawn(async move {
            let mut low = dag_start;
            loop {
                let t0 = Instant::now();
                match dag_client.get_blocks(Some(low), true, false).await {
                    Ok(resp) => {
                        if resp.blocks.is_empty() {
                            debug!("dag: at tip");
                            tokio::time::sleep(poll_interval).await;
                            continue;
                        }
                        if let Some(last) = resp.block_hashes.last() {
                            low = *last;
                        }
                        info!("dag fetched {:4} blocks in {:.2}s | {}",
                              resp.blocks.len(), t0.elapsed().as_secs_f64(), low);
                        if dag_sender.send(resp.blocks).await.is_err() { break; }
                    }
                    Err(e) => {
                        warn!("get_blocks error: {} — retrying in 5s", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
            info!("DAG fetcher stopped");
            drop(dag_client);
        });

        let dag_consumer_task = tokio::spawn(async move {
            while let Some(blocks) = dag_receiver.recv().await {
                let t0 = Instant::now();
                let rows = build_dag_block_rows(&blocks);
                let n    = commit_dag_blocks(&dag_pool, &rows).await;
                info!("dag: +{:4} blocks | {:.2}s", n, t0.elapsed().as_secs_f64());
            }
        });

        tokio::select! {
            _ = fetcher_task      => { warn!("VCP fetcher exited — reconnecting in 5s"); }
            _ = consumer_task     => { warn!("VCP consumer exited — reconnecting in 5s"); }
            _ = dag_fetcher_task  => { warn!("DAG fetcher exited — reconnecting in 5s"); }
            _ = dag_consumer_task => { warn!("DAG consumer exited — reconnecting in 5s"); }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
