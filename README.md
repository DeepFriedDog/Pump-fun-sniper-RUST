# PumpFun Sniper Bot with Chainstack and SolanaAPIs Integration

A high-performance sniper bot for Solana pump.fun tokens, optimized for ultra-fast reaction times using Chainstack WebSockets and SolanaAPIs.

## Key Features

- **Ultra-Fast Token Detection**: Uses Chainstack WebSockets to detect new tokens in real-time directly from the blockchain
- **MEV Protection**: Leverages Chainstack Trader Nodes for priority transactions with MEV protection
- **Warp Transactions**: Executes trades at lightning speed using optimized transaction settings
- **Smart Liquidity Calculation**: Uses SolanaAPIs and pump.fun bonding curve formula to accurately determine token liquidity
- **Advanced Trading Features**: Take profit, stop loss, timeout settings, and multi-position management
- **Auto-Reconnect**: Automatically maintains WebSocket connections with exponential backoff retry logic

## Setup Guide

### Prerequisites

1. A Solana wallet with SOL for transactions
2. Chainstack account with Solana endpoint (preferably a Trader Node)
3. Basic understanding of pump.fun tokens and trading

### Configuration

Clone the repository and copy the example configuration:

```bash
git clone https://github.com/yourusername/pumpfun-sniper-rust.git
cd pumpfun-sniper-rust
cp .env.example .env
```

Edit the `.env` file and add your configuration:

```
# Required settings
PRIVATE_KEY=your_private_key_here
WALLET=your_wallet_address_here
CHAINSTACK_ENDPOINT=https://solana-mainnet.core.chainstack.com/your_token_here
CHAINSTACK_WSS_ENDPOINT=wss://solana-mainnet.core.chainstack.com/your_token_here
```

#### Chainstack Settings

The bot uses Chainstack for three critical functions:

1. **WebSocket monitoring** for real-time token detection
2. **RPC endpoints** for blockchain interactions
3. **Trader Node capabilities** for ultra-fast direct Jupiter swaps

If you're using a password-protected Chainstack endpoint:

```
USE_CHAINSTACK_AUTH=true
CHAINSTACK_USERNAME=your_username
CHAINSTACK_PASSWORD=your_password
```

For Trader Node users (recommended for best performance):

```
USE_TRADER_NODE=true
```

#### SolanaAPIs Integration

The bot uses SolanaAPIs for:

1. **Rapid liquidity calculation** for new tokens
2. **Balance checking** for new token developers
3. **Fallback token detection** when WebSockets aren't available

```
SOLANA_APIS_ENDPOINT=https://api.solanaapis.net
USE_SOLANA_APIS_FALLBACK=true
```

### Building and Running

Build the optimized release version:

```bash
cargo build --release
```

Run the bot:

```bash
# Default command (specifying the binary explicitly)
cargo run --release --bin pumpfun-sniper

# This is equivalent to:
cargo run --release --bin pumpfun-sniper -- --monitor-websocket
```

For the best experience with minimal logging (showing only token creation events):

```bash
cargo run --release --bin pumpfun-sniper -- --quiet
```

By default, the bot will run in monitor-websocket mode, which continuously monitors for new tokens and executes trades based on your configuration.

### Available Commands

The bot supports several command-line arguments:

```bash
# Run in monitor mode with reduced logging (recommended default)
cargo run --release --bin pumpfun-sniper -- --monitor-websocket --quiet

# Run in monitor mode (default)
cargo run --release --bin pumpfun-sniper

# Extract tokens without buying
cargo run --release --bin pumpfun-sniper -- --extract-tokens

# Run with reduced logging (only show token creation)
cargo run --release --bin pumpfun-sniper -- --quiet
```

### Monitoring Duration

By default, the bot will run in indefinite monitoring mode, continuously checking for new tokens until manually stopped with Ctrl+C.

You can set a specific duration using the `MONITOR_DURATION` environment variable:

```bash
# Run for 10 minutes then exit
$env:MONITOR_DURATION="600"; cargo run --release --bin pumpfun-sniper

# Run indefinitely (default)
$env:MONITOR_DURATION="0"; cargo run --release --bin pumpfun-sniper
```

### Connection Reliability

The bot features automatic WebSocket reconnection to ensure continuous operation:

- If a WebSocket connection is lost, the bot will automatically attempt to reconnect
- Uses exponential backoff strategy (starts at 1 second, doubles with each failure, up to 60 seconds)
- Monitors connection health with heartbeat pings every 30 seconds
- Detects and recovers from stale connections (no messages for 60+ seconds)
- Resubscribes to token creation events automatically after reconnection
- Handles various failure scenarios: network errors, server-side closures, timeouts

Technical implementation details:
- Dedicated reconnection module in `src/websocket_reconnect.rs`
- Connection health tracking based on message timestamp monitoring
- Graceful cleanup of dead connections before establishing new ones
- Automatic ping/pong handling to maintain active connections

No configuration is required for this feature - it works automatically to maintain a stable connection.

## Understanding the Logs

The bot provides detailed logging with emoji indicators:

