use std::collections::HashSet;

use kaspa_hashes::Hash;
use kaspa_rpc_core::{RpcBlock, RpcChainBlockAcceptedTransactions};

use crate::models::*;

fn map_inputs(rpc_tx: &kaspa_rpc_core::RpcOptionalTransaction) -> Option<Vec<TransactionInput>> {
    if rpc_tx.inputs.is_empty() {
        return None;
    }
    Some(rpc_tx.inputs.iter().enumerate().map(|(i, input)| {
        let outpoint = input.previous_outpoint.as_ref();
        let utxo = input.verbose_data.as_ref().and_then(|vd| vd.utxo_entry.as_ref());
        TransactionInput {
            index:                    i as i16,
            previous_outpoint_hash:   outpoint.and_then(|o| o.transaction_id.map(|h| h.as_bytes().to_vec())),
            previous_outpoint_index:  outpoint.and_then(|o| o.index.map(|v| v as i16)),
            sig_op_count:             input.sig_op_count.map(|v| v as i16),
            previous_outpoint_script: utxo.and_then(|u| u.script_public_key.as_ref().map(|spk| spk.script().to_vec())),
            previous_outpoint_amount: utxo.and_then(|u| u.amount.map(|v| v as i64)),
        }
    }).collect())
}

fn map_outputs(rpc_tx: &kaspa_rpc_core::RpcOptionalTransaction) -> Option<Vec<TransactionOutput>> {
    if rpc_tx.outputs.is_empty() {
        return None;
    }
    Some(rpc_tx.outputs.iter().enumerate().map(|(i, output)| {
        TransactionOutput {
            index:                     i as i16,
            amount:                    output.value.map(|v| v as i64),
            script_public_key:         output.script_public_key.as_ref().map(|spk| spk.script().to_vec()),
            script_public_key_address: output.verbose_data.as_ref()
                .and_then(|vd| vd.script_public_key_address.as_ref())
                .map(|a| a.payload_to_string()),
        }
    }).collect())
}

pub fn hash_to_bytes(h: Hash) -> Vec<u8> {
    h.as_bytes().to_vec()
}

fn is_likely_spam(
    subnetwork_id: &Option<Vec<u8>>,
    mass: Option<i32>,
    inputs: &Option<Vec<TransactionInput>>,
    outputs: &Option<Vec<TransactionOutput>>,
) -> bool {
    let is_coinbase = subnetwork_id.as_deref() == Some(&[0x01]);
    if is_coinbase { return false; }
    let ins = match inputs { Some(v) if v.len() == 1 => v, _ => return false };
    let outs = match outputs { Some(v) if v.len() == 1 => v, _ => return false };
    if mass != Some(1624) { return false; }
    let in_amount = ins[0].previous_outpoint_amount.unwrap_or(0);
    let out_amount = outs[0].amount.unwrap_or(0);
    out_amount < 100_000_000 && in_amount - out_amount == 2036
}

fn compress_subnetwork(bytes: &[u8; 20]) -> Option<Vec<u8>> {
    let len = bytes.iter().rposition(|&b| b != 0).map(|i| i + 1).unwrap_or(0);
    if len == 0 { None } else { Some(bytes[..len].to_vec()) }
}

