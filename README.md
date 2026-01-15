# Agentic Memecoin Trading Bot

High-performance, fully automated memecoin trading system written in Rust. The bot monitors on-chain liquidity, evaluates rug-pull and honeypot risks via GoPlus, scores momentum using live DexScreener data, and executes trades through Uniswap-compatible routers using `ethers-rs`. A built-in Axum monitoring API exposes live health and portfolio telemetry.

> **Warning:** This software signs and broadcasts live trades. Only run with wallets you control, review the source carefully, and understand the financial risks. Never expose your private key in production logs or version control.

## Features

- **Live market intelligence:** DexScreener trending and token endpoints for fresh liquidity discovery, momentum scoring, and volume/liquidity filtering.
- **Rug-pull detection:** GoPlus security feed checks honeypots, ownership controls, tax levels, holder concentration, and liquidity locks.
- **Automated execution:** Gas-capped swaps on UniswapV2-compatible routers with slippage, deadline, and allowance management.
- **Stateful portfolio management:** JSON-backed persistent ledger with take-profit / stop-loss exit planning and dynamic PnL.
- **Operator API:** Axum HTTP server (`/health`, `/portfolio`) for monitoring and integration.
- **Config via environment:** Complete runtime control with `.env` or environment variables.

## Prerequisites

- Rust 1.72+ (`rustup` recommended)
- Installed `wasm32` target not required; builds natively.
- An Ethereum RPC endpoint (Infura, Alchemy, self-hosted, etc.)
- A funded EOA private key with ETH for gas (DO NOT use your cold wallet).

## Quick Start

```bash
cp .env.example .env
# edit .env with your RPC URL, router, and private key

cargo build --release

# dry-run single discovery + execution cycle
TRADING_PRIVATE_KEY=0x... cargo run -- run --once
```

### Long-Running Daemon

```bash
RUST_LOG=info,axum=warn cargo run -- run
```

The bot will:

1. Refresh current positions from the blockchain
2. Discover new memecoins with sufficient liquidity/momentum
3. Evaluate GoPlus risk heuristics
4. Enter positions up to `MAX_POSITIONS`
5. Persist portfolio state to `portfolio_state.json`
6. Serve monitoring endpoints on `MONITOR_ADDR`

### CLI Utilities

```bash
# print current opportunities without trading
cargo run -- scan

# evaluate a specific token
cargo run -- evaluate 0xTokenAddress

# RPC health check
cargo run -- health
```

## Environment Variables

All configuration is driven by env vars (see `.env.example`):

| Variable | Description |
|----------|-------------|
| `CHAIN_ID` | EVM chain id (1 = Ethereum mainnet) |
| `RPC_HTTP` | HTTPS RPC endpoint |
| `ROUTER_ADDRESS` | Uniswap V2 router contract used for swaps |
| `TRADING_PRIVATE_KEY` | Hex private key for executing trades |
| `POSITION_SIZE_ETH` | Amount of native coin per entry |
| `MAX_POSITIONS` | Simultaneous open positions |
| `MAX_SLIPPAGE_BPS` | Slippage limit in basis points |
| `TAKE_PROFIT_BPS`/`STOP_LOSS_BPS` | Exit targets in basis points |
| `MIN_LIQUIDITY_USD`, `MIN_DAILY_VOLUME_USD`, `MIN_TOKEN_AGE_MINUTES` | Candidate filters |
| `MAX_TOP_HOLDER_PERCENT`, `MIN_LOCK_RATIO_PERCENT` | Risk heuristics |
| `MONITOR_ADDR` | Axum HTTP bind address for metrics |

## Monitoring API

- `GET /health` – latest block sync state
- `GET /portfolio` – JSON snapshot of active positions, valuations, and PnL

Example:

```bash
curl http://localhost:8787/portfolio | jq
```

## Architecture Overview

- `config.rs` – env-driven configuration loader with validation and typed accessors.
- `engine/scanner.rs` – DexScreener integration, candidate discovery, trend validation.
- `engine/risk.rs` – GoPlus security analysis, heuristic scoring.
- `engine/trader.rs` – ethers-based execution engine, gas management, allowance control.
- `engine/portfolio.rs` – persistence, position tracking, exit order generation.
- `api.rs` – Axum monitoring service.
- `main.rs` – CLI entrypoint and orchestration.

## Security Notes

- The private key is loaded from `TRADING_PRIVATE_KEY` at startup. Protect the environment variables, shell history, and process.
- Consider using a dedicated RPC provider with rate limits and WSS streaming for latency-sensitive trading.
- The bot assumes access to a UniswapV2-compatible router. Adjust ABI or module for alternative DEXs.
- Always review the code paths touching funds, especially before deploying to production infrastructure.

## License

MIT © 2025 – generated and curated for high-performance automated trading pipelines.
