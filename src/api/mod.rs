// src/api/mod.rs
// Central module that re-exports all components

// Import and expose submodules
mod models;
pub mod websocket;
pub mod transactions;
mod price;
mod utils;

// Re-export all public items from submodules
pub use models::*;
pub use websocket::*;
pub use transactions::*;
pub use price::*;
pub use utils::*;

// Export any shared types, helpers or constants here
use anyhow::{anyhow, Context, Result};
use lazy_static::lazy_static;
use log::{debug, error, info, trace, warn};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature};
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use url::Url;

// Global cached values
lazy_static! {
    static ref CACHED_BLOCKHASH: Mutex<(String, u64)> = Mutex::new((String::new(), 0));
    static ref WS_CONNECTION_POOL: Mutex<Vec<(tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<std::net::TcpStream>>, tokio_tungstenite::tungstenite::handshake::client::Response)>> = Mutex::new(Vec::new());
    pub static ref HTTP_CLIENT: Client = create_speed_optimized_client();
    static ref TRADER_NODE_WSS_URL: String = std::env::var("TRADER_NODE_WSS_URL")
        .unwrap_or_else(|_| "wss://solana-mainnet.core.chainstack.com/ws".to_string());
    static ref TRADER_NODE_RPC_URL: String = std::env::var("TRADER_NODE_RPC_URL")
        .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com".to_string());
    
    // Add missing caches that other code depends on
    pub static ref TOKEN_PRICE_CACHE: Mutex<HashMap<String, (f64, Instant)>> = Mutex::new(HashMap::new());
    pub static ref NEW_TOKEN_QUEUE: Mutex<VecDeque<models::TokenData>> = Mutex::new(VecDeque::new());
}

// Constants
pub const PUMP_PROGRAM_ID: &str = "FunNVnBYXCzd1MxQV9qOAHQJUmfKVTUfkj8hAyGRSXL5"; 