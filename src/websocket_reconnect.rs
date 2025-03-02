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

use crate::token_detector::{self, DetectorTokenData};
use crate::websocket_test;
use crate::websocket_test::TokenData;

// Lazy-initialized HTTP client for RPC calls
lazy_static::lazy_static! {
    static ref HTTP_CLIENT: Client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client");
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
            let mut random = rand::rng();
            let jitter = random.random_range(0..=500);
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
                                        let parts: Vec<&str> = log_str.split(": ").collect();
                                        if parts.len() > 1 {
                                            // The data will be base64 encoded in the logs
                                            if let Ok(data) = STANDARD.decode(parts[1]) {
                                                // Parse the instruction data
                                                if let Some(mut token_data) = crate::websocket_test::parse_instruction(&data) {
                                                    // Set the transaction signature
                                                    token_data.tx_signature = signature.clone();
                                                    
                                                    // Create a clone for async processing
                                                    let token_data_clone = token_data.clone();
                                                    
                                                    // Create TokenData for our result immediately
                                                    let token_data_result = TokenData {
                                                        name: token_data.name.clone(),
                                                        symbol: token_data.symbol.clone(),
                                                        uri: token_data.uri.clone(),
                                                        mint: token_data.mint.clone(),
                                                        bonding_curve: token_data.bonding_curve.clone(),
                                                        user: token_data.user.clone(),
                                                        tx_signature: token_data.tx_signature.clone(),
                                                    };
                                                    
                                                    // Spawn a task to check liquidity asynchronously without blocking detection
                                                    tokio::spawn(async move {
                                                        // Check token liquidity
                                                        let (has_liquidity, sol_amount) = match token_detector::check_token_liquidity(
                                                            &token_data_clone.mint,
                                                            &token_data_clone.bonding_curve,
                                                            0.0
                                                        ).await {
                                                            Ok((has_liq, amount)) => (has_liq, amount),
                                                            Err(_) => (false, 0.0),
                                                        };
                                                        
                                                        // Log in the desired format
                                                        let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                        info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {:.2} SOL {}", 
                                                             token_data_clone.name, token_data_clone.mint, sol_amount, check_mark);
                                                    });
                                                    
                                                    *token_creations += 1;
                                                    return Some(token_data_result);
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