use anyhow::Result;
use base64::prelude::*;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use reqwest::Client;
use serde_json::{json, Value};
use solana_client::rpc_client::RpcClient;
use solana_program::pubkey::Pubkey;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

// Import config module
mod config;

// Constants
const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

// Global cache for token data
lazy_static::lazy_static! {
    static ref TOKEN_CACHE: Mutex<HashMap<String, CachedTokenData>> = Mutex::new(HashMap::new());
    static ref LIQUIDITY_CACHE: TokioMutex<HashMap<String, (bool, std::time::Instant)>> = TokioMutex::new(HashMap::new());
    static ref BONDING_CURVE_MAP: TokioMutex<HashMap<String, String>> = TokioMutex::new(HashMap::new());
    static ref WEBSOCKET_MESSAGES: TokioMutex<VecDeque<String>> = TokioMutex::new(VecDeque::new());
}

// Struct to hold cached token data
struct CachedTokenData {
    price: f64,
    balance: f64,
    timestamp: Instant,
}

// Struct to represent a new token
#[derive(Debug, Clone)]
pub struct NewToken {
    pub token_name: String,
    pub token_symbol: String,
    pub mint_address: String,
    pub creator_address: String,
    pub transaction_signature: String,
    pub timestamp: i64,
}

// Create a chainstack client
pub fn create_chainstack_client() -> Client {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client");
    
    client
}

// Get the chainstack endpoint
pub fn get_chainstack_endpoint() -> String {
    std::env::var("CHAINSTACK_RPC_ENDPOINT")
        .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com/".to_string())
}

// Make a JSON-RPC call
async fn make_jsonrpc_call(client: &Client, method: &str, params: Value) -> Result<Value> {
    let endpoint = get_chainstack_endpoint();
    
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    });
    
    let response = client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await?;
    
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "HTTP error: {} - {}",
            response.status(),
            response.text().await?
        ));
    }
    
    let response_json: Value = response.json().await?;
    
    if let Some(error) = response_json.get("error") {
        return Err(anyhow::anyhow!("JSON-RPC error: {}", error));
    }
    
    Ok(response_json.get("result").cloned().unwrap_or(Value::Null))
}

// Process account WebSocket notifications
async fn process_account_websocket(
    mut ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    account: String,
) {
    info!("Starting account subscription handler for {}", account);
    
    // Set up ping interval to keep connection alive
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut last_message_time = Instant::now();
    
    // Process messages from this account's WebSocket
    loop {
        tokio::select! {
            // Handle incoming messages
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(message)) => {
                        // Update the last message time
                        last_message_time = Instant::now();
                        
                        if let Message::Text(text) = message {
                            // Process account notifications - simplified processing
                            if text.contains("accountNotification") {
                                debug!("Received account update for {}", account);
                                
                                // Update liquidity cache directly
                                let mut cache = LIQUIDITY_CACHE.lock().await;
                                cache.insert(
                                    account.clone(), 
                                    (true, std::time::Instant::now())
                                );
                                debug!("Updated liquidity cache for account {}", account);
                            }
                        }
                    },
                    Some(Err(e)) => {
                        error!("Error in account WebSocket for {}: {}", account, e);
                        break;
                    },
                    None => {
                        error!("WebSocket for {} disconnected", account);
                        break;
                    }
                }
            },
            // Send periodic pings
            _ = ping_interval.tick() => {
                // If no message for 60 seconds, consider connection dead
                if last_message_time.elapsed() > Duration::from_secs(60) {
                    error!("No messages received for account {} in 60 seconds, reconnecting", account);
                    break;
                }
                
                // Send ping
                let ping_msg = json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "ping"
                }).to_string();
                
                if let Err(e) = ws_stream.send(Message::Text(ping_msg)).await {
                    error!("Failed to send ping for account {}: {}", account, e);
                    break;
                }
            }
        }
    }
    
    // If we're here, the connection broke - try to reconnect
    error!("WebSocket connection for account {} ended, will reconnect on next poll", account);
}

// Subscribe to account updates via WebSocket
pub async fn subscribe_to_account(account: &str) -> Result<()> {
    // Get the WebSocket endpoint from environment or use default
    let ws_endpoint = std::env::var("CHAINSTACK_WS_ENDPOINT")
        .unwrap_or_else(|_| "wss://solana-mainnet.core.chainstack.com/".to_string());
    
    debug!("Connecting to WebSocket endpoint: {}", ws_endpoint);
    
    // Connect to the WebSocket
    let (mut ws_stream, _) = connect_async(&ws_endpoint).await?;
    
    // Subscribe to account updates
    let subscribe_msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "accountSubscribe",
        "params": [
            account,
            {
                "encoding": "base64",
                "commitment": "confirmed"
            }
        ]
    }).to_string();
    
    // Send subscription request
    ws_stream.send(Message::Text(subscribe_msg)).await?;
    
    // Wait for subscription confirmation
    if let Some(Ok(Message::Text(response))) = ws_stream.next().await {
        let response_json: Value = serde_json::from_str(&response)?;
        
        if let Some(error) = response_json.get("error") {
            return Err(anyhow::anyhow!("WebSocket error: {}", error));
        }
        
        if let Some(subscription_id) = response_json.get("result") {
            info!("Subscribed to account for updates: {}", account);
            
            // Spawn a task to handle the WebSocket connection
            let account_owned = account.to_string();
            tokio::spawn(async move {
                process_account_websocket(ws_stream, account_owned).await;
            });
            
            return Ok(());
        }
    }
    
    Err(anyhow::anyhow!("Failed to subscribe to account updates"))
} 