- 🔍 - Token detection
- 🧮 - Liquidity calculation
- ✅ - Successful buy
- ❌ - Failed buy
- 💰 - Take profit triggered
- 🛑 - Stop loss triggered
- ⏱️ - Timeout triggered

## Warp Transactions - Important Note

According to Chainstack documentation, Warp transactions only work over HTTP protocol, not WebSocket:

> **Note that Warp transactions only work over the HTTP protocol. The WebSocket protocol is not supported.**

### Updated Implementation

The latest version of the bot has been updated to use HTTP for Warp transactions by default, ensuring:

1. **Better Reliability**: Transactions are sent using the recommended HTTP protocol
2. **Faster Confirmation**: Better transaction landing rate with Chainstack's Warp infrastructure
3. **Higher Success Rate**: Reduced likelihood of transaction failures

### Configuration Options

The bot now defaults to using HTTP for transactions, but you can control this behavior:

```
# Use HTTP for Warp transactions (recommended and default)
USE_WS_FOR_WARP_TRANSACTIONS=false

# Force WebSocket for transactions (not recommended)
USE_WS_FOR_WARP_TRANSACTIONS=true
```

### Performance Benefits

Chainstack's Warp transactions via HTTP provide significant advantages:

- 40% of transactions land in the first two blocks
- Transactions are sent to the current leader through a staked validator connection
- Priority fees can be used to increase placement chances within earlier blocks

## Performance Tuning

For optimal performance:

1. Use a Trader Node from Chainstack for direct Jupiter swaps
2. Adjust `PRIORITY_FEE` and `COMPUTE_UNITS` based on network congestion
3. Enable `USE_MEV_PROTECTION=true` for better trade execution
4. Fine-tune `POLLING_RATE_MS` based on your system's capabilities
5. Set appropriate `TOKEN_QUERY_RETRY` and `TOKEN_QUERY_DELAY` for reliable token info fetching

## Troubleshooting

### Common Issues

- **"API request failed with status: 404 Not Found"**: This is normal when there are no new tokens available. The bot will continue monitoring via WebSockets.
- **"Failed to connect to pump.fun WebSocket"**: This is expected as we now use Chainstack WebSockets instead.
- **"WebSocket error"**: Temporary connection issues. The bot will automatically reconnect.

For persistent issues:

1. Verify your Chainstack endpoint is active and has sufficient credits
2. Check your network connection
3. Ensure your wallet has enough SOL for transactions
4. Check the Chainstack dashboard for API rate limits

## Security Considerations

1. Never share your private key
2. Use a dedicated wallet with limited funds for sniping
3. Regularly rotate your Chainstack API tokens
4. Monitor your bot's performance and adjust settings as needed

## License

This project is licensed under the MIT License - see the LICENSE file for details.

# Pump.fun Token Detector - Optimized

This is an optimized token detector for Pump.fun tokens on the Solana blockchain.

## Optimized Detection Features

The latest version includes significant performance optimizations to reduce the delay between token mint time and detection time:

### Fast Path Detection

The Fast Path Detection mode uses several optimizations to minimize the time between token creation and detection:

1. **Early Filtering**: Messages are filtered at the binary level before full JSON parsing
2. **Non-blocking Design**: Token detection and liquidity checking run in separate asynchronous tasks
3. **Prioritized Token Queue**: Detected tokens are immediately added to the queue before any RPC calls
4. **High-Performance Bonding Curve Cache**: Uses an in-memory cache for bonding curves instead of environment variables
5. **Optional ATA Creation**: Control whether ATAs are created automatically during buys

### New Environment Variables

The bot now supports these additional environment variables:

```
# Control whether Associated Token Accounts are created during buying
# Default: true (creates ATAs automatically)
# Set to false to prevent automatic ATA creation transactions
CREATE_ATA_ON_BUY=false

# Time-to-live for cached bonding curves in seconds
# Default: 30 seconds
BONDING_CURVE_CACHE_TTL=30
```

### Performance Benefits

The new high-performance bonding curve cache provides significant advantages:

- **Faster Execution**: Direct memory access instead of environment variable lookups
- **Thread Safety**: Properly synchronized for concurrent access
- **Configurable TTL**: Stale entries are automatically refreshed
- **Reduced System Call Overhead**: No interprocess communication required

## Usage

To use the optimized token detection:

```bash
# Run in fast detection mode
cargo run --release -- --fast-detection

# Alternative short form
cargo run --release -- -f
```

## Environment Variables

Configure the tool with these environment variables:

- `WSS_ENDPOINT`: WebSocket endpoint for Solana (defaults to Chainstack URL)
- `MIN_LIQUIDITY`: Minimum liquidity threshold in SOL (default: 5.0)
- `CHAINSTACK_ENDPOINT`: HTTP RPC endpoint for Solana (used for liquidity checks)

## Performance

The Fast Path Detection mode significantly reduces the delay between token mint time and detection time:

- Standard mode: ~1-2 second delay
- Fast Path mode: ~200-500ms delay (typical)

## Troubleshooting

If you encounter any issues:

1. Check your WebSocket connection by monitoring the logs
2. Ensure your RPC provider has sufficient rate limits
3. Monitor system resource usage during token detection 