use dotenv::dotenv;
use lazy_static::lazy_static;
use solana_program::pubkey::Pubkey;
use std::env;
use std::str::FromStr;
use log::{debug, error, warn};
use std::sync::Arc;

// Load environment variables from config.env
fn load_env() {
    if let Err(_) = dotenv::from_filename("config.env") {
        warn!("Could not load config.env file");
    }
}

// WebSocket endpoint for the token monitor - completely separate from Warp transaction WebSocket
pub fn get_wss_endpoint() -> String {
    load_env(); // Load config.env file if exists
    let endpoint = env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| {
        // Default to Chainstack WebSocket endpoint as fallback
        warn!("CHAINSTACK_WSS_ENDPOINT not found, using default endpoint for token detection");
        "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    });
    
    debug!("Using WebSocket endpoint for token detection: {}", endpoint);
    endpoint
}

// Default WebSocket endpoint for the token monitor
pub const WSS_ENDPOINT: &str =
    "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab";

lazy_static! {
    // Pump.fun program ID
    pub static ref PUMP_PROGRAM_ID: Pubkey = Pubkey::from_str(
        &env::var("PUMP_PROGRAM_ID")
        .unwrap_or_else(|_| "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string())
    ).unwrap_or_else(|_| panic!("Invalid PUMP_PROGRAM_ID"));

    // Solana Token Program ID (standard constant)
    pub static ref TOKEN_PROGRAM_ID: Pubkey = Pubkey::from_str(
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
    ).unwrap();

    // Solana Associated Token Account Program ID (standard constant)
    pub static ref ATA_PROGRAM_ID: Pubkey = Pubkey::from_str(
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
    ).unwrap();

    // Cached node configuration to avoid repeated env var lookups
    static ref TRADER_NODE_WSS_URL: String = std::env::var("TRADER_NODE_WSS_URL")
        .or_else(|_| std::env::var("CHAINSTACK_TRADER_WSS_URL"))
        .unwrap_or_else(|_| {
            warn!("No trader node WebSocket URL found in environment variables. Using default RPC WebSocket endpoint.");
            std::env::var("CHAINSTACK_WSS_ENDPOINT")
                .unwrap_or_else(|_| {
                    warn!("No WebSocket endpoint found in environment. WebSocket functions may not work correctly.");
                    String::new()
                })
        });

    static ref TRADER_NODE_RPC_URL: String = std::env::var("TRADER_NODE_RPC_URL")
        .or_else(|_| std::env::var("CHAINSTACK_TRADER_RPC_URL"))
        .unwrap_or_else(|_| {
            warn!("No trader node RPC URL found in environment variables. Using default RPC endpoint.");
            std::env::var("CHAINSTACK_ENDPOINT")
                .unwrap_or_else(|_| {
                    warn!("No RPC endpoint found in environment. API functions may not work correctly.");
                    String::new()
                })
        });

    static ref TRADER_NODE_USERNAME: String = std::env::var("TRADER_NODE_USERNAME")
        .or_else(|_| std::env::var("CHAINSTACK_USERNAME"))
        .unwrap_or_else(|_| {
            warn!("No trader node username found in environment variables.");
            String::new()
        });

    static ref TRADER_NODE_PASSWORD: String = std::env::var("TRADER_NODE_PASSWORD")
        .or_else(|_| std::env::var("CHAINSTACK_PASSWORD"))
        .unwrap_or_else(|_| {
            warn!("No trader node password found in environment variables.");
            String::new()
        });
}

// Structure to hold the configuration
pub struct Config {
    pub wss_endpoint: String,
    pub pump_program_id: String,
    pub token_program_id: String,
    pub ata_program_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wss_endpoint: get_wss_endpoint(),
            pump_program_id: PUMP_PROGRAM_ID.to_string(),
            token_program_id: TOKEN_PROGRAM_ID.to_string(),
            ata_program_id: ATA_PROGRAM_ID.to_string(),
        }
    }
}

// Function to load configuration from environment variables
pub fn load_config() -> Config {
    load_env(); // Load config.env file
    Config::default()
}

// Function to determine if WebSockets should be used
pub fn use_websocket() -> bool {
    load_env(); // Load config.env file
    env::var("USE_WEBSOCKET")
        .unwrap_or_else(|_| "true".to_string())
        .to_lowercase()
        == "true"
}

/// Check if Warp transactions should be used
pub fn should_use_warp_transactions() -> bool {
    // Set default to false since Chainstack documentation states
    // that Warp transactions only work over HTTP
    std::env::var("USE_WARP_TRANSACTIONS")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false)
}

/// Get the WebSocket URL for trader node transactions, prioritizing Chainstack Warp endpoint
/// This is specifically for Warp transactions and completely separate from the token detection WebSocket
pub fn get_trader_node_ws_url() -> String {
    // Check if Chainstack-specific URL is available
    if should_use_warp_transactions() {
        if let Ok(chainstack_url) = std::env::var("CHAINSTACK_TRADER_WSS_URL") {
            if !chainstack_url.is_empty() {
                debug!("Using Chainstack Trader WebSocket URL for Warp transactions: {}", chainstack_url);
                return chainstack_url;
            }
        }
    }
    
    // Fallback to standard trader node URL - still separate from token detection WebSocket
    match std::env::var("TRADER_NODE_WSS_URL") {
        Ok(url) if !url.is_empty() => {
            debug!("Using Trader Node WebSocket URL for Warp transactions: {}", url);
            url
        },
        _ => {
            warn!("No dedicated Warp transaction WebSocket URL found. Using default RPC WebSocket endpoint");
            "wss://api.mainnet-beta.solana.com".to_string()
        }
    }
}

/// Get the RPC URL for the trader node
pub fn get_trader_node_rpc_url() -> String {
    // Use the cached trader node rpc URL from the env
    let url = TRADER_NODE_RPC_URL.clone();
    if url.is_empty() {
        error!("No trader node RPC URL configured");
    } else {
        debug!("Using trader node RPC URL: {}", url);
    }
    url
}

/// Get trader node credentials (username, password)
pub fn get_trader_node_credentials() -> (String, String) {
    (TRADER_NODE_USERNAME.clone(), TRADER_NODE_PASSWORD.clone())
}

/// Get the WebSocket connection timeout in seconds
pub fn get_websocket_timeout_seconds() -> u64 {
    std::env::var("WEBSOCKET_TIMEOUT_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10) // Default to 10 seconds if not specified
}

/// Get the transaction timeout in seconds
pub fn get_transaction_timeout_seconds() -> u64 {
    std::env::var("TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30) // Default to 30 seconds if not specified
}

// Add a function to get the commitment level with processed as default
pub fn get_commitment_level() -> String {
    std::env::var("COMMITMENT_LEVEL").unwrap_or_else(|_| "processed".to_string())
}
