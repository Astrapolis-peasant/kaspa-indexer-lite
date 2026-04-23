# kaspa-indexer-lite

A lightweight Kaspa blockchain indexer that uses the Virtual Chain (VCP) RPC endpoint with borsh encoding to index transactions, blocks, and address mappings into PostgreSQL.

## Architecture

```
kaspad (borsh wRPC :17110)
    │
    ├── VCP fetcher ─▶ mpsc(5) ─▶ VCP consumer ─▶ chain_blocks + transactions + addresses_transactions
    └── DAG fetcher ─▶ mpsc(5) ─▶ DAG consumer ─▶ dag_blocks (headers only)
```

- **VCP fetcher** polls `getVirtualChainFromBlockV2` with full verbosity for chain blocks + tx bodies.
- **DAG fetcher** polls `getBlocks` (headers + verbose_data, no tx bodies) for every DAG block.
- **Pruner** runs hourly, deleting expired `dag_blocks` and spam transactions older than 7 days. Chain blocks and non-spam transactions are kept forever.
- Backpressure: 5-response channel per side (~1,750 chain blocks or ~5,000 DAG blocks buffered).
- Automatic reconnection on kaspad disconnect.

## What gets indexed

| Table | Writer | Description |
|---|---|---|
| `dag_blocks` | DAG consumer | Every DAG block kaspad returns. Full header + `selected_parent`, `parents`, `tx_count`. Pruned after 7 days. |
| `chain_blocks` | VCP consumer | Blocks on the virtual chain with full headers. `tx_count`, `selected_parent` (= previous chain block). Kept forever. |
| `transactions` | VCP consumer | Tx data with inputs/outputs as composite type arrays. Includes `is_spam` flag. Spam txs pruned with their dag_blocks. |
| `addresses_transactions` | VCP consumer | Address → tx lookup (Rust-side deduped). Spam entries pruned with their transactions. |

Each table has a single writer — no cross-writer lock contention, no deadlocks.

Each transaction stores:
- `block_hash` — the DAG block that included the transaction
- `accepted_by` — the chain block that accepted/confirmed it
- `is_spam` — detected at insert time (1-in/1-out, mass=1624, fee=2036, <1 KAS output)
- `inputs` / `outputs` — composite type arrays (not JSONB)

## Spam detection

Transactions are flagged as spam at insert time based on:
- Not a coinbase transaction
- Exactly 1 input and 1 output
- Compute mass = 1624
- Output amount < 1 KAS (100,000,000 sompi)
- Fee = 2036 sompi (input - output)

Spam transactions are pruned along with their expired dag_blocks by the built-in pruner.

## Pruner

The pruner runs as a background task (every hour) and deletes:
1. `dag_blocks` with `timestamp` older than 7 days (in 1000-row batches)
2. Spam transactions (`is_spam = true`) whose `block_hash` is in the expired dag_blocks
3. Corresponding `addresses_transactions` entries

Chain blocks and non-spam transactions are never pruned.

## Requirements

- Rust 1.93.1+
- PostgreSQL 14+ (17 recommended)
- kaspad v1.1.0+ with `--rpclisten-borsh` enabled

## Setup

### Database

```bash
createdb kaspa_indexer_lite
psql -U postgres -h localhost kaspa_indexer_lite -f schema.sql
```

### PostgreSQL tuning (recommended for bulk indexing)

```sql
ALTER SYSTEM SET shared_buffers = '4GB';
ALTER SYSTEM SET work_mem = '256MB';
ALTER SYSTEM SET maintenance_work_mem = '2GB';
ALTER SYSTEM SET effective_cache_size = '36GB';
ALTER SYSTEM SET max_wal_size = '8GB';
ALTER SYSTEM SET synchronous_commit = off;
ALTER SYSTEM SET fsync = off;           -- only during initial sync, re-enable after
ALTER SYSTEM SET full_page_writes = off; -- only during initial sync
```

### kaspad

```bash
kaspad --utxoindex --rpclisten-borsh --rpclisten-json --ram-scale=1.0
```

### Build and run

```bash
cargo build --release
./target/release/kaspa-indexer-lite -d postgresql://postgres:postgres@localhost/kaspa_indexer_lite
```

## CLI options

```
-w, --kaspad-ws <URL>          kaspad borsh wRPC URL [default: ws://localhost:17110]
-d, --database-url <URL>       PostgreSQL connection string (required)
-p, --page-size <N>            Blocks per DB transaction [default: 100]
-c, --min-confirmations <N>    Safety buffer behind tip [default: 10]
-i, --poll-interval-ms <MS>    Poll interval when caught up [default: 1000]
-s, --start-hash <HASH>        Override starting block hash
    --db-pool-size <N>         DB connection pool size [default: 4]
    --prune <BOOL>             Enable pruning [default: true]
```

## Reorg handling

On chain reorganization, the VCP response includes `removed_chain_block_hashes`. The VCP consumer:

1. Sets `accepted_by = NULL` on transactions accepted by removed blocks.
2. `DELETE`s the removed hashes from `chain_blocks`.

`dag_blocks` is untouched — the reorged blocks are still valid DAG blocks, they're just not on the virtual chain anymore.

The same VCP response's `added_chain_block_hashes` insert the new chain into `chain_blocks`; txs are re-linked via the transactions UPSERT (`ON CONFLICT DO UPDATE SET accepted_by, block_hash, block_time, is_spam = EXCLUDED.*`).

Deep reorgs beyond the pruning window surface as a VCP error and require a manual reset (truncate the indexer DB and re-sync from pruning point).

## Performance

Tested on WSL2 (12 cores, 48GB RAM) with PostgreSQL 17:

| Batch type | Transactions | Time |
|---|---|---|
| Normal (100 blocks) | ~3-5k tx | 0.07-0.14s |
| Heavy (near pruning point) | ~100-120k tx | 2-3s |

Database growth: ~10 GB/day including indexes at ~78 TPS sustained.

## Schema

See [schema.sql](schema.sql) for the full database schema.
