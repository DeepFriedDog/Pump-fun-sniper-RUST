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
use tokio::task::JoinHandle;
use tokio::sync::oneshot;

// Global cached values
lazy_static! {
    static ref CACHED_BLOCKHASH: Mutex<(String, u64)> = Mutex::new((String::new(), 0));
    static ref WS_CONNECTION_POOL: Mutex<Vec<Option<(String, String)>>> = Mutex::new(Vec::with_capacity(3));
    pub static ref HTTP_CLIENT: Client = create_speed_optimized_client();
    static ref TRADER_NODE_WSS_URL: String = std::env::var("TRADER_NODE_WSS_URL")
        .unwrap_or_else(|_| "wss://solana-mainnet.core.chainstack.com/ws".to_string());
    static ref TRADER_NODE_RPC_URL: String = std::env::var("TRADER_NODE_RPC_URL")
        .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com".to_string());
    
    // Add missing caches that other code depends on
    pub static ref TOKEN_PRICE_CACHE: Mutex<HashMap<String, (f64, Instant)>> = Mutex::new(HashMap::new());
    pub static ref NEW_TOKEN_QUEUE: Mutex<VecDeque<models::TokenData>> = Mutex::new(VecDeque::new());
    
    // Task handles collection for graceful shutdown
    pub static ref TASK_HANDLES: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());
    
    // Cancellation senders for individual monitoring tasks
    pub static ref API_CANCEL_SENDERS: Mutex<Vec<(String, oneshot::Sender<()>)>> = Mutex::new(Vec::new());
}

// Constants
pub const PUMP_PROGRAM_ID: &str = "FunNVnBYXCzd1MxQV9qOAHQJUmfKVTUfkj8hAyGRSXL5"; 

/// Helper function to get bonding curve for a mint
pub async fn get_bonding_curve_for_mint(mint: &str) -> Option<String> {
    // First check if we have it in the token queue
    if let Ok(queue) = NEW_TOKEN_QUEUE.try_lock() {
        for token in queue.iter() {
            if token.mint == mint {
                if let Some(metadata) = &token.metadata {
                    if metadata.starts_with("bonding_curve:") {
                        return Some(metadata.replace("bonding_curve:", ""));
                    }
                }
            }
        }
    }
    
    // If not found in queue, try getting it from token_detector
    if let Ok(mint_pubkey) = solana_sdk::pubkey::Pubkey::from_str(mint) {
        let bonding_curve = crate::token_detector::get_bonding_curve_address(&mint_pubkey).0;
        return Some(bonding_curve.to_string());
    }
    
    None
}

// Add a debugging function to dump environment variables
pub fn dump_environment_variables() {
    println!("========== ENVIRONMENT VARIABLES DUMP ==========");
    println!("DEBUG_WEBSOCKET_MESSAGES: {}", std::env::var("DEBUG_WEBSOCKET_MESSAGES").unwrap_or_else(|_| "not set".to_string()));
    println!("DEBUG_BONDING_CURVE_UPDATES: {}", std::env::var("DEBUG_BONDING_CURVE_UPDATES").unwrap_or_else(|_| "not set".to_string()));
    println!("DISABLE_API_FALLBACK: {}", std::env::var("DISABLE_API_FALLBACK").unwrap_or_else(|_| "not set".to_string()));
    println!("BONDING_CURVE_ADDRESS: {}", std::env::var("BONDING_CURVE_ADDRESS").unwrap_or_else(|_| "not set".to_string()));
    println!("TOKEN_BONDING_CURVE: {}", std::env::var("TOKEN_BONDING_CURVE").unwrap_or_else(|_| "not set".to_string()));
    println!("CHAINSTACK_WSS_ENDPOINT: {}", std::env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| "not set".to_string()));
    println!("CHAINSTACK_USERNAME: {}", std::env::var("CHAINSTACK_USERNAME").unwrap_or_else(|_| "not set".to_string()));
    println!("CHAINSTACK_PASSWORD: {}", match std::env::var("CHAINSTACK_PASSWORD") {
        Ok(_) => "set (value hidden)",
        Err(_) => "not set",
    });
    println!("PRIVATE_KEY: {}", match std::env::var("PRIVATE_KEY") {
        Ok(_) => "set (value hidden)",
        Err(_) => "not set",
    });
    println!("WALLET: {}", std::env::var("WALLET").unwrap_or_else(|_| "not set".to_string()));
    println!("==============================================");
}

// Make absolutely sure that start_price_monitor is properly exported
pub use price::start_price_monitor; 