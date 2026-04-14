# kaspa-indexer-lite

A lightweight Kaspa blockchain indexer that uses the Virtual Chain (VCP) RPC endpoint with borsh encoding to index transactions, blocks, and address mappings into PostgreSQL.

## Architecture

```
kaspad (borsh wRPC :17110)
    │
    ├── VCP fetcher ─▶ mpsc(5) ─▶ VCP consumer ─▶ blocks (is_chain_block=true) + transactions
    └── DAG fetcher ─▶ mpsc(5) ─▶ DAG consumer ─▶ blocks (all DAG blocks, headers only)
```

- **VCP fetcher** polls `getVirtualChainFromBlockV2` with full verbosity for chain blocks + tx bodies.
- **DAG fetcher** polls `getBlocks` (headers + verbose_data, no tx bodies) for every DAG block.
- Both write to the unified `blocks` table via upsert; `is_chain_block` gets OR-merged between sources.
- Backpressure: 5-response channel per side (~1,750 chain blocks or ~5,000 DAG blocks buffered).
- Automatic reconnection on kaspad disconnect.

## What gets indexed

| Table | Description |
|---|---|
| `blocks` | Every DAG block kaspad returns; `is_chain_block` flags those on the virtual chain |
| `transactions` | Full transaction data with inputs/outputs as composite type arrays |
| `addresses_transactions` | Address to transaction lookup (deduped in Rust) |

Each block row stores:
- `hash` (PK), `is_chain_block`, `selected_parent` (GHOSTDAG; for chain blocks = previous chain block)
- `parents` — level-0 DAG parents
- `tx_ids` — transaction ids included in this block
- header fields: `timestamp`, `blue_score`, `daa_score`, `bits`, `nonce`, merkle roots, etc.

Each transaction stores:
- `block_hash` — the DAG block that included the transaction
- `accepted_by` — the chain block that accepted/confirmed it
- `inputs` / `outputs` — composite type arrays

Virtual chain queries: filter `WHERE is_chain_block` (partial index keeps this fast).

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
```

## Reorg handling

On chain reorganization, the VCP response includes `removed_chain_block_hashes`. The indexer:

1. Sets `accepted_by = NULL` on transactions accepted by removed blocks.
2. Flips `is_chain_block = false` on the removed blocks. Blocks stay in the table (they're still DAG blocks); only their chain-status changes.

The same VCP response's `added_chain_block_hashes` re-flip the new chain via upsert, and txs are re-linked.

Deep reorgs beyond the pruning window surface as a VCP error and require a manual reset (truncate the indexer DB and re-sync from pruning point).

## Performance

Tested on WSL2 (12 cores, 48GB RAM) with PostgreSQL 17:

| Batch type | Transactions | Time |
|---|---|---|
| Normal (100 blocks) | ~3-5k tx | 0.07-0.14s |
| Heavy (near pruning point) | ~100-120k tx | 2-3s |

Database growth: ~6 GB/day with composite type arrays (vs ~50 GB/day with JSONB).

## Schema

See [schema.sql](schema.sql) for the full database schema.