pub fn build_index_batches(
    chain: &[RpcChainBlockAcceptedTransactions],
    page_size: usize,
    mut prev_hash: Option<Hash>,
) -> Vec<IndexBatch> {
    let mut batches = vec![];
    let mut seen_transactions:  HashSet<Hash> = HashSet::new();
    let mut seen_addr_tx:       HashSet<(String, Hash)> = HashSet::new();

    for chunk in chain.chunks(page_size) {
        let mut chain_blocks:            Vec<ChainBlockRow>           = vec![];
        let mut transactions:            Vec<TransactionRow>          = vec![];
        let mut address_transactions:    Vec<AddressTransactionRow>   = vec![];

        for entry in chunk {
            let header     = &entry.chain_block_header;
            let block_hash = header.hash.expect("chain_block_header.hash");

            // selected_parent on the virtual chain = previous chain block
            // in VCP's added_chain_block_hashes order.
            let selected_parent = prev_hash.map(hash_to_bytes);
            prev_hash = Some(block_hash);

            // Level-0 DAG parents (first level of parents_by_level).
            let parents = header.parents_by_level.as_ref()
                .and_then(|pbl| pbl.get(0))
                .map(|level0| level0.iter().map(|h| hash_to_bytes(*h)).collect::<Vec<_>>());

            chain_blocks.push(ChainBlockRow {
                hash:                    hash_to_bytes(block_hash),
                selected_parent,
                parents,
                tx_count:                entry.accepted_transactions.len() as i16,
                accepted_id_merkle_root: header.accepted_id_merkle_root.map(hash_to_bytes),
                bits:                    header.bits.map(|v| v as i64),
                blue_score:              header.blue_score.map(|v| v as i64),
                blue_work:               header.blue_work.map(|bw| bw.to_be_bytes_var()),
                daa_score:               header.daa_score.map(|v| v as i64),
                hash_merkle_root:        header.hash_merkle_root.map(hash_to_bytes),
                nonce:                   header.nonce.map(|n| n.to_be_bytes().to_vec()),
                pruning_point:           header.pruning_point.map(hash_to_bytes),
                timestamp:               header.timestamp.map(|v| v as i64),
                utxo_commitment:         header.utxo_commitment.map(hash_to_bytes),
                version:                 header.version.map(|v| v as i16),
            });

            // ── Transactions ─────────────────────────────────────────────────
            for rpc_tx in &entry.accepted_transactions {
                let verbose_data   = rpc_tx.verbose_data.as_ref().expect("missing verbose_data");
                let transaction_id = verbose_data.transaction_id.expect("missing transaction_id");
                let block_time     = verbose_data.block_time.expect("missing block_time") as i64;
                let included_in    = verbose_data.block_hash.expect("missing block_hash");

                if seen_transactions.insert(transaction_id) {
                    let subnetwork_id = rpc_tx.subnetwork_id.as_ref().and_then(|s| compress_subnetwork(s.as_ref()));
                    let mass = verbose_data.compute_mass.and_then(|m| (m != 0).then_some(m as i32));
                    let inputs = map_inputs(rpc_tx);
                    let outputs = map_outputs(rpc_tx);
                    let is_spam = is_likely_spam(&subnetwork_id, mass, &inputs, &outputs);
                    transactions.push(TransactionRow {
                        transaction_id: hash_to_bytes(transaction_id),
                        subnetwork_id,
                        hash:           verbose_data.hash.map(hash_to_bytes),
                        mass,
                        payload:        rpc_tx.payload.as_ref().filter(|p| !p.is_empty()).map(|p| p.to_vec()),
                        block_time:     Some(block_time),
                        version:        rpc_tx.version.and_then(|v| (v != 0).then_some(v as i16)),
                        block_hash:     Some(hash_to_bytes(included_in)),
                        accepted_by:    hash_to_bytes(block_hash),
                        is_spam,
                        inputs,
                        outputs,
                    });

                    for output in &rpc_tx.outputs {
                        if let Some(address) = output
                            .verbose_data.as_ref()
                            .and_then(|vd| vd.script_public_key_address.as_ref())
                            .map(|a| a.payload_to_string())
                        {
                            if seen_addr_tx.insert((address.clone(), transaction_id)) {
                                address_transactions.push(AddressTransactionRow {
                                    address,
                                    transaction_id: hash_to_bytes(transaction_id),
                                    block_time,
                                });
                            }
                        }
                    }

                    for input in &rpc_tx.inputs {
                        if let Some(address) = input
                            .verbose_data.as_ref()
                            .and_then(|ivd| ivd.utxo_entry.as_ref())
                            .and_then(|utxo| utxo.verbose_data.as_ref())
                            .and_then(|uvd| uvd.script_public_key_address.as_ref())
                            .map(|a| a.payload_to_string())
                        {
                            if seen_addr_tx.insert((address.clone(), transaction_id)) {
                                address_transactions.push(AddressTransactionRow {
                                    address,
                                    transaction_id: hash_to_bytes(transaction_id),
                                    block_time,
                                });
                            }
                        }
                    }
                }
            }
        }

        batches.push(IndexBatch {
            chain_blocks,
            transactions,
            address_transactions,
        });
    }

    batches
}

/// Turn `get_blocks` response entries into DagBlockRow. `RpcBlock` (from
/// `get_blocks`) uses direct (non-Option) header fields, unlike VCP's
/// `RpcOptionalHeader`.
pub fn build_dag_block_rows(blocks: &[RpcBlock]) -> Vec<DagBlockRow> {
    blocks.iter().map(|b| {
        let header = &b.header;
        let vd     = b.verbose_data.as_ref();

        let parents: Option<Vec<Vec<u8>>> = header.parents_by_level
            .get(0)
            .map(|level0| level0.iter().map(|h| hash_to_bytes(*h)).collect());

        let selected_parent = vd.map(|v| hash_to_bytes(v.selected_parent_hash));
        let tx_count = vd.map(|v| v.transaction_ids.len() as i16);

        DagBlockRow {
            hash:                    hash_to_bytes(header.hash),
            selected_parent,
            parents,
            tx_count,
            accepted_id_merkle_root: Some(hash_to_bytes(header.accepted_id_merkle_root)),
            bits:                    Some(header.bits as i64),
            blue_score:              Some(header.blue_score as i64),
            blue_work:               Some(header.blue_work.to_be_bytes_var()),
            daa_score:               Some(header.daa_score as i64),
            hash_merkle_root:        Some(hash_to_bytes(header.hash_merkle_root)),
            nonce:                   Some(header.nonce.to_be_bytes().to_vec()),
            pruning_point:           Some(hash_to_bytes(header.pruning_point)),
            timestamp:               Some(header.timestamp as i64),
            utxo_commitment:         Some(hash_to_bytes(header.utxo_commitment)),
            version:                 Some(header.version as i16),
        }
    }).collect()
}
