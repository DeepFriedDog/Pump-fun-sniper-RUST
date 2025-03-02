use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use rand::Rng;
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message},
};
use url::Url;
use std::str::FromStr;
use solana_program::pubkey::Pubkey;
use reqwest::Client;
use std::collections::{HashMap, VecDeque};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;

use crate::token_detector::{self, DetectorTokenData};
use crate::websocket_test;
use crate::websocket_test::TokenData;

// Lazy-initialized HTTP client for RPC calls
lazy_static::lazy_static! {
    static ref HTTP_CLIENT: Client = Client::new();
}

/// Run WebSocket connection with automatic reconnection
pub async fn run_websocket_with_reconnect(
    endpoint: &str,
    max_attempts: Option<usize>,
    test_duration: Option<u64>,
) -> Result<Vec<TokenData>> {
    let max_attempts = max_attempts.unwrap_or(5);
    let test_duration = test_duration.unwrap_or(120);
    
    let start_time = Instant::now();
    let timeout_duration = Duration::from_secs(test_duration);
    
    let mut token_data_list = Vec::new();
    let mut attempt = 0;
    let mut backoff_secs = 1;
    
    // Track overall progress
    let mut message_count = 0;
    let mut token_creations = 0;
    
    info!("Starting WebSocket connection with reconnect logic");
    info!("Will listen for {} seconds with max {} reconnection attempts", test_duration, max_attempts);
    
    while attempt < max_attempts && start_time.elapsed() < timeout_duration {
        attempt += 1;
        
        let remaining_time = timeout_duration.saturating_sub(start_time.elapsed());
        
        if remaining_time.as_secs() == 0 {
            info!("Test duration reached, exiting");
            break;
        }
        
        info!("Connection attempt {}/{}", attempt, max_attempts);
        
        // If this isn't the first attempt, apply backoff with jitter
        if attempt > 1 {
            let jitter = rand::thread_rng().gen_range(0..=500);
            let backoff_ms = backoff_secs * 1000 + jitter;
            
            info!("Applying backoff: waiting for {}ms before reconnecting", backoff_ms);
            sleep(Duration::from_millis(backoff_ms)).await;
            
            // Exponential backoff with cap
            backoff_secs = std::cmp::min(backoff_secs * 2, 30);
        }
        
        // Try to run the WebSocket session
        let result = run_websocket_session(
            endpoint, 
            remaining_time,
            &mut token_data_list,
            &mut message_count,
            &mut token_creations,
        ).await;
        
        match result {
            Ok(_) => {
                // Session ended normally (timeout or closed connection)
                info!("WebSocket session completed normally");
                break;
            }
            Err(e) => {
                // Connection error, will retry
                if let Some(ws_err) = e.downcast_ref::<WsError>() {
                    match ws_err {
                        WsError::Http(response) => {
                            error!("HTTP error connecting to WebSocket: {}", response.status());
                            if response.status() == 503 {
                                warn!("Service unavailable (503), server might be down or overloaded");
                            }
                        }
                        WsError::Io(io_err) => {
                            error!("IO error connecting to WebSocket: {}", io_err);
                        }
                        _ => {
                            error!("WebSocket error: {}", ws_err);
                        }
                    }
                } else {
                    error!("Error in WebSocket session: {}", e);
                }
                
                if attempt >= max_attempts {
                    error!("Maximum reconnection attempts reached");
                    return Err(anyhow!("Failed to establish WebSocket connection after {} attempts", max_attempts));
                }
                
                warn!("Will try to reconnect (attempt {}/{})", attempt, max_attempts);
            }
        }
    }
    
    info!("WebSocket completed. Processed {} messages, found {} tokens", 
          message_count, token_creations);
    
    Ok(token_data_list)
}

