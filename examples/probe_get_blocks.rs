// Quick probe: call get_blocks and get_block with verbose_data, print field shape.
// Run: cargo run --release --example probe_get_blocks -- ws://localhost:17110

use std::time::Duration;

use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_wrpc_client::client::ConnectOptions;
use kaspa_wrpc_client::prelude::*;
use kaspa_wrpc_client::{KaspaRpcClient, WrpcEncoding};

#[tokio::main]
async fn main() {
    // No URL → use the built-in Resolver (public Kaspa community wRPC nodes).
    let arg = std::env::args().nth(1);
    let url_opt: Option<&str> = arg.as_deref();
    let network_id = NetworkId::new(NetworkType::Mainnet);
    let resolver = if url_opt.is_none() { Some(Resolver::default()) } else { None };
    let client =
        KaspaRpcClient::new_with_args(WrpcEncoding::Borsh, url_opt, resolver, Some(network_id), None)
            .expect("create client");
    client
        .connect(Some(ConnectOptions {
            block_async_connect: true,
            strategy: ConnectStrategy::Fallback,
            url: None,
            connect_timeout: Some(Duration::from_secs(15)),
            retry_interval: None,
        }))
        .await
        .expect("connect");
    println!("connected (url={:?}, resolved={:?})", url_opt, client.url());

    let dag_info = client.get_block_dag_info().await.expect("get_block_dag_info");
    println!("\n=== getBlockDagInfo ===");
    println!("block_count       = {}", dag_info.block_count);
    println!("header_count      = {}", dag_info.header_count);
    println!("virtual_daa_score = {}", dag_info.virtual_daa_score);
    println!("pruning_point     = {}", dag_info.pruning_point_hash);
    println!("tip_hashes[0]     = {:?}", dag_info.tip_hashes.first());

    let low = dag_info.pruning_point_hash;

    println!("\n=== get_blocks(low_hash=pruning_point, include_blocks=true, include_txs=false) ===");
    let resp = client.get_blocks(Some(low), true, false).await.expect("get_blocks");
    println!("block_hashes.len() = {}", resp.block_hashes.len());
    println!("blocks.len()       = {}", resp.blocks.len());

    if let Some(b) = resp.blocks.first() {
        println!("\nfirst RpcBlock:");
        println!("  header.hash = {:?}", b.header.hash);
        println!("  header.parents_by_level.len() = {}", b.header.parents_by_level.len());
        println!("  transactions.len() = {}", b.transactions.len());
        if let Some(vd) = b.verbose_data.as_ref() {
            println!("  verbose_data:");
            println!("    hash                        = {:?}", vd.hash);
            println!("    selected_parent_hash        = {:?}", vd.selected_parent_hash);
            println!("    transaction_ids.len()       = {}", vd.transaction_ids.len());
            println!("    is_header_only              = {:?}", vd.is_header_only);
            println!("    blue_score                  = {:?}", vd.blue_score);
            println!("    children_hashes.len()       = {}", vd.children_hashes.len());
            println!("    merge_set_blues_hashes.len()= {}", vd.merge_set_blues_hashes.len());
            println!("    merge_set_reds_hashes.len() = {}", vd.merge_set_reds_hashes.len());
            println!("    is_chain_block              = {:?}", vd.is_chain_block);
        } else {
            println!("  verbose_data = None");
        }
    }

    if let Some(&h) = resp.block_hashes.iter().nth(100) {
        println!("\n=== get_block(h=block_hashes[100], include_txs=false) ===");
        let blk = client.get_block(h, false).await.expect("get_block");
        println!("header.hash = {:?}", blk.header.hash);
        if let Some(vd) = blk.verbose_data.as_ref() {
            println!("merge_set_blues_hashes.len() = {}", vd.merge_set_blues_hashes.len());
            println!("merge_set_reds_hashes.len()  = {}", vd.merge_set_reds_hashes.len());
            println!("is_chain_block               = {:?}", vd.is_chain_block);
            println!("selected_parent_hash         = {:?}", vd.selected_parent_hash);
        }
    }
}
