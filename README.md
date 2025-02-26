# PumpFun Sniper Bot (Rust Version)

A high-performance Rust implementation of the PumpFun sniper bot for Solana tokens. This bot monitors and automatically buys new tokens on PumpFun with configurable filters and strategies.

## Features

- **High Performance**: Optimized Rust implementation for speed and efficiency
- **Token Filtering**: Filter tokens by tag, URL presence, minimum liquidity, and approved developers
- **Automated Trading**: Configurable take-profit, stop-loss, and timeout parameters
- **Persistent Storage**: Tracks all trades and their state in an SQLite database
- **Concurrent Processing**: Simultaneously monitors new tokens and manages existing positions

## Configuration

The bot uses a `.env` file for configuration. Here's an explanation of the available options:

```
PRIVATE_KEY=your_solana_private_key
WALLET=your_solana_wallet_address
AMOUNT=0.1                        # Amount of SOL to spend per trade
SLIPPAGE=50                       # Slippage tolerance in percentage
TAKE_PROFIT=60                    # Take profit percentage
STOP_LOSS=10                      # Stop loss percentage
TIMEOUT=10                        # Maximum position hold time in minutes
CHECK_URLS=false                  # Whether to check for valid URLs
SNIPE_BY_TAG=                     # Only buy tokens with this string in the name
MAX_POSITIONS=1                   # Maximum number of open positions
CHECK_MIN_LIQUIDITY=true          # Whether to check for minimum liquidity
MIN_LIQUIDITY=3                   # Minimum liquidity in SOL
APPROVED_DEVS_ONLY=false          # Whether to only buy from approved developers
```

## Usage

Build and run the bot in release mode for optimal performance:

```bash
cargo build --release
./target/release/pumpfun-sniper-rust
```

## Monitoring

The bot logs all activity to the console, including:
- New token detections
- Buy and sell actions
- Price monitoring
- Error messages

## Requirements

- Rust 1.70 or higher
- SQLite
- Internet connection

## License

MIT 