async fn run_websocket_session(
    endpoint: &str,
    remaining_time: Duration,
    token_data_list: &mut Vec<TokenData>,
    message_count: &mut usize,
    token_creations: &mut usize,
) -> Result<()> {
    info!("Connecting to Chainstack WebSocket: {}", endpoint);
    
    // Parse URL and connect with optimized settings
    let url = Url::parse(endpoint)?;
    
    // Use connect_async with lower buffer sizes for faster processing
    let (mut ws_stream, _) = connect_async(url).await?;
    
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    info!("WebSocket connection established at timestamp: {}", timestamp);

    // Pump.fun program ID for token creation
    let pump_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    
    // Use a simple subscription request like in the Python script
    let subscription_request = json!({
        "client_request_time_ms": timestamp,
        "id": 1,
        "jsonrpc": "2.0",
        "method": "logsSubscribe",
        "params": [
            {"mentions": [pump_program_id]},
            {"commitment": "processed"}
        ]
    });
    
    // Send subscription request with debug info
    info!("Sending subscription request: {}", subscription_request);
    ws_stream.send(Message::Text(subscription_request.to_string())).await?;
    
    // Setup status updates every 30 seconds
    let mut status_interval = tokio::time::interval(Duration::from_secs(30));
    let session_start = Instant::now();
    
    // Process messages
    loop {
        tokio::select! {
            // Check if we've exceeded remaining time
            _ = sleep(remaining_time.saturating_sub(session_start.elapsed())) => {
                info!("Session time limit reached");
                // Send close frame
                if let Err(e) = ws_stream.send(Message::Close(None)).await {
                    warn!("Failed to send close frame: {}", e);
                }
                break;
            }
            
            // Status update interval
            _ = status_interval.tick() => {
                let elapsed = session_start.elapsed();
                let remaining = remaining_time.saturating_sub(elapsed);
                let mins = remaining.as_secs() / 60;
                let secs = remaining.as_secs() % 60;
                
                info!("Status: Processed {} messages, found {} tokens. Session ends in {}m{}s", 
                    *message_count, *token_creations, mins, secs);
            }
            
            // Process WebSocket messages
            result = ws_stream.next() => {
                match result {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Text(text) => {
                                *message_count += 1;
                                debug!("Raw WebSocket message received: {}", text);
                                
                                // Parse and process message
                                if let Ok(json_msg) = serde_json::from_str::<Value>(&text) {
                                    if let Some(result) = process_message(&json_msg, token_creations).await {
                                        token_data_list.push(result);
                                    }
                                }
                            }
                            Message::Ping(data) => {
                                debug!("WebSocket ping received");
                                if let Err(e) = ws_stream.send(Message::Pong(data)).await {
                                    error!("Failed to send pong: {}", e);
                                }
                            }
                            Message::Close(frame) => {
                                info!("WebSocket close frame received: {:?}", frame);
                                break;
                            }
                            _ => {}
                        }
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        return Err(anyhow!("WebSocket stream error: {}", e));
                    }
                    None => {
                        info!("WebSocket connection closed by server");
                        break;
                    }
                }
            }
        }
    }
    
    info!("WebSocket session ended");
    Ok(())
}

