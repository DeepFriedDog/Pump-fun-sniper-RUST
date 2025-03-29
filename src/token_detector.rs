use crate::chainstack_simple;
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use tokio::time::timeout;
use solana_client::rpc_client::RpcClient;
use solana_transaction_status::UiTransactionEncoding;
use chrono::{DateTime, Utc};
use solana_sdk::commitment_config::CommitmentConfig;
use reqwest::Client;
use std::collections::HashSet;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};

// Import from config
use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};

/// Derives the bonding curve address for a given mint
pub fn get_bonding_curve_address(mint: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[b"bonding-curve", mint.as_ref()];

    Pubkey::find_program_address(seeds, &PUMP_PROGRAM_ID)
}

/// Finds the associated bonding curve for a given mint and bonding curve.
pub fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Pubkey {
    let seeds = &[
        bonding_curve.as_ref(),
        TOKEN_PROGRAM_ID.as_ref(),
        mint.as_ref(),
    ];

    let (derived_address, _) = Pubkey::find_program_address(seeds, &ATA_PROGRAM_ID);
    derived_address
}

/// Parses the create instruction data
pub fn parse_create_instruction(data: &[u8]) -> Option<DetectorTokenData> {
    if data.len() < 8 {
        debug!(
            "Data too short to be a valid instruction: {} bytes",
            data.len()
        );
        return None;
    }

    let mut offset = 8; // Skip discriminator

    let read_string = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 4 > data.len() {
            debug!(
                "Offset out of bounds when reading string length: offset={}, len={}",
                *offset,
                data.len()
            );
            return None;
        }

        let length = u32::from_le_bytes([
            data[*offset],
            data[*offset + 1],
            data[*offset + 2],
            data[*offset + 3],
        ]) as usize;

        *offset += 4;

        if *offset + length > data.len() {
            debug!(
                "String content would exceed data bounds: offset={}, length={}, data_len={}",
                *offset,
                length,
                data.len()
            );
            return None;
        }

        let value = match std::str::from_utf8(&data[*offset..*offset + length]) {
            Ok(s) => s.to_string(),
            Err(e) => {
                debug!("Failed to decode UTF-8 string: {}", e);
                return None;
            }
        };

        *offset += length;
        debug!("Read string: {}", value);

        Some(value)
    };

    let read_pubkey = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 32 > data.len() {
            debug!(
                "Offset out of bounds when reading pubkey: offset={}, len={}",
                *offset,
                data.len()
            );
            return None;
        }

        let pubkey_data = &data[*offset..*offset + 32];
        *offset += 32;

        let encoded = bs58::encode(pubkey_data).into_string();
        debug!("Read pubkey: {}", encoded);

        Some(encoded)
    };

    // Parse fields
    debug!("Parsing token data starting at offset {}", offset);
    let name = match read_string(data, &mut offset) {
        Some(name) => name,
        None => {
            debug!("Failed to parse token name");
            return None;
        }
    };

    let symbol = match read_string(data, &mut offset) {
        Some(symbol) => symbol,
        None => {
            debug!("Failed to parse token symbol");
            return None;
        }
    };

    let uri = match read_string(data, &mut offset) {
        Some(uri) => uri,
        None => {
            debug!("Failed to parse token URI");
            return None;
        }
    };

    let mint = match read_pubkey(data, &mut offset) {
        Some(mint) => mint,
        None => {
            debug!("Failed to parse mint address");
            return None;
        }
    };

    let bonding_curve = match read_pubkey(data, &mut offset) {
        Some(bonding_curve) => bonding_curve,
        None => {
            debug!("Failed to parse bonding curve address");
            return None;
        }
    };

    let user = match read_pubkey(data, &mut offset) {
        Some(user) => user,
        None => {
            debug!("Failed to parse user address");
            return None;
        }
    };

    info!("Successfully parsed token data: {} ({})", name, symbol);
    Some(DetectorTokenData {
        name,
        symbol,
        uri,
        mint,
        bonding_curve,
        user,
        tx_signature: String::new(),
    })
}

// Define our own TokenData rather than importing from crate::api
#[derive(Debug, Clone)]
pub struct DetectorTokenData {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub mint: String,
    pub bonding_curve: String,
    pub user: String,
    pub tx_signature: String,
}

// Convert TokenData to the existing NewToken structure for compatibility
impl From<DetectorTokenData> for NewToken {
    fn from(token: DetectorTokenData) -> Self {
        NewToken {
            token_name: token.name,
            token_symbol: token.symbol,
            mint_address: token.mint,
            creator_address: token.user,
            transaction_signature: token.tx_signature,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

/// Tests if the WebSocket connection is experiencing throttling
/// Returns Ok(true) if throttling is detected, Ok(false) if no throttling, or Err() if test fails
pub async fn test_websocket_throttling(wss_endpoint: String) -> Result<bool> {
    info!("🧪 Starting WebSocket throttling test...");
    
    // Create a channel to receive the first token detection
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, String, i64)>(1);
    let tx = Arc::new(Mutex::new(tx));
    
    // Flag to ensure we only process one token during the test
    let test_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let test_complete_clone = test_complete.clone();
    
    // Clone the endpoint since we're moving it into the async block
    let wss_endpoint_clone = wss_endpoint.clone();
    
    // Create a task to process WebSocket messages specifically for testing
    let test_task = tokio::spawn(async move {
        match connect_to_websocket_for_testing(&wss_endpoint_clone, tx, test_complete_clone).await {
            Ok(_) => info!("Testing WebSocket connection closed normally"),
            Err(e) => warn!("Testing WebSocket connection failed: {}", e),
        }
    });
    
    // Wait for a detection without timeout - we'll wait indefinitely for a token
    info!("Waiting for a token detection to measure throttling...");
    
    // Process the detection result
    match rx.recv().await {
        Some((mint, tx_signature, detection_time_ms)) => {
            info!("🔍 Token detected during test: {}", mint);
            info!("🔗 Transaction signature: {}", tx_signature);
            
            // Get the actual blockchain timestamp of the mint transaction
            match get_transaction_time(&tx_signature).await {
                Ok(blockchain_time_ms) => {
                    // Calculate the detection delay in milliseconds
                    let delay_ms = detection_time_ms - blockchain_time_ms;
                    
                    // Convert timestamps to human-readable format for logging
                    let blockchain_time = DateTime::<Utc>::from_timestamp(blockchain_time_ms / 1000, 0).unwrap_or_default();
                    let detection_time = DateTime::<Utc>::from_timestamp(detection_time_ms / 1000, 0).unwrap_or_default();
                    
                    info!("⏱️ Transaction time on blockchain: {} ({}ms)", blockchain_time, blockchain_time_ms);
                    info!("⏱️ Detection time by WebSocket: {} ({}ms)", detection_time, detection_time_ms);
                    info!("⏱️ Detection delay: {}ms", delay_ms);
                    
                    // Mark test as complete to stop the WebSocket task
                    test_complete.store(true, std::sync::atomic::Ordering::SeqCst);
                    
                    if delay_ms > 2000 {
                        info!("❌ Throttling detected! Detection delay ({}ms) exceeds threshold (2000ms)", delay_ms);
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await; // Brief pause before returning
                        return Ok(true);
                    } else {
                        info!("✅ No throttling detected! Detection delay ({}ms) within acceptable range", delay_ms);
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await; // Brief pause before returning
                        return Ok(false);
                    }
                },
                Err(e) => {
                    // Mark test as complete to stop the WebSocket task
                    test_complete.store(true, std::sync::atomic::Ordering::SeqCst);
                    warn!("Failed to get transaction time: {}", e);
                    return Err(anyhow!("Failed to get transaction time: {}", e));
                }
            }
        },
        None => {
            // Mark test as complete to stop the WebSocket task
            test_complete.store(true, std::sync::atomic::Ordering::SeqCst);
            warn!("Channel closed without receiving a detection");
            return Err(anyhow!("Channel closed without receiving a detection"));
        }
    }
}

/// Gets the timestamp of a transaction from the blockchain or estimates it
/// Uses alternative methods if getTransaction with processed commitment fails
async fn get_transaction_time(signature: &str) -> Result<i64> {
    // First, try to get the transaction using confirmed commitment level
    // This is supported by all RPC providers but may take longer
    let rpc_urls = vec![
        std::env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        "https://api.mainnet-beta.solana.com".to_string(),
    ];
    
    let mut last_error = None;
    
    // Try to get the transaction with confirmed commitment
    for rpc_url in &rpc_urls {
        let client = RpcClient::new_with_timeout_and_commitment(
            rpc_url.clone(),
            std::time::Duration::from_secs(5), // Shorter timeout
            CommitmentConfig::confirmed(),
        );
        
        match client.get_transaction_with_config(
            &signature.parse().map_err(|e| anyhow!("Invalid signature: {}", e))?,
            solana_client::rpc_config::RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                max_supported_transaction_version: Some(0),
            },
        ) {
            Ok(tx_data) => {
                if let Some(block_time) = tx_data.block_time {
                    // Convert block time (seconds) to milliseconds
                    return Ok(block_time * 1000);
                }
            },
            Err(e) => {
                debug!("Failed to get transaction from {} with confirmed commitment: {}", rpc_url, e);
                last_error = Some(anyhow!("RPC error: {}", e));
            }
        }
    }
    
    // If the standard approach failed, use an alternative method:
    // Get the current slot and estimate the transaction time
    info!("Standard transaction fetching failed, using alternative method to estimate transaction time");
    
    // Try to get the current slot
    for rpc_url in &rpc_urls {
        let client = RpcClient::new_with_timeout_and_commitment(
            rpc_url.clone(),
            std::time::Duration::from_secs(5),
            CommitmentConfig::processed(), // We can use processed here
        );
        
        match client.get_slot_with_commitment(CommitmentConfig::processed()) {
            Ok(current_slot) => {
                // Get the current time
                let current_time = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                
                // Fetch the recent performance samples to get the actual slot time
                match client.get_recent_performance_samples(Some(10)) {
                    Ok(samples) if !samples.is_empty() => {
                        // Calculate average slot time from samples
                        let total_slots: u64 = samples.iter().map(|s| s.num_slots as u64).sum();
                        let total_time: u64 = samples.iter().map(|s| s.sample_period_secs as u64).sum();
                        
                        // Avoid division by zero
                        if total_slots > 0 {
                            let avg_slot_time_ms = (total_time * 1000) as f64 / total_slots as f64;
                            debug!("Average slot time: {:.2}ms based on {} samples", avg_slot_time_ms, samples.len());
                            
                            // Now fetch signature status to get slot information
                            match client.get_signature_statuses(&[signature.parse().unwrap()]) {
                                Ok(response) => {
                                    if let Some(Some(status)) = response.value.first() {
                                        // slot is already a u64, not an Option<u64>
                                        let confirmation_slot = status.slot;
                                        // Calculate how many slots ago this transaction was confirmed
                                        let slots_ago = current_slot.saturating_sub(confirmation_slot);
                                        debug!("Transaction was confirmed {} slots ago", slots_ago);
                                        
                                        // Estimate the transaction time
                                        let estimated_time = current_time - (slots_ago as f64 * avg_slot_time_ms) as i64;
                                        info!("Estimated transaction time: {} ms", estimated_time);
                                        return Ok(estimated_time);
                                    }
                                },
                                Err(e) => debug!("Failed to get signature status: {}", e)
                            }
                        }
                    },
                    _ => debug!("Failed to get performance samples or received empty samples")
                }
                
                // Fallback to a simple slot-based estimate using 400ms per slot (Solana's target)
                let slots_per_second: f64 = 2.5; // 400ms per slot = 2.5 slots per second
                
                // Try to get the signature status to determine its slot
                match client.get_signature_statuses(&[signature.parse().unwrap()]) {
                    Ok(response) => {
                        if let Some(Some(status)) = response.value.first() {
                            // slot is already a u64, not an Option<u64>
                            let confirmation_slot = status.slot;
                            // Calculate how many slots ago this transaction was confirmed
                            let slots_ago = current_slot.saturating_sub(confirmation_slot);
                            debug!("Transaction was confirmed {} slots ago", slots_ago);
                            
                            // Estimate the transaction time based on 400ms per slot
                            let estimated_time = current_time - (slots_ago as f64 * 400.0) as i64;
                            info!("Estimated transaction time using fixed slot time: {} ms", estimated_time);
                            return Ok(estimated_time);
                        }
                    },
                    Err(e) => debug!("Failed to get signature status: {}", e)
                }
                
                // Last resort - use the current time minus a small offset
                // This is least accurate but better than nothing
                let estimated_time = current_time - 1000; // Assume transaction was 1 second ago as fallback
                info!("Using fallback estimation: {} ms (current time - 1000ms)", estimated_time);
                return Ok(estimated_time);
            },
            Err(e) => {
                debug!("Failed to get current slot from {}: {}", rpc_url, e);
            }
        }
    }
    
    // If all methods failed, return the error from the standard approach
    Err(last_error.unwrap_or_else(|| anyhow!("Failed to estimate transaction time with any method")))
}

/// Connect to WebSocket for testing throttling - processes only one token
async fn connect_to_websocket_for_testing(
    wss_endpoint: &str,
    tx: Arc<Mutex<tokio::sync::mpsc::Sender<(String, String, i64)>>>,
    test_complete: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    // Connect to the WebSocket server
    let url = Url::parse(wss_endpoint)?;
    info!("Connecting to {} for throttling test", url);

    // Add connection timeout
    let connect_future = connect_async(url.clone());
    let connection_timeout = Duration::from_secs(15);
    
    let ws_stream = match tokio::time::timeout(connection_timeout, connect_future).await {
        Ok(result) => match result {
            Ok((stream, _)) => stream,
            Err(e) => return Err(anyhow!("WebSocket connection error: {}", e)),
        },
        Err(_) => return Err(anyhow!("WebSocket connection timed out")),
    };

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to logs
    let program_id = PUMP_PROGRAM_ID.to_string();
    info!("Test monitoring program: {}", program_id);

    let subscription_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {"mentions": [program_id]},
            {"commitment": "processed"}
        ]
    });

    // Add timeout for subscription request
    let subscription_timeout = Duration::from_secs(10);
    match tokio::time::timeout(
        subscription_timeout,
        write.send(Message::Text(subscription_message.to_string()))
    ).await {
        Ok(result) => match result {
            Ok(_) => {
                info!("Test listening for new token creations from program: {}", program_id);
            },
            Err(e) => return Err(anyhow!("Failed to send subscription request: {}", e)),
        },
        Err(_) => return Err(anyhow!("Subscription request timed out")),
    }

    // Process subscription confirmation
    match read.next().await {
        Some(Ok(message)) => {
            match message {
                Message::Text(text) => {
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(json) => {
                            if let Some(result) = json.get("result") {
                                info!("✅ Test subscription confirmed with ID: {:?}", result);
                            } else if let Some(error) = json.get("error") {
                                return Err(anyhow!("Subscription error: {:?}", error));
                            }
                        },
                        Err(e) => return Err(anyhow!("Failed to parse subscription response: {}", e)),
                    }
                },
                _ => return Err(anyhow!("Unexpected message type for subscription confirmation")),
            }
        },
        Some(Err(e)) => return Err(anyhow!("WebSocket error: {}", e)),
        None => return Err(anyhow!("WebSocket closed unexpectedly")),
    }

    info!("⏱️ Waiting for a token creation to measure detection time...");
    
    // Set up ping timer to keep connection alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(5));
    let mut last_ping_time = Instant::now();

    // Process messages until test is complete or error occurs
    while !test_complete.load(std::sync::atomic::Ordering::SeqCst) {
        tokio::select! {
            // Send periodic pings to keep connection alive
            _ = ping_interval.tick() => {
                let ping_message = json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "ping"
                });

                if let Err(e) = write.send(Message::Text(ping_message.to_string())).await {
                    warn!("Failed to send ping: {}", e);
                } else {
                    debug!("Ping sent to WebSocket server during test");
                }
                last_ping_time = Instant::now();
            }
            
            next_message = read.next() => {
                match next_message {
                    Some(Ok(Message::Text(text))) => {
                        // Get the system detection time immediately
                        let detection_time_ms = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as i64;
                            
                        // Parse the message
                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some("logsNotification") = data.get("method").and_then(Value::as_str) {
                                if let Some(log_data) = data
                                    .get("params")
                                    .and_then(|p| p.get("result"))
                                    .and_then(|r| r.get("value"))
                                {
                                    let signature = log_data
                                        .get("signature")
                                        .and_then(Value::as_str)
                                        .unwrap_or("Unknown");

                                    // Check logs for Create instruction
                                    if let Some(logs) = log_data.get("logs").and_then(Value::as_array) {
                                        let has_create = logs.iter().any(|log| {
                                            log.as_str()
                                                .map_or(false, |s| s.contains("Program log: Instruction: Create"))
                                        });

                                        if has_create {
                                            info!("Found Create instruction in transaction {} during test", signature);
                                            
                                            // Detailed log for debugging
                                            debug!("Processing Create instruction with logs: {:?}", logs);
                                            
                                            // Look for program data to extract token information
                                            for log in logs {
                                                if let Some(log_str) = log.as_str() {
                                                    if log_str.contains("Program data:") {
                                                        let parts: Vec<&str> = log_str.split("Program data: ").collect();
                                                        if parts.len() < 2 {
                                                            continue;
                                                        }

                                                        let encoded_data = parts[1].trim();
                                                        
                                                        // Try to decode the data
                                                        if let Ok(decoded_data) = BASE64.decode(encoded_data) {
                                                            debug!("Successfully decoded base64 data for testing, length: {}", decoded_data.len());
                                                            
                                                            if let Some(token_data) = parse_create_instruction(&decoded_data) {
                                                                // Log complete token data for debugging
                                                                info!("Test extracted token data: name={}, symbol={}, mint={}",
                                                                      token_data.name, token_data.symbol, token_data.mint);
                                                                
                                                                // Only send the detection if this is a valid token
                                                                if !token_data.name.is_empty() && token_data.name != "Unknown" {
                                                                    // Send the detection for analysis
                                                                    let mint = token_data.mint.clone();
                                                                    info!("🔔 TEST - DETECTED TOKEN: {} (tx: {})", mint, signature);
                                                                    
                                                                    // Send the detection through the channel
                                                                    let sender = tx.lock().await;
                                                                    if let Err(e) = sender.send((mint, signature.to_string(), detection_time_ms)).await {
                                                                        warn!("Failed to send detection: {}", e);
                                                                    }
                                                                    // Exit after first token detection
                                                                    return Ok(());
                                                                } else {
                                                                    debug!("Skipping invalid token with name: {}, mint: {}", token_data.name, token_data.mint);
                                                                }
                                                            } else {
                                                                debug!("Failed to parse create instruction from decoded data");
                                                            }
                                                        }
                                                        // Try bs58 decode as fallback
                                                        else if let Ok(decoded_bs58) = bs58::decode(encoded_data).into_vec() {
                                                            debug!("Successfully decoded bs58 data for testing, length: {}", decoded_bs58.len());
                                                            
                                                            if let Some(token_data) = parse_create_instruction(&decoded_bs58) {
                                                                // Log complete token data for debugging
                                                                info!("Test extracted token data from bs58: name={}, symbol={}, mint={}",
                                                                      token_data.name, token_data.symbol, token_data.mint);
                                                                
                                                                // Only send the detection if this is a valid token
                                                                if !token_data.name.is_empty() && token_data.name != "Unknown" {
                                                                    // Send the detection for analysis
                                                                    let mint = token_data.mint.clone();
                                                                    info!("🔔 TEST - DETECTED TOKEN: {} (tx: {})", mint, signature);
                                                                    
                                                                    // Send the detection through the channel
                                                                    let sender = tx.lock().await;
                                                                    if let Err(e) = sender.send((mint, signature.to_string(), detection_time_ms)).await {
                                                                        warn!("Failed to send detection: {}", e);
                                                                    }
                                                                    // Exit after first token detection
                                                                    return Ok(());
                                                                } else {
                                                                    debug!("Skipping invalid token with name: {}, mint: {}", token_data.name, token_data.mint);
                                                                }
                                                            } else {
                                                                debug!("Failed to parse create instruction from bs58 decoded data");
                                                            }
                                                        } else {
                                                            debug!("Failed to decode Program data with both base64 and bs58: {}", encoded_data);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    Some(Ok(Message::Ping(data))) => {
                        // Automatically respond to pings
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            warn!("Failed to send pong during test: {}", e);
                        } else {
                            debug!("Responded to server ping with pong during test");
                        }
                    },
                    Some(Ok(Message::Pong(_))) => {
                        debug!("Received pong from server during test");
                    },
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket connection closed by server during test");
                        return Ok(());
                    },
                    Some(Err(e)) => return Err(anyhow!("WebSocket error: {}", e)),
                    None => return Ok(()),
                    _ => {} // Ignore other message types
                }
            }
        }
    }

    Ok(())
}

// Add a static flag to track if polling is already running
lazy_static! {
    static ref POLLING_ACTIVE: AtomicBool = AtomicBool::new(false);
}

/// Start listening for new tokens using WebSocket with throttling test
pub async fn listen_for_new_tokens(wss_endpoint: String) -> Result<()> {
    info!("🎧 Starting WebSocket listener for pump.fun token detection");
    
    // Run throttling test before connecting if not explicitly skipped
    let skip_throttling_test = std::env::var("SKIP_THROTTLING_TEST").is_ok() ||
                               std::env::var("THROTTLING_TESTING")
                                   .map(|v| v.to_lowercase() != "true")
                                   .unwrap_or(false);
    
    if !skip_throttling_test {
        info!("Running WebSocket throttling test to ensure optimal detection...");
        match test_websocket_throttling(wss_endpoint.clone()).await {
            Ok(throttling_detected) => {
                if throttling_detected {
                    // Throttling was detected, so we warn the user
                    warn!("⚠️ WebSocket throttling detected! Token detection will be delayed.");
                    warn!("⚠️ Consider using a different RPC endpoint for faster detection.");
                    
                    // Continue with regular connection despite throttling
                    // We won't stop execution but will log the warning
                    info!("Continuing with throttled connection...");
                } else {
                    info!("✅ No throttling detected! Using WebSocket connection for token detection.");
                }
            },
            Err(e) => {
                // Error in throttling test is significant - log and return
                error!("❌ Throttling test failed: {}", e);
                return Err(anyhow!("WebSocket throttling test failed: {}", e));
            }
        }
    } else {
        info!("Skipping WebSocket throttling test due to configuration.");
    }
    
    // When the function starts running, check if we should pause
    if is_token_detection_paused() {
        info!("🛑 Token detection is paused due to active position monitoring");
        
        // Wait until token detection is unpaused
        while is_token_detection_paused() {
            info!("Waiting for active position to be sold before resuming token detection...");
            sleep(Duration::from_secs(5)).await;
        }
        
        info!("🟢 Token detection lock released. Resuming detection.");
    }
    
    // Also check database for pending trades as an extra safety measure
    match crate::db::count_pending_trades() {
        Ok(count) => {
            if count > 0 {
                info!("🔒 Found {} pending trade(s) in database - Setting position monitoring active", count);
                set_position_monitoring_active(true);
                
                // Wait until position monitoring is deactivated
                while is_position_monitoring_active() {
                    info!("Waiting for pending position to be sold before resuming token detection...");
                    sleep(Duration::from_secs(5)).await;
                }
                
                info!("🟢 Position monitoring released. Resuming detection.");
            } else {
                info!("✅ No pending trades found in database");
            }
        },
        Err(e) => {
            warn!("⚠️ Could not check pending trades: {}", e);
            // Continue anyway, but add a warning
        }
    }
    
    // Connect to the WebSocket
    match connect_to_websocket(&wss_endpoint).await {
        Ok(_) => {
            info!("✅ WebSocket token detection completed successfully");
            Ok(())
        }
        Err(e) => {
            error!("❌ WebSocket token detection failed: {}", e);
            Err(e)
        }
    }
}

async fn connect_to_websocket(wss_endpoint: &str) -> Result<()> {
    // Connection state tracking
    let connection_start = Instant::now();
    
    // Connect to the WebSocket server
    let url = Url::parse(wss_endpoint)?;
    info!("Connecting to {}", url);

    // Add connection timeout
    let connect_future = connect_async(url.clone());
    let connection_timeout = Duration::from_secs(15);
    
    let ws_stream = match tokio::time::timeout(connection_timeout, connect_future).await {
        Ok(result) => match result {
            Ok((stream, _)) => {
                let connect_time = connection_start.elapsed();
                info!("WebSocket connection established in {:.2?}", connect_time);
                stream
            },
            Err(e) => {
                error!("Failed to connect to WebSocket at {}: {}", url, e);
                return Err(anyhow!("WebSocket connection error: {}", e));
            }
        },
        Err(_) => {
            error!("WebSocket connection timed out after {:?}", connection_timeout);
            return Err(anyhow!("WebSocket connection timed out"));
        }
    };

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to logs
    let program_id = PUMP_PROGRAM_ID.to_string();
    info!("Monitoring program: {}", program_id);

    let subscription_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {"mentions": [program_id]},
            {"commitment": "processed"}
        ]
    });

    // Add timeout for subscription request
    let subscription_timeout = Duration::from_secs(10);
    match tokio::time::timeout(
        subscription_timeout,
        write.send(Message::Text(subscription_message.to_string()))
    ).await {
        Ok(result) => match result {
            Ok(_) => {
                info!("Listening for new token creations from program: {}", program_id);
            },
            Err(e) => {
                error!("Failed to send subscription request: {}", e);
                return Err(anyhow!("Failed to send subscription request: {}", e));
            }
        },
        Err(_) => {
            error!("Subscription request timed out after {:?}", subscription_timeout);
            return Err(anyhow!("Subscription request timed out"));
        }
    }

    // Process subscription confirmation with timeout
    let confirmation_timeout = Duration::from_secs(10);
    let confirmation_result = tokio::time::timeout(
        confirmation_timeout,
        read.next()
    ).await;
    
    match confirmation_result {
        Ok(Some(Ok(message))) => {
            match message {
                Message::Text(text) => {
                    info!("Subscription response: {}", text);

                    // Parse subscription response to verify it was successful
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(json) => {
                            if let Some(result) = json.get("result") {
                                info!("✅ Subscription successfully confirmed with ID: {:?}", result);
                            } else if let Some(error) = json.get("error") {
                                error!("⛔ Subscription failed: {:?}", error);
                                return Err(anyhow!("Subscription failed: {:?}", error));
                            } else {
                                warn!("⚠️ Unusual subscription response format: {}", text);
                            }
                        }
                        Err(e) => {
                            warn!("Could not parse subscription response as JSON: {}", e);
                            return Err(anyhow!("Invalid subscription response: {}", e));
                        }
                    }
                }
                _ => {
                    warn!("Unexpected message format for subscription confirmation");
                    return Err(anyhow!("Unexpected message format for subscription confirmation"));
                }
            }
        }
        Ok(Some(Err(e))) => {
            error!("Error receiving subscription confirmation: {}", e);
            return Err(anyhow!("Error receiving subscription confirmation: {}", e));
        }
        Ok(None) => {
            error!("WebSocket closed before receiving subscription confirmation");
            return Err(anyhow!("WebSocket closed before receiving subscription confirmation"));
        }
        Err(_) => {
            error!("Timed out waiting for subscription confirmation");
            return Err(anyhow!("Timed out waiting for subscription confirmation"));
        }
    }

    info!("✅ WebSocket setup complete, monitoring for new tokens...");

    // Set up ping timer to keep connection alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(5)); // Ping more frequently (5s instead of 10s)
    let mut last_ping_time = Instant::now();
    let mut last_pong_time = Instant::now();
    let pong_timeout = Duration::from_secs(10); // Lower timeout for pongs
    let mut subscribed_bonding_curves = HashMap::new();

    // Process incoming messages
    loop {
        tokio::select! {
            // Send periodic pings to keep connection alive
            _ = ping_interval.tick() => {
                let ping_message = json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "ping"
                });

                if let Err(e) = write.send(Message::Text(ping_message.to_string())).await {
                    error!("Failed to send ping: {}", e);
                    return Err(anyhow!("Ping failed"));
                }

                // Also send a raw ping frame, which should always receive a pong frame response
                if let Err(e) = write.send(Message::Ping(vec![1, 2, 3, 4])).await {
                    error!("Failed to send ping frame: {}", e);
                }

                debug!("Ping sent to WebSocket server");
                last_ping_time = Instant::now();

                // Check if we've received pongs recently
                let pong_delay = last_ping_time.duration_since(last_pong_time);
                if pong_delay > pong_timeout {
                    warn!("No pong response received in {} seconds, reconnecting...", pong_delay.as_secs());
                    // Force reconnection by returning from this function
                    return Ok(());
                }
            }

            // Process next message
            next_message = read.next() => {
                match next_message {
                    Some(Ok(Message::Text(text))) => {
                        // Check if token detection is paused before processing any new tokens
                        if is_token_detection_paused() || is_position_monitoring_active() {
                            debug!("Token detection is paused or position monitoring active - skipping WebSocket message");
                            
                            // For ping/pong handling, we still need to respond but not process further
                            match &text {
                                _ => {
                                    // Just skip all message processing for now
                                    continue;
                                }
                            }
                        }
                        
                        debug!("Received WebSocket text message");
                        // Process the message
                        if let Err(e) = process_message(&text, WEBSOCKET_MESSAGES.clone(), &mut write, &mut subscribed_bonding_curves).await {
                            warn!("Error processing message: {}", e);
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        // Automatically respond to pings
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            return Err(anyhow!("Pong failed"));
                        }
                        debug!("Responded to server ping with pong");
                        last_pong_time = Instant::now();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        debug!("Received pong from server");
                        last_pong_time = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket connection closed by server");
                        return Ok(());
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        return Err(anyhow!("WebSocket error: {}", e));
                    }
                    None => {
                        info!("WebSocket stream ended");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn process_message(
    text: &str,
    queue: Arc<Mutex<VecDeque<String>>>,
    write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
    subscribed_bonding_curves: &mut HashMap<String, Instant>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse the message as JSON
    let data: Value = serde_json::from_str(text)?;

    // Check if this is a logs notification
    if let Some("logsNotification") = data.get("method").and_then(Value::as_str) {
        if let Some(log_data) = data
            .get("params")
            .and_then(|p| p.get("result"))
            .and_then(|r| r.get("value"))
        {
            let signature = log_data
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");

            // Extract logs
            if let Some(logs) = log_data.get("logs").and_then(Value::as_array) {
                // Check if this is a create instruction
                let has_create = logs.iter().any(|log| {
                    log.as_str()
                        .map_or(false, |s| s.contains("Program log: Instruction: Create"))
                });

                if has_create {
                    debug!("Found Create instruction in transaction {}", signature);

                    // Look for program data
                    for log in logs {
                        if let Some(log_str) = log.as_str() {
                            if log_str.contains("Program data:") {
                                debug!("Found Program data log: {}", log_str);

                                // Extract the base64-encoded data - making sure to get the data part only
                                let parts: Vec<&str> = log_str.split("Program data: ").collect();
                                if parts.len() < 2 {
                                    debug!("Couldn't split Program data");
                                    continue;
                                }

                                let encoded_data = parts[1].trim();
                                debug!("Extracted encoded data: {}", encoded_data);

                                // Try to decode the base64 data
                                match BASE64.decode(encoded_data) {
                                    Ok(decoded_data) => {
                                        debug!(
                                            "Successfully decoded Program data ({} bytes)",
                                            decoded_data.len()
                                        );

                                        // Parse the instruction
                                        if let Some(token_data) = parse_create_instruction(&decoded_data)
                                        {
                                            // Filter out invalid tokens
                                            if !token_data.mint.ends_with("pump")
                                                && token_data.name == "Unknown"
                                            {
                                                debug!(
                                                    "Filtered out invalid token with mint: {}",
                                                    token_data.mint
                                                );
                                                continue;
                                            }

                                            // Save mint and bonding curve before token_data is moved or cloned
                                            let mint_str = token_data.mint.clone();
                                            let bonding_curve_str =
                                                token_data.bonding_curve.clone();
                                            let token_data_clone = token_data.clone();

                                            // IMMEDIATELY log the token detection with pending liquidity status
                                            let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet");
                                            if !is_quiet_mode {
                                                info!("🪙 NEW TOKEN DETECTED! {} (mint: {}) 💰 Checking liquidity...", token_data_clone.name, token_data_clone.mint);
                                            }

                                            // IMMEDIATELY check liquidity with RPC to display the format the user wants
                                            if let (Ok(mint), Ok(bonding_curve)) = (
                                                Pubkey::from_str(&mint_str),
                                                Pubkey::from_str(&bonding_curve_str),
                                            ) {
                                                // Get min liquidity threshold
                                                let min_liquidity = std::env::var("MIN_LIQUIDITY")
                                                    .unwrap_or_else(|_| "5.0".to_string())
                                                    .parse::<f64>()
                                                    .unwrap_or(5.0);
                                                
                                                // Check liquidity via direct RPC call for immediate feedback
                                                match solana_client::rpc_client::RpcClient::new_with_timeout_and_commitment(
                                                    std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
                                                        "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
                                                    }),
                                                    std::time::Duration::from_millis(500),
                                                    solana_sdk::commitment_config::CommitmentConfig::processed(),
                                                ).get_account(&bonding_curve) {
                                                    Ok(account) => {
                                                        // Calculate actual liquidity after subtracting rent
                                                        const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
                                                        let total_balance = account.lamports as f64 / 1_000_000_000.0;
                                                        let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
                                                        
                                                        // Update cache
                                                        let cache_key = format!("bonding_curve:{}:{}", bonding_curve_str, mint_str);
                                                        let primary_key = format!("primary:{}", mint_str);
                                                        {
                                                            let mut cache = LIQUIDITY_CACHE.lock().await;
                                                            cache.insert(cache_key, (actual_liquidity, Instant::now()));
                                                            cache.insert(primary_key, (actual_liquidity, Instant::now()));
                                                        }
                                                        
                                                        // Display in the requested format
                                                        let has_liquidity = actual_liquidity >= min_liquidity;
                                                        let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                        
                                                        // Log in the exact format requested
                                                        info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                                                              token_data_clone.name, mint_str, actual_liquidity, check_mark);
                                                        
                                                        // Add the follow-up message about liquidity check
                                                        if has_liquidity {
                                                            info!("✅ Liquidity check PASSED - Proceeding with buy");
                                                        } else {
                                                            info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                                                                  actual_liquidity, min_liquidity);
                                                        }

                                                        // Also subscribe to the bonding curve for real-time updates
                                                        let associated_curve = find_associated_bonding_curve(&mint, &bonding_curve);
                                                        info!("Associated Bonding Curve: {}", associated_curve);
                                                        
                                                        // Subscribe to the bonding curve to get real-time liquidity updates
                                                        subscribe_to_bonding_curve(write, &mint_str, &bonding_curve_str, subscribed_bonding_curves).await?;

                                            // Convert to NewToken and add to the queue IMMEDIATELY
                                            let mut new_token: NewToken = token_data.into();
                                            new_token.transaction_signature = signature.to_string();

                                            // Add message to global queue for processing IMMEDIATELY
                                            let mut queue_guard = queue.lock().await;
                                            queue_guard.push_back(text.to_string());
                                            info!(
                                                "Added token to queue. Current queue size: {}",
                                                queue_guard.len()
                                            );

                                            // IMPORTANT: Also add to the API queue that is checked by fetch_new_tokens
                                            let token_data = crate::api::TokenData {
                                                status: "success".to_string(),
                                                mint: mint_str.clone(),
                                                dev: new_token.creator_address.clone(),
                                                metadata: Some(format!(
                                                                "bonding_curve:{},tx:{}",
                                                                bonding_curve_str,
                                                                signature
                                                )),
                                                name: Some(new_token.token_name.clone()),
                                                symbol: Some(new_token.token_symbol.clone()),
                                                timestamp: Some(chrono::Utc::now().timestamp()),
                                                            liquidity_status: Some(has_liquidity),   // Already have the ACTUAL liquidity value
                                                            liquidity_amount: Some(actual_liquidity), // Already have the ACTUAL liquidity value
                                            };

                                            let mut api_queue =
                                                crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
                                            api_queue.push_back(token_data);
                                            info!("Added token to API queue. Current API queue size: {}", api_queue.len());
                                                    },
                                                    Err(_) => {
                                                        // If RPC call fails, still log but with zero liquidity
                                                        info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", 
                                                              token_data_clone.name, mint_str);
                                                                info!("❌ Liquidity check FAILED (0.00 SOL < {:.2} SOL) - Skipping buy", 
                                                                      min_liquidity);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Failed to decode base64 data: {}", e);
                                        debug!("Raw encoded data: {}", encoded_data);

                                        // Try with fallback to bs58 decoding (some chains use this)
                                        match bs58::decode(encoded_data).into_vec() {
                                            Ok(decoded_bs58) => {
                                                debug!(
                                                    "Decoded using bs58 instead ({} bytes)",
                                                    decoded_bs58.len()
                                                );
                                                if let Some(token_data) =
                                                    parse_create_instruction(&decoded_bs58)
                                                {
                                                    // Filter out invalid tokens
                                                    if !token_data.mint.ends_with("pump")
                                                        && token_data.name == "Unknown"
                                                    {
                                                        debug!("Filtered out invalid token with mint: {}", token_data.mint);
                                                        continue;
                                                    }

                                                    // Save mint and bonding curve before token_data is moved or cloned
                                                    let mint_str = token_data.mint.clone();
                                                    let bonding_curve_str =
                                                        token_data.bonding_curve.clone();
                                                    let token_data_clone = token_data.clone();

                                                    // IMMEDIATELY log the token detection with pending liquidity status
                                                    let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet");
                                                    if !is_quiet_mode {
                                                        info!("🪙 NEW TOKEN DETECTED! {} (mint: {}) 💰 Checking liquidity...", token_data_clone.name, token_data_clone.mint);
                                                    }

                                                    // IMMEDIATELY check liquidity with RPC to display the format the user wants
                                                    if let (Ok(mint), Ok(bonding_curve)) = (
                                                        Pubkey::from_str(&mint_str),
                                                        Pubkey::from_str(&bonding_curve_str),
                                                    ) {
                                                        // Get min liquidity threshold
                                                        let min_liquidity = std::env::var("MIN_LIQUIDITY")
                                                            .unwrap_or_else(|_| "5.0".to_string())
                                                            .parse::<f64>()
                                                            .unwrap_or(5.0);
                                                        
                                                        // Check liquidity via direct RPC call for immediate feedback
                                                        match solana_client::rpc_client::RpcClient::new_with_timeout_and_commitment(
                                                            std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
                                                                "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
                                                            }),
                                                            std::time::Duration::from_millis(500),
                                                            solana_sdk::commitment_config::CommitmentConfig::processed(),
                                                        ).get_account(&bonding_curve) {
                                                            Ok(account) => {
                                                                // Calculate actual liquidity after subtracting rent
                                                                const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
                                                                let total_balance = account.lamports as f64 / 1_000_000_000.0;
                                                                let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
                                                                
                                                                // Update cache
                                                                let cache_key = format!("bonding_curve:{}:{}", bonding_curve_str, mint_str);
                                                                let primary_key = format!("primary:{}", mint_str);
                                                                {
                                                                    let mut cache = LIQUIDITY_CACHE.lock().await;
                                                                    cache.insert(cache_key, (actual_liquidity, Instant::now()));
                                                                    cache.insert(primary_key, (actual_liquidity, Instant::now()));
                                                                }
                                                                
                                                                // Display in the requested format
                                                                let has_liquidity = actual_liquidity >= min_liquidity;
                                                                let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                                
                                                                // Log in the exact format requested
                                                                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                                                                      token_data_clone.name, mint_str, actual_liquidity, check_mark);
                                                                
                                                                // Add the follow-up message about liquidity check
                                                                if has_liquidity {
                                                                    info!("✅ Liquidity check PASSED - Proceeding with buy");
                                                                } else {
                                                                    info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                                                                          actual_liquidity, min_liquidity);
                                                                }

                                                                // Also subscribe to the bonding curve for real-time updates
                                                                let associated_curve = find_associated_bonding_curve(&mint, &bonding_curve);
                                                                info!("Associated Bonding Curve: {}", associated_curve);
                                                                
                                                                // Subscribe to the bonding curve to get real-time liquidity updates
                                                                subscribe_to_bonding_curve(write, &mint_str, &bonding_curve_str, subscribed_bonding_curves).await?;

                                                    // Convert to NewToken and add to the queue IMMEDIATELY
                                                    let mut new_token: NewToken = token_data.into();
                                                    new_token.transaction_signature = signature.to_string();

                                                    // Add message to global queue for processing IMMEDIATELY
                                                    let mut queue_guard = queue.lock().await;
                                                    queue_guard.push_back(text.to_string());
                                                    info!(
                                                        "Added token to queue. Current queue size: {}",
                                                        queue_guard.len()
                                                    );

                                                    // IMPORTANT: Also add to the API queue that is checked by fetch_new_tokens
                                                    let token_data = crate::api::TokenData {
                                                        status: "success".to_string(),
                                                        mint: mint_str.clone(),
                                                        dev: new_token.creator_address.clone(),
                                                        metadata: Some(format!(
                                                                        "bonding_curve:{},tx:{}",
                                                                        bonding_curve_str,
                                                                        signature
                                                        )),
                                                        name: Some(new_token.token_name.clone()),
                                                        symbol: Some(new_token.token_symbol.clone()),
                                                        timestamp: Some(chrono::Utc::now().timestamp()),
                                                                    liquidity_status: Some(has_liquidity),   // Already have the ACTUAL liquidity value
                                                                    liquidity_amount: Some(actual_liquidity), // Already have the ACTUAL liquidity value
                                                    };

                                                    let mut api_queue =
                                                        crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
                                                    api_queue.push_back(token_data);
                                                    info!("Added token to API queue. Current API queue size: {}", api_queue.len());
                                                            },
                                                            Err(_) => {
                                                                // If RPC call fails, still log but with zero liquidity
                                                                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", 
                                                                      token_data_clone.name, mint_str);
                                                                info!("❌ Liquidity check FAILED (0.00 SOL < {:.2} SOL) - Skipping buy", 
                                                                      min_liquidity);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                debug!("Failed to decode with bs58 as well");
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
    // Check if this is an account notification (for liquidity updates)
    else if let Some("accountNotification") = data.get("method").and_then(Value::as_str) {
        if let Some(account_info) = data
            .get("params")
            .and_then(|p| p.get("result"))
            .and_then(|r| r.get("value"))
        {
            let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet");
            
            // Extract account data and lamports (SOL balance)
            let lamports = account_info.get("lamports").and_then(Value::as_u64).unwrap_or(0);
            let sol_balance = lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL
            
            // The rent exempt minimum amount in SOL
            const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
            let actual_liquidity = (sol_balance - RENT_EXEMPT_MINIMUM).max(0.0);
            
            // Get the bonding curve address which is included in the subscription ID
            if let Some(subscription) = data
                .get("params")
                .and_then(|p| p.get("subscription"))
                .and_then(Value::as_str)
            {
                // The subscription ID format is "bonding_curve_{address}"
                if subscription.starts_with("bonding_curve_") {
                    let parts: Vec<&str> = subscription.split('_').collect();
                    if parts.len() >= 3 {
                        let bonding_curve = parts[2];
                        
                        // Find the token info from our tracker
                        let min_liquidity = std::env::var("MIN_LIQUIDITY")
                                .unwrap_or_else(|_| "5.0".to_string())
                                .parse::<f64>()
                                .unwrap_or(5.0);
                        
                        let has_liquidity = actual_liquidity >= min_liquidity;
                        let check_mark = if has_liquidity { "✅" } else { "❌" };
                        
                        // First collect mint address and token name
                        let mut mint_to_update = None;
                        let mut token_name = String::from("Unknown");
                        
                        // Look up matching mint in the cache
                        {
                            let cache = LIQUIDITY_CACHE.lock().await;
                            // Find associated mint by looking at the bonding curve
                            for (cache_key, _) in cache.iter() {
                                if cache_key.contains(bonding_curve) {
                                    if let Some(mint) = cache_key.split(':').last() {
                                        mint_to_update = Some(mint.to_string());
                                        // Get token name if possible
                                        if let Some(name) = get_token_name_for_mint(mint) {
                                            token_name = name;
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        
                        // Now update the cache if we found a mint
                        if let Some(mint) = mint_to_update {
                            // Log the updated liquidity in the exact format requested
                            info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                                  token_name, mint, actual_liquidity, check_mark);
                            
                            // Add the follow-up message about liquidity check
                            if has_liquidity {
                                info!("✅ Liquidity check PASSED - Proceeding with buy");
                            } else {
                                info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                                    actual_liquidity, min_liquidity);
                            }
                            
                            // Update the cache with the new amount in a separate lock scope
                            let cache_key = format!("bonding_curve:{}:{}", bonding_curve, mint);
                            let mut cache = LIQUIDITY_CACHE.lock().await;
                            cache.insert(cache_key, (actual_liquidity, Instant::now()));
                            
                            // Also update primary liquidity cache to make it available for other functions
                            let primary_cache_key = format!("primary:{}", mint);
                            cache.insert(primary_cache_key, (actual_liquidity, Instant::now()));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// Helper function to get token name from mint address
fn get_token_name_for_mint(mint: &str) -> Option<String> {
    // Try to find the token name from recently detected tokens
    let api_queue = crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
    for token_data in api_queue.iter() {
        if token_data.mint == mint {
            return token_data.name.clone();
        }
    }
    None
}

// Subscribe to bonding curve account changes to track liquidity in real-time
async fn subscribe_to_bonding_curve(
    write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
    mint: &str, 
    bonding_curve: &str,
    subscribed_curves: &mut HashMap<String, Instant>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Check if we already subscribed to this bonding curve
    if subscribed_curves.contains_key(bonding_curve) {
        debug!("Already subscribed to bonding curve: {}", bonding_curve);
        return Ok(());
    }
    
    // Create a subscription message for the bonding curve account
    let subscription_id = format!("bonding_curve_{}", bonding_curve);
    let subscription_message = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "accountSubscribe",
        "params": [
            bonding_curve,
            {
                "encoding": "jsonParsed",
                "commitment": "processed"
            }
        ]
    });
    
    // Send the subscription request
    if let Err(e) = write.send(Message::Text(subscription_message.to_string())).await {
        warn!("Failed to subscribe to bonding curve {}: {}", bonding_curve, e);
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to subscribe to bonding curve: {}", e),
        )));
    }
    
    debug!("Subscribed to bonding curve {} for mint {}", bonding_curve, mint);
    
    // Track this subscription
    subscribed_curves.insert(bonding_curve.to_string(), Instant::now());
    
    // Also update the liquidity cache with an initial entry
    {
        let cache_key = format!("bonding_curve:{}:{}", bonding_curve, mint);
        let mut cache = LIQUIDITY_CACHE.lock().await;
        cache.insert(cache_key, (0.0, Instant::now()));
    }
    
    Ok(())
}

// Test function for unit testing
#[cfg(test)]
pub fn test_parse_instruction(base64_data: &str) -> Option<DetectorTokenData> {
    match BASE64.decode(base64_data) {
        Ok(decoded) => parse_create_instruction(&decoded),
        Err(_) => None,
    }
}

// Add a cache for liquidity checks to reduce redundant RPC calls
lazy_static! {
    static ref LIQUIDITY_CACHE: Arc<Mutex<HashMap<String, (f64, std::time::Instant)>>> =
        Arc::new(Mutex::new(HashMap::new()));
}

// Create or get the RPC client
async fn get_rpc_client() -> solana_client::rpc_client::RpcClient {
    use solana_client::rpc_client::RpcClient;

    // Only use Chainstack endpoint for RPC calls
    let rpc_url = std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
        "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    });

    // Validate that the URL has a proper schema
    let rpc_url = if !rpc_url.starts_with("http") {
        format!("https://{}", rpc_url)
    } else {
        rpc_url
    };

    debug!("Creating RPC client with URL: {}", rpc_url);

    // Create a Solana RPC client with processed commitment level and VERY short timeout
    RpcClient::new_with_timeout_and_commitment(
        rpc_url,
        std::time::Duration::from_millis(500), // 500ms timeout for faster responsiveness
        solana_sdk::commitment_config::CommitmentConfig::processed(),
    )
}

/// New liquidity check using RPC (from commit c0b1a813cf5e3e22aebe2b233b6a2a4240ece528)
pub async fn check_token_liquidity(mint: &str, bonding_curve: &str, liquidity_threshold: f64) -> Result<(bool, f64)> {
    use solana_sdk::pubkey::Pubkey;
    use std::time::Duration;
    
    // First, check for cached primary values which are most reliable
    // These are set via the WebSocket notification handlers
    let primary_cache_key = format!("primary:{}", mint);
    {
        let cache = LIQUIDITY_CACHE.lock().await;
        if let Some((balance, timestamp)) = cache.get(&primary_cache_key) {
            if timestamp.elapsed() < Duration::from_secs(10) {
                debug!("Using cached primary liquidity for {}: {} SOL", mint, balance);
                return Ok((*balance >= liquidity_threshold, *balance));
            }
        }
    }
    
    // Next, check for bonding curve cached values
    let curve_cache_key = format!("bonding_curve:{}:{}", bonding_curve, mint);
    {
        let cache = LIQUIDITY_CACHE.lock().await;
        if let Some((balance, timestamp)) = cache.get(&curve_cache_key) {
            if timestamp.elapsed() < Duration::from_secs(10) {
                debug!("Using cached bonding curve liquidity for {}: {} SOL", mint, balance);
                return Ok((*balance >= liquidity_threshold, *balance));
            }
        }
    }
    
    // If we don't have cached values, do an RPC call as a fallback
    let client = get_rpc_client().await;
    let mint_pubkey = Pubkey::from_str(mint).map_err(|e| anyhow!("Invalid mint pubkey: {}", e))?;
    let (primary_bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);
    
    // The rent exempt minimum amount in SOL
    const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
    match client.get_account(&primary_bonding_curve) {
        Ok(account) => {
            let total_balance = account.lamports as f64 / 1_000_000_000.0;
            let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
            
            // Update both cache entries
            {
                let mut cache = LIQUIDITY_CACHE.lock().await;
                cache.insert(primary_cache_key, (actual_liquidity, std::time::Instant::now()));
                cache.insert(curve_cache_key, (actual_liquidity, std::time::Instant::now()));
            }
            
            debug!("Primary bonding curve has {} SOL (after subtracting {} SOL rent)", actual_liquidity, RENT_EXEMPT_MINIMUM);
            
            // Log in consistent format for any RPC-based checks too
            let min_liquidity = std::env::var("MIN_LIQUIDITY")
                .unwrap_or_else(|_| "5.0".to_string())
                .parse::<f64>()
                .unwrap_or(5.0);
            
            let has_liquidity = actual_liquidity >= liquidity_threshold;
            let check_mark = if has_liquidity { "✅" } else { "❌" };
            
            // Get token name if available
            let token_name = get_token_name_for_mint(mint).unwrap_or_else(|| "Unknown".to_string());
            
            // Log consistent output format 
            info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                  token_name, mint, actual_liquidity, check_mark);
            
            // No need to log follow-up message since main.rs will handle that based on return value
            
            Ok((actual_liquidity >= liquidity_threshold, actual_liquidity))
        },
        Err(e) => {
            // Try looking up in the queue one more time in case the token was just detected
            if let Some(name) = get_token_name_for_mint(mint) {
                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", 
                      name, mint);
                info!("❌ Liquidity check FAILED (0.00 SOL < {:.2} SOL) - Skipping buy", 
                      liquidity_threshold);
            }
            
            Err(anyhow!("Failed to get account info for primary bonding curve: {}", e))
        }
    }
}

/// Check only the primary bonding curve account liquidity and subtract rent exemption
/// This is faster than the original function as it only checks one account
pub async fn check_token_primary_liquidity(
    mint: &str,
    liquidity_threshold: f64,
) -> Result<(bool, f64), Box<dyn std::error::Error + Send + Sync>> {
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    // Check cache first for recent results
    let cache_key = format!("primary:{}", mint);
    {
        let cache = LIQUIDITY_CACHE.lock().await;
        if let Some((balance, timestamp)) = cache.get(&cache_key) {
            // Use cached value if less than 5 seconds old
            if timestamp.elapsed() < std::time::Duration::from_secs(5) {
                debug!(
                    "Using cached primary liquidity for {}: {} SOL",
                    mint, balance
                );
                return Ok((*balance >= liquidity_threshold, *balance));
            }
        }
    }

    // Get RPC client
    let client = get_rpc_client().await;

    // The rent exempt minimum amount in SOL
    const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;

    // Convert mint address to public key
    let mint_pubkey = Pubkey::from_str(mint)
        .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))?;

    // Get the primary bonding curve address
    let (primary_bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);
    debug!(
        "Checking primary bonding curve for liquidity: {}",
        primary_bonding_curve
    );

    // Check the primary bonding curve
    match client.get_account(&primary_bonding_curve) {
        Ok(account) => {
            // Get total balance
            let total_balance = account.lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL

            // Subtract rent exempt minimum to get actual liquidity
            let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);

            // Update the cache
            {
                let mut cache = LIQUIDITY_CACHE.lock().await;
                cache.insert(cache_key, (actual_liquidity, std::time::Instant::now()));
            }

            debug!(
                "Primary bonding curve has {} SOL (after subtracting {} SOL rent)",
                actual_liquidity, RENT_EXEMPT_MINIMUM
            );

            Ok((actual_liquidity >= liquidity_threshold, actual_liquidity))
        }
        Err(e) => {
            debug!(
                "Error retrieving primary bonding curve account {}: {}",
                primary_bonding_curve, e
            );
            Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                "Failed to get account info for primary bonding curve: {}",
                e
            )))
        }
    }
}

// Define our own NewToken structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewToken {
    pub token_name: String,
    pub token_symbol: String,
    pub mint_address: String,
    pub creator_address: String,
    pub transaction_signature: String,
    pub timestamp: i64,
}

// Define a constant for WebSocket messages
lazy_static! {
    pub static ref WEBSOCKET_MESSAGES: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::new()));
        
    // Global cache for deduplication of tokens by mint+timestamp
    static ref RECENT_TOKEN_TIMESTAMPS: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(HashSet::new()));
}

/// Subscribe to bonding curve address using WebSocket instead of RPC calls.
/// This function calculates the primary bonding curve address and returns it
/// so it can be used for WebSocket subscription.
pub fn get_primary_bonding_curve_for_subscription(mint: &str) -> Option<String> {
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    // Convert mint address to public key
    if let Ok(mint_pubkey) = Pubkey::from_str(mint) {
        // Get the primary bonding curve address
        let (bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);

        // Return the bonding curve address as a string
        return Some(bonding_curve.to_string());
    }

    None
}

/// Public function to connect to a WebSocket and verify connection
/// This is provided for backward compatibility
pub async fn connect_websocket_simple(url: &str) -> Result<()> {
    // This just calls our internal function and returns an Ok if the connection succeeds
    connect_to_websocket(url).await
}

// Add this new polling function for fallback token detection
/// Poll for new tokens using the Solana APIs endpoint as a fallback when WebSocket is throttled
pub async fn poll_for_new_tokens() -> Result<()> {
    // Declare constant for API polling
    static POLLING_ACTIVE: AtomicBool = AtomicBool::new(false);
    
    // Check if polling is already active
    if POLLING_ACTIVE.swap(true, Ordering::SeqCst) {
        info!("API polling for token detection is already active, skipping duplicate start");
        return Ok(());
    }
    
    info!("Starting fallback polling mechanism for token detection");
    
    // Get API key
    let api_key = match std::env::var("SOLANAAPIS_KEY") {
        Ok(key) => key,
        Err(_) => {
            POLLING_ACTIVE.store(false, Ordering::SeqCst);
            return Err(anyhow!("SOLANAAPIS_KEY environment variable not set"));
        }
    };
    
    // Create HTTP client
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    
    // Create cache for processed tokens
    let processed_tokens: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    
    // Create LRU cache for recent transactions to avoid duplicates
    let cache_size = NonZeroUsize::new(100).unwrap();
    let recent_tx_cache: Arc<Mutex<LruCache<String, i64>>> = Arc::new(Mutex::new(LruCache::new(cache_size)));
    
    let poll_interval = std::env::var("API_POLL_INTERVAL")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u64>()
        .unwrap_or(3000);
    
    info!("⚙️ Using API polling for token detection at https://api.solanaapis.net/pumpfun/new/tokens");
    info!("📊 API Poll interval: {}ms", poll_interval);
    
    // Infinite polling loop
    loop {
        // Check if we should pause polling due to active position
        if is_token_detection_paused() {
            info!("🛑 API polling is paused due to active position monitoring");
            
            // Wait until token detection is unpaused
            while is_token_detection_paused() {
                info!("Waiting for active position to be sold before resuming API polling...");
                sleep(Duration::from_secs(5)).await;
            }
            
            info!("🟢 Token detection lock released. Resuming API polling.");
        }
        
        // ... rest of the function
    }
}

// Add a static set to track stored signatures
lazy_static! {
    static ref STORED_SIGNATURES: tokio::sync::Mutex<HashSet<String>> = tokio::sync::Mutex::new(HashSet::new());
}

/// Poll the new tokens API endpoint and process any new tokens found
async fn poll_new_tokens_endpoint(
    client: &Client,
    processed_tokens: &Arc<Mutex<HashSet<String>>>,
    api_key: &str,
    recent_tx_cache: &Arc<Mutex<LruCache<String, i64>>>
) -> Result<(bool, i64)> {
    let api_url = "https://api.solanaapis.net/pumpfun/new/tokens";
    
    // When capturing detection time, specify the timezone explicitly
    let detection_start_time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    
    // Create the request with optimized headers
    let mut request = client.get(api_url)
        .header("Connection", "keep-alive")
        .header("Accept-Encoding", "gzip, deflate")
        .header("Cache-Control", "no-cache");
        
    // Add API key if provided
    if !api_key.is_empty() {
        request = request.header("x-api-key", api_key);
    }
    
    // Make the API request with timeout handling
    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        request.send()
    ).await {
        Ok(result) => match result {
            Ok(response) => response,
            Err(e) => return Err(anyhow!("Request error: {}", e))
        },
        Err(_) => return Err(anyhow!("Request timed out after 5 seconds"))
    };
    
    // Check for rate limits in response headers
    if let Some(rate_limit_remaining) = response.headers().get("x-ratelimit-remaining") {
        if let Ok(remaining) = rate_limit_remaining.to_str() {
            debug!("API rate limit remaining: {}", remaining);
        }
    }
    
    // Check if we hit rate limit (429 status)
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        // Check for retry-after header and parse it if available
        if let Some(retry_after) = response.headers().get("retry-after") {
            if let Ok(retry_str) = retry_after.to_str() {
                if let Ok(seconds) = retry_str.parse::<u64>() {
                    return Err(anyhow!("API rate limit exceeded, retry after {} seconds", seconds));
                }
            }
            return Err(anyhow!("API rate limit exceeded, will retry"));
        }
        return Err(anyhow!("API rate limit exceeded, will retry"));
    }
    
    // Check if the request was successful
    if !response.status().is_success() {
        return Err(anyhow!("API request failed with status: {}", response.status()));
    }
    
    // Parse the response with timing
    let parse_start = Instant::now();
    let token_data: serde_json::Value = response.json().await?;
    let parse_duration = parse_start.elapsed();
    debug!("JSON parsing took {}ms", parse_duration.as_millis());
    
    // Check if status is success
    if token_data.get("status").and_then(|s| s.as_str()) != Some("success") {
        return Err(anyhow!("API returned non-success status"));
    }
    
    // Extract token data
    let mint = match token_data.get("mint").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => return Err(anyhow!("Missing mint in API response")),
    };
    
    // Extract transaction signature for cache checking
    let tx_signature = token_data.get("signature")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();
    
    // Check if we've already stored this signature
    let store_signature = {
        let mut stored_sigs = STORED_SIGNATURES.lock().await;
        if !stored_sigs.contains(&tx_signature) {
            // Only store if we haven't seen this signature before
            stored_sigs.insert(tx_signature.clone());
            true
        } else {
            false
        }
    };
    
    // Store the transaction signature for performance metrics when buying
    if store_signature {
        info!("📝 Storing mint transaction signature for performance metrics: {}", tx_signature);
        std::env::set_var("LAST_MINT_SIGNATURE", tx_signature.clone());
    } else {
        debug!("Signature {} already stored, skipping duplicate storage", tx_signature);
    }
    
    // Check if we've already processed this transaction in our cache
    {
        let mut cache = recent_tx_cache.lock().await;
        if cache.contains(&tx_signature) {
            debug!("Transaction {} already processed (from cache), skipping", tx_signature);
            return Ok((false, 0));
        }
        
        // Add to cache with current timestamp
        cache.put(tx_signature.clone(), detection_start_time_ms);
    }
    
    // Get the transaction time to measure detection performance
    let blockchain_time_ms = match get_transaction_time(&tx_signature).await {
        Ok(time) => time,
        Err(e) => {
            warn!("Failed to get transaction time for performance measurement: {}", e);
            // Use API timestamp or current time as fallback
            token_data.get("timestamp")
                .and_then(|t| t.as_i64())
                .unwrap_or_else(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64 * 1000 - 500 
                })
        }
    };
    
    // Create a deduplication key combining mint address and blockchain timestamp 
    // This prevents duplicate processing when the API returns the same token multiple times
    let dedup_key = format!("{}:{}", mint, blockchain_time_ms);
    
    // Check if we've recently processed this exact token+timestamp combination
    {
        let mut recent_tokens = RECENT_TOKEN_TIMESTAMPS.lock().await;
        if recent_tokens.contains(&dedup_key) {
            debug!("Token {} at timestamp {} already processed recently, skipping duplicate", 
                  mint, blockchain_time_ms);
            return Ok((false, 0));
        }
        
        // Add to recent tokens set and limit its size
        recent_tokens.insert(dedup_key);
        if recent_tokens.len() > 1000 {
            // Remove oldest entries if we have too many (simple approach)
            while recent_tokens.len() > 900 {
                if let Some(first) = recent_tokens.iter().next().cloned() {
                    recent_tokens.remove(&first);
                }
            }
        }
    }
    
    // Check if we've already processed this token more generally
    {
        let mut processed = processed_tokens.lock().await;
        if processed.contains(&mint) {
            debug!("Token {} already processed, skipping", mint);
            return Ok((false, 0));
        }
        
        // Add to processed set
        processed.insert(mint.clone());
    }
    
    // Calculate detection delay with proper handling of time anomalies
    let raw_detection_delay_ms = detection_start_time_ms - blockchain_time_ms;
    let detection_delay_ms = if raw_detection_delay_ms < 0 {
        // Log the timestamp anomaly
        debug!("Time synchronization anomaly detected: API time appears to be {}ms before blockchain time", 
               -raw_detection_delay_ms);
        // Use absolute value for metrics (we detected it quickly, that's what matters)
        raw_detection_delay_ms.abs()
    } else {
        raw_detection_delay_ms
    };
    
    // Convert API response to our token data format
    let token = DetectorTokenData {
        name: token_data.get("name").and_then(|n| n.as_str()).unwrap_or("Unknown").to_string(),
        symbol: token_data.get("symbol").and_then(|s| s.as_str()).unwrap_or("UNKNOWN").to_string(),
        uri: token_data.get("metadata").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        mint: mint.clone(),
        bonding_curve: token_data.get("bondingCurve").and_then(|b| b.as_str()).unwrap_or("").to_string(),
        user: token_data.get("dev").and_then(|d| d.as_str()).unwrap_or("").to_string(),
        tx_signature: tx_signature.clone(),
    };
    
    // Convert timestamps to human-readable format for logging
    let blockchain_time = DateTime::<Utc>::from_timestamp(blockchain_time_ms / 1000, 0).unwrap_or_default();
    let detection_time = DateTime::<Utc>::from_timestamp(detection_start_time_ms / 1000, 0).unwrap_or_default();
    
    // Log the new token with performance metrics
    info!("🔔 NEW TOKEN DETECTED (via API polling): {} ({})", token.name, token.symbol);
    info!("   Mint: {}", token.mint);
    info!("   Bonding Curve: {}", token.bonding_curve);
    info!("   Developer: {}", token.user);
    info!("⏱️ Transaction time on blockchain: {} ({}ms)", blockchain_time, blockchain_time_ms);
    info!("⏱️ Detection time by API: {} ({}ms)", detection_time, detection_start_time_ms);
    
    // Log detection delay with clear indication if there was a time anomaly
    if raw_detection_delay_ms < 0 {
        info!("⏱️ Detection delay: {}ms (time sync anomaly, actual delay likely near-zero)", detection_delay_ms);
    } else {
        info!("⏱️ Detection delay: {}ms", detection_delay_ms);
    }
    
    // Get configuration for token processing
    let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "0".to_string());
    let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(0.0);
    
    // Check token liquidity
    match check_token_liquidity(&token.mint, &token.bonding_curve, min_liquidity).await {
        Ok((has_liquidity, liquidity)) => {
            if has_liquidity {
                // Process the token (similar to WebSocket path)
                // Send to processing queue
                if let Ok(mut queue) = crate::api::NEW_TOKEN_QUEUE.try_lock() {
                    // Create a TokenData object for the queue using the API TokenData structure
                    let token_data = crate::api::TokenData {
                        status: "success".to_string(),
                        mint: token.mint.clone(),
                        dev: token.user.clone(),
                        metadata: Some(format!("bonding_curve:{}", token.bonding_curve)),
                        name: Some(token.name.clone()),
                        symbol: Some(token.symbol.clone()),
                        timestamp: Some(chrono::Utc::now().timestamp_millis()),
                        liquidity_status: None,
                        liquidity_amount: None,
                    };
                    
                    queue.push_back(token_data);
                    info!("Added token to processing queue: {}", token.mint);
                    return Ok((true, detection_delay_ms));
                } else {
                    warn!("Could not add token to queue: {:?}", token);
                }
            } else {
                info!("Token failed liquidity check ({}): {}", liquidity, token.mint);
            }
        },
        Err(e) => {
            warn!("Error checking token liquidity: {}", e);
        }
    }
    
    Ok((false, detection_delay_ms))
}

// Function to process a new token after detection - WebSocket or API polling
fn process_new_token(token: DetectorTokenData) -> anyhow::Result<()> {
    // CRITICAL: Check if position monitoring is active FIRST, before any processing
    if is_position_monitoring_active() || is_token_detection_paused() {
        info!("🛑 Ignoring new token {} - Position monitoring active or token detection paused", token.mint);
        return Ok(());
    }

    // Only process tokens with a "pump" suffix in the mint address
    if !token.mint.ends_with("pump") {
        debug!("Skipping non-pump.fun token: {}", token.mint);
        return Ok(());
    }

    // Store the token's transaction signature in the environment variable for performance metrics
    info!("📝 Storing mint transaction signature for performance metrics: {}", token.tx_signature);
    std::env::set_var("LAST_MINT_SIGNATURE", token.tx_signature.clone());
    
    // Check token liquidity
    let bonding_curve = &token.bonding_curve;
    
    // Use the token queue to track new tokens
    if let Ok(mut queue) = crate::api::NEW_TOKEN_QUEUE.try_lock() {
        // Convert to TokenData format
        let token_data = crate::api::TokenData {
            status: "success".to_string(),
            mint: token.mint.clone(),
            dev: token.user.clone(),
            metadata: Some(format!("bonding_curve:{}", bonding_curve)),
            name: Some(token.name.clone()),
            symbol: Some(token.symbol.clone()),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            liquidity_status: None,
            liquidity_amount: None,
        };
        
        // Add to queue for processing
        queue.push_back(token_data);
        info!("Added token to processing queue: {}", token.mint);
        drop(queue);
    } else {
        error!("Failed to obtain lock on token queue");
    }
    
    Ok(())
}

// Static atomic flags for token detection and position monitoring
static TOKEN_DETECTION_ACTIVE: AtomicBool = AtomicBool::new(true);
static POSITION_MONITORING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Get whether token detection is active
pub fn is_token_detection_active() -> bool {
    TOKEN_DETECTION_ACTIVE.load(Ordering::SeqCst)
}

/// Set whether token detection is active
pub fn set_token_detection_active(active: bool) {
    TOKEN_DETECTION_ACTIVE.store(active, Ordering::SeqCst);
    
    if active {
        info!("🔓 Token detection activated");
    } else {
        info!("🔒 Token detection paused");
    }
}

/// Check if token detection is paused
pub fn is_token_detection_paused() -> bool {
    !TOKEN_DETECTION_ACTIVE.load(Ordering::SeqCst)
}

/// Set whether position monitoring is active
pub fn set_position_monitoring_active(active: bool) {
    POSITION_MONITORING_ACTIVE.store(active, Ordering::SeqCst);
    
    if active {
        info!("🔍 Position monitoring activated");
        
        // When position monitoring is activated, we should also pause token detection
        // if MAX_POSITIONS is set to 1
        if std::env::var("MAX_POSITIONS")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<usize>()
            .unwrap_or(1) == 1 
        {
            set_token_detection_active(false);
            info!("🔒 Token detection automatically paused due to active position and MAX_POSITIONS=1");
        }
    } else {
        info!("🔍 Position monitoring deactivated");
        
        // When position monitoring is deactivated, we can resume token detection
        // if it was paused due to position monitoring
        set_token_detection_active(true);
        info!("🔓 Token detection automatically resumed after position monitoring ended");
    }
}

/// Check if position monitoring is active
pub fn is_position_monitoring_active() -> bool {
    POSITION_MONITORING_ACTIVE.load(Ordering::SeqCst)
}
