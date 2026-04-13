# kaspa-indexer-lite

A lightweight Kaspa blockchain indexer that uses the Virtual Chain (VCP) RPC endpoint with borsh encoding to index transactions, blocks, and address mappings into PostgreSQL.

## Architecture

```
kaspad (borsh wRPC :17110)
    │
    ▼
┌──────────┐     mpsc(5)     ┌──────────┐
│ Fetcher  │────────────────▶│ Consumer │────▶ PostgreSQL
│          │  VCP responses  │          │
└──────────┘                 └──────────┘
```

- **Fetcher** polls `getVirtualChainFromBlockV2` with full verbosity, streams responses into a bounded channel
- **Consumer** slices each response into page-sized batches and commits to PostgreSQL in transactions
- Backpressure: channel capacity of 5 responses (~1,750 blocks buffered)
- Automatic reconnection on kaspad disconnect

## What gets indexed

| Table | Description |
|---|---|
| `blocks` | Chain block headers (selected parent chain only) |
| `block_parent` | DAG parent relationships (level 0) |
| `transactions` | Full transaction data with inputs/outputs as composite type arrays |
| `addresses_transactions` | Address to transaction lookup (deduped in Rust) |

Each transaction stores:
- `block_hash` — the DAG block that included the transaction
- `accepted_by` — the chain block that accepted/confirmed it (FK to `blocks`)
- `inputs` — composite type array with previous outpoint, signature script, UTXO script/amount
- `outputs` — composite type array with amount, script public key, address

## Requirements

- Rust 1.93.1+
- PostgreSQL 14+ (17 recommended)
- kaspad v1.1.0+ with `--rpclisten-borsh` enabled

## Setup

### Database

```bash
createdb kaspa_py
psql -U postgres -h localhost kaspa_py -f schema.sql
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
./target/release/kaspa-indexer-lite -d postgresql://postgres:postgres@localhost/kaspa_py
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

On chain reorganization, the VCP response includes removed chain block hashes. The indexer sets `accepted_by = NULL` on transactions that were accepted by removed blocks. The new chain blocks in the same response re-set `accepted_by` for re-accepted transactions via `ON CONFLICT DO UPDATE`.

## Performance

Tested on WSL2 (12 cores, 48GB RAM) with PostgreSQL 17:

| Batch type | Transactions | Time |
|---|---|---|
| Normal (100 blocks) | ~3-5k tx | 0.07-0.14s |
| Heavy (near pruning point) | ~100-120k tx | 2-3s |

Database growth: ~6 GB/day with composite type arrays (vs ~50 GB/day with JSONB).

## Schema

See [schema.sql](schema.sql) for the full database schema.