// Process message and directly parse logs without making additional RPC calls
async fn process_message(
    message: &serde_json::Value, 
    token_creations: &mut usize
) -> Option<TokenData> {
    // Check if this is a subscription confirmation
    if let Some(id) = message.get("id").and_then(|v| v.as_i64()) {
        info!("WebSocket subscription confirmed with id: {}", id);
        return None;
    }
    
    // Check if this is a notification
    if let Some(method) = message.get("method").and_then(|v| v.as_str()) {
        if method == "logsNotification" {
            if let Some(params) = message.get("params") {
                if let Some(result) = params.get("result") {
                    if let Some(value) = result.get("value") {
                        let signature_str = value.get("signature").and_then(|v| v.as_str()).unwrap_or("unknown");
                        let signature = signature_str.to_string();
                        
                        // Clone the logs array to create a longer-lived value
                        let logs_array = value.get("logs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        
                        // Get slot info for timing comparison
                        let slot = value.get("slot").and_then(|v| v.as_u64()).unwrap_or(0);
                        
                        // Check if this is a token creation instruction
                        if logs_array.iter().any(|log| {
                            log.as_str().map_or(false, |s| s.contains("Program log: Instruction: Create"))
                        }) {
                            // Parse program data from logs
                            for log in logs_array {
                                if let Some(log_str) = log.as_str() {
                                    if log_str.contains("Program data:") {
                                        let parts: Vec<&str> = log_str.split("Program data: ").collect();
                                        if parts.len() > 1 {
                                            let data_base64 = parts[1];
                                            if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data_base64) {
                                                if let Some(mut token_data) = crate::websocket_test::parse_instruction(&data) {
                                                    // Set the transaction signature for the token data
                                                    token_data.tx_signature = signature.clone();
                                                    
                                                    // Create a clone for async processing
                                                    let token_data_clone = token_data.clone();
                                                    
                                                    // Spawn a task to check liquidity asynchronously
                                                    tokio::spawn(async move {
                                                        // Get MIN_LIQUIDITY from environment variable
                                                        let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "4.0".to_string());
                                                        let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(4.0);
                                                        
                                                        debug!("Checking liquidity for token: {} (mint: {})", token_data_clone.name, token_data_clone.mint);
                                                        debug!("Using bonding curve: {}", token_data_clone.bonding_curve);
                                                        debug!("Minimum liquidity threshold: {} SOL", min_liquidity);
                                                        
                                                        let liquidity_start = std::time::Instant::now();
                                                        
                                                        // Use the original token_detector function which is known to work
                                                        match token_detector::check_token_liquidity(
                                                            &token_data_clone.mint,
                                                            &token_data_clone.bonding_curve,
                                                            min_liquidity // Use the environment variable instead of hardcoded value
                                                        ).await {
                                                            Ok((has_liquidity, sol_amount)) => {
                                                                // Log the token creation with liquidity info
                                                                let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                                let elapsed = liquidity_start.elapsed();
                                                                
                                                                debug!("Liquidity check completed in {:.2}ms: {} SOL (required: {} SOL)", 
                                                                    elapsed.as_millis(), sol_amount, min_liquidity);
                                                                
                                                                info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {:.2} SOL {}", 
                                                                    token_data_clone.name, token_data_clone.mint, sol_amount, check_mark);
                                                            },
                                                            Err(e) => {
                                                                // Log error and show token with 0 SOL
                                                                debug!("Error checking liquidity: {}", e);
                                                                info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 0.00 SOL ❌", 
                                                                    token_data_clone.name, token_data_clone.mint);
                                                            }
                                                        }
                                                    });
                                                    
                                                    *token_creations += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    None
}

/// Fast liquidity check using direct getBalance with processed commitment
async fn fast_check_liquidity(mint: &str, bonding_curve: &str, liquidity_threshold: f64) -> (bool, f64) {
    debug!("Starting fast_check_liquidity for mint: {}, bonding_curve: {}", mint, bonding_curve);
    
    // Try to convert addresses to public keys
    let mint_pubkey = match Pubkey::from_str(mint) {
        Ok(pk) => pk,
        Err(e) => {
            debug!("Error parsing mint address: {}", e);
            return (false, 0.0);
        }
    };
    
    let bonding_curve_pubkey = match Pubkey::from_str(bonding_curve) {
        Ok(pk) => pk,
        Err(e) => {
            debug!("Error parsing bonding curve address: {}", e);
            return (false, 0.0);
        }
    };
    
    // Calculate the associated bonding curve address
    let associated_bonding_curve = match find_associated_bonding_curve(&mint_pubkey, &bonding_curve_pubkey) {
        Ok(addr) => {
            debug!("Derived associated bonding curve address: {}", addr);
            addr
        },
        Err(e) => {
            debug!("Error finding associated bonding curve: {}", e);
            return (false, 0.0);
        }
    };
    
    // Get Chainstack endpoint
    let rpc_url = std::env::var("CHAINSTACK_ENDPOINT")
        .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string());
    
    // Prepare direct getBalance request with processed commitment
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBalance",
        "params": [
            associated_bonding_curve.to_string(),
            {"commitment": "processed"}
        ]
    });
    
    debug!("Sending getBalance request to {}: {}", rpc_url, request_body);
    
    // Make direct HTTP request
    match HTTP_CLIENT.post(&rpc_url)
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await {
            Ok(response) => {
                let status = response.status();
                debug!("Got HTTP response with status: {}", status);
                
                match response.text().await {
                    Ok(text) => {
                        debug!("Response text: {}", text);
                        match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(json) => {
                                debug!("Parsed JSON response: {}", json);
                                
                                // The correct path for getBalance response is result.value
                                if let Some(result) = json.get("result") {
                                    if let Some(value) = result.as_u64() {
                                        let sol_amount = value as f64 / 1_000_000_000.0;
                                        debug!("Extracted SOL amount: {} (from lamports: {})", sol_amount, value);
                                        return (sol_amount >= liquidity_threshold, sol_amount);
                                    } else {
                                        debug!("Couldn't extract lamports as u64 from result: {:?}", result);
                                    }
                                } else {
                                    debug!("No 'result' field in response: {:?}", json);
                                    
                                    // Check for error in response
                                    if let Some(error) = json.get("error") {
                                        debug!("Error in response: {:?}", error);
                                    }
                                }
                            },
                            Err(e) => debug!("Error parsing JSON response: {}", e),
                        }
                    },
                    Err(e) => debug!("Error getting response text: {}", e),
                }
                
                debug!("Defaulting to 0.0 SOL due to response parsing issues");
                (false, 0.0)
            },
            Err(e) => {
                debug!("Error making HTTP request: {}", e);
                (false, 0.0)
            },
        }
}

/// Finds the associated bonding curve for a given mint and bonding curve
fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Result<Pubkey, Box<dyn std::error::Error>> {
    use crate::config::{ATA_PROGRAM_ID, TOKEN_PROGRAM_ID};
    
    let seeds = &[
        bonding_curve.as_ref(),
        TOKEN_PROGRAM_ID.as_ref(),
        mint.as_ref(),
    ];
    
    let (derived_address, _) = Pubkey::find_program_address(seeds, &ATA_PROGRAM_ID);
    Ok(derived_address)
} 