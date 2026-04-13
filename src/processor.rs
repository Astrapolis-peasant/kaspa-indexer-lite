use std::collections::HashSet;

use kaspa_hashes::Hash;
use kaspa_rpc_core::RpcChainBlockAcceptedTransactions;

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
            signature_script:         input.signature_script.clone(),
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

fn compress_subnetwork(bytes: &[u8; 20]) -> Option<Vec<u8>> {
    let len = bytes.iter().rposition(|&b| b != 0).map(|i| i + 1).unwrap_or(0);
    if len == 0 { None } else { Some(bytes[..len].to_vec()) }
}

pub fn build_index_batches(chain: &[RpcChainBlockAcceptedTransactions], page_size: usize) -> Vec<IndexBatch> {
    let mut batches = vec![];
    let mut seen_transactions:  HashSet<Hash> = HashSet::new();
    let mut seen_parents:       HashSet<(Hash, Hash)> = HashSet::new();
    let mut seen_addr_tx:       HashSet<(String, Hash)> = HashSet::new();

    for chunk in chain.chunks(page_size) {
        let mut blocks:                  Vec<BlockRow>                = vec![];
        let mut block_parents:           Vec<BlockParentRow>          = vec![];
        let mut transactions:            Vec<TransactionRow>          = vec![];
        let mut address_transactions:    Vec<AddressTransactionRow>   = vec![];

        for entry in chunk {
            let header     = &entry.chain_block_header;
            let block_hash = header.hash.expect("chain_block_header.hash");

            // ── Block ────────────────────────────────────────────────────────
            blocks.push(BlockRow {
                hash:                    hash_to_bytes(block_hash),
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

            // ── Block parents (level 0) ──────────────────────────────────────
            if let Some(level0) = header.parents_by_level.as_ref().and_then(|pbl| pbl.get(0)) {
                for &parent_hash in level0 {
                    if seen_parents.insert((block_hash, parent_hash)) {
                        block_parents.push(BlockParentRow {
                            block_hash:  hash_to_bytes(block_hash),
                            parent_hash: hash_to_bytes(parent_hash),
                        });
                    }
                }
            }

            // ── Transactions ─────────────────────────────────────────────────
            for rpc_tx in &entry.accepted_transactions {
                let verbose_data   = rpc_tx.verbose_data.as_ref().expect("missing verbose_data");
                let transaction_id = verbose_data.transaction_id.expect("missing transaction_id");
                let block_time     = verbose_data.block_time.expect("missing block_time") as i64;
                let included_in    = verbose_data.block_hash.expect("missing block_hash");

                if seen_transactions.insert(transaction_id) {
                    transactions.push(TransactionRow {
                        transaction_id: hash_to_bytes(transaction_id),
                        subnetwork_id:  rpc_tx.subnetwork_id.as_ref().and_then(|s| compress_subnetwork(s.as_ref())),
                        hash:           verbose_data.hash.map(hash_to_bytes),
                        mass:           verbose_data.compute_mass.and_then(|m| (m != 0).then_some(m as i32)),
                        payload:        rpc_tx.payload.as_ref().filter(|p| !p.is_empty()).map(|p| p.to_vec()),
                        block_time:     Some(block_time),
                        version:        rpc_tx.version.and_then(|v| (v != 0).then_some(v as i16)),
                        block_hash:     Some(hash_to_bytes(included_in)),
                        accepted_by:    hash_to_bytes(block_hash),
                        inputs:         map_inputs(rpc_tx),
                        outputs:        map_outputs(rpc_tx),
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
            blocks,
            block_parents,
            transactions,
            address_transactions,
        });
    }

    batches
}
