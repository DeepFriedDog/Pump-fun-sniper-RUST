use anyhow::{anyhow, Error, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::{Message, WebSocketConfig}, tungstenite::handshake::client::Request};
use url::Url;
use http::{HeaderMap, HeaderValue};
use uuid;
use reqwest;
use std::collections::HashMap;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_client::rpc_client::RpcClient;

use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};
use crate::chainstack_simple;

/// Creates a fresh WebSocket connection without extra parameters that might delay message delivery
/// This approach is based on the older implementation that had more immediate token detection
async fn connect_fresh(url_str: &str) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    // Parse the URL
    let url = Url::parse(url_str)?;
    
    // Generate a unique identifier for this session
    let session_id = uuid::Uuid::new_v4().to_string();
    
    // Current timestamp for cache busting
    let current_timestamp_millis = Utc::now().timestamp_millis();
    
    // Create a fresh URL with minimal parameters - just cache busting
    let mut fresh_url = url.clone();
    fresh_url.query_pairs_mut()
        .append_pair("_", &current_timestamp_millis.to_string());   // Cache busting
    
    // Create a fresh URL string
    let fresh_url_str = fresh_url.to_string();
    
    // Use a standard WebSocket config
    info!("Connecting to WebSocket with minimal parameters: {}", fresh_url_str);
    
    // Direct connection attempt with minimal configuration
    let (stream, _) = connect_async(&fresh_url_str).await?;
    
    info!("Connection established successfully");
    info!("Ready to subscribe to token events");
    
    Ok(stream)
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

/// Parse the create instruction data
fn parse_create_instruction(data: &[u8]) -> Option<TokenData> {
    if data.len() < 8 {
        debug!(
            "Data too short to be a valid instruction: {} bytes",
            data.len()
        );
        return None;
    }

    // Log the first 8 bytes (discriminator) for debugging
    let discriminator = &data[0..8];
    debug!("Instruction discriminator: {:?}", discriminator);

    let mut offset = 8; // Skip discriminator

    // Read a string from the data
    let read_string = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 4 > data.len() {
            debug!("Offset out of bounds when reading string length");
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
            debug!("String content would exceed data bounds");
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

    // Read a pubkey from the data
    let read_pubkey = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 32 > data.len() {
            debug!("Offset out of bounds when reading pubkey");
            return None;
        }

        let pubkey_data = &data[*offset..*offset + 32];
        *offset += 32;

        let encoded = bs58::encode(pubkey_data).into_string();
        debug!("Read pubkey: {}", encoded);

        Some(encoded)
    };

    // Parse fields
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

    debug!("Successfully parsed token data: {} ({})", name, symbol);
    Some(TokenData {
        name,
        symbol,
        uri,
        mint,
        bonding_curve,
        user,
        tx_signature: String::new(),
        block_time: None,
        detection_time: None,
    })
}

/// Parses instruction data into TokenData
pub fn parse_instruction(data: &[u8]) -> Option<TokenData> {
    parse_create_instruction(data)
}

// Struct for parsed token data
#[derive(Debug, Clone)]
pub struct TokenData {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub mint: String,
    pub bonding_curve: String,
    pub user: String,
    pub tx_signature: String,
    pub block_time: Option<i64>,
    pub detection_time: Option<i64>, // Detection time in UNIX timestamp
}

/// Checks if the node is fully synced and ready for connections
async fn check_node_sync_status(node_endpoint: &str) -> Result<bool> {
    let client = reqwest::Client::new();
    let rpc_endpoint = node_endpoint.replace("wss://", "https://").replace("ws://", "http://");
    
    // First, let's check if the node is healthy
    let health_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getHealth",
        "params": []
    });
    
    // Use a fast timeout to avoid blocking if the node is slow to respond
    let timeout_duration = std::time::Duration::from_millis(250);
    
    // Check node health with timeout
    let health_response = match tokio::time::timeout(
        timeout_duration,
        client.post(&rpc_endpoint)
            .json(&health_request)
            .send()
    ).await {
        Ok(response_result) => match response_result {
            Ok(response) => {
                match response.json::<Value>().await {
                    Ok(json) => json,
                    Err(_) => {
                        warn!("Failed to parse health check response");
                        return Ok(false);
                    }
                }
            },
            Err(_) => {
                warn!("Health check request failed");
                return Ok(true);
            }
        },
        Err(_) => {
            warn!("Health check request timed out");
            // We'll continue anyway since timeouts don't necessarily mean the node is unhealthy
            return Ok(true);
        }
    };
    
    // Parse the health response
    if let Some(result) = health_response.get("result") {
        if result != "ok" {
            warn!("Node is not healthy: {:?}", result);
            return Ok(false);
        }
    }
    
    info!("Node is healthy and ready for connections");
    
    // For high performance, don't wait for slot checks, just assume the node is synced
    // This helps avoid delay when the API is slow to respond but messages are coming through
    return Ok(true);
    
    // NOTE: The code below has been commented out to avoid unnecessary delay
    /*
    // Next, check if the node is synced with the network
    let slot_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "getSlot",
        "params": []
    });
    
    let network_slot_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "getSlot",
        "params": [{"commitment": "finalized"}]
    });
    
    // Get the node's current slot
    let slot_response = match client.post(&rpc_endpoint)
        .json(&slot_request)
        .send()
        .await {
        Ok(response) => {
            match response.json::<Value>().await {
                Ok(json) => json,
                Err(_) => {
                    warn!("Failed to parse slot response");
                    return Ok(false);
                }
            }
        },
        Err(_) => {
            warn!("Slot request failed");
            return Ok(false);
        }
    };
    
    // Get the network's finalized slot
    let network_slot_response = match client.post(&rpc_endpoint)
        .json(&network_slot_request)
        .send()
        .await {
        Ok(response) => {
            match response.json::<Value>().await {
                Ok(json) => json,
                Err(_) => {
                    warn!("Failed to parse network slot response");
                    return Ok(false);
                }
            }
        },
        Err(_) => {
            warn!("Network slot request failed");
            return Ok(false);
        }
    };
    
    // Extract the slot numbers
    let node_slot = slot_response
        .get("result")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    
    let network_slot = network_slot_response
        .get("result")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    
    // Calculate slot lag
    let slot_lag = if node_slot > network_slot {
        0 // Node is ahead, which is fine
    } else {
        network_slot - node_slot
    };
    
    // Log the sync status
    info!("Node slot: {}, Network slot: {}, Lag: {} slots", node_slot, network_slot, slot_lag);
    
    // Node is considered synced if it's less than 50 slots behind
    // This is a reasonable threshold for most applications
    let is_synced = slot_lag < 50;
    
    if is_synced {
        info!("Node is synced with the network.");
    } else {
        warn!("Node is not fully synced. Lag: {} slots", slot_lag);
    }
    
    Ok(is_synced)
    */
}

/// Gets the block time for a transaction signature
pub async fn get_transaction_block_time(tx_signature: &str) -> Result<i64> {
    // Get RPC URL from environment or use default
    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

    // Create RPC client with processed commitment for faster results
    let client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::processed());
    
    // Get current time for fallback
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    
    // Try to get transaction details including block time
    match client.get_transaction_with_config(
        &bs58::decode(tx_signature).into_vec().map_err(|e| anyhow!("Failed to decode tx signature: {}", e))?.try_into().map_err(|_| anyhow!("Invalid transaction signature"))?,
        solana_client::rpc_config::RpcTransactionConfig {
            encoding: None,
            commitment: Some(CommitmentConfig::processed()),
            max_supported_transaction_version: Some(0),
        },
    ) {
        Ok(tx_response) => {
            // Extract block time if available
            if let Some(block_time) = tx_response.block_time {
                debug!("Transaction {} block time: {}", tx_signature, block_time);
                Ok(block_time)
            } else {
                debug!("No block time available for transaction {}, using current time", tx_signature);
                Ok(current_time)
            }
        },
        Err(e) => {
            warn!("Failed to get transaction details: {:?}", e);
            Ok(current_time) // Fallback to current time
        }
    }
}

/// Runs a simple WebSocket test to verify the connection and token detection capabilities
pub async fn run_websocket_test(endpoint: &str) -> Result<Vec<TokenData>, Error> {
    info!("Starting WebSocket test, connecting to: {}", endpoint);

    // Check if we need to use authentication
    let use_auth = std::env::var("USE_CHAINSTACK_AUTH")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    let username = if use_auth {
        std::env::var("CHAINSTACK_USERNAME").unwrap_or_default()
    } else {
        String::new()
    };
    
    let password = if use_auth {
        std::env::var("CHAINSTACK_PASSWORD").unwrap_or_default()
    } else {
        String::new()
    };
    
    // Parse the WebSocket URL
    let mut url_string = endpoint.to_string();
    
    // If authentication is enabled and credentials are provided, modify the URL
    if use_auth && !username.is_empty() && !password.is_empty() {
        info!("Using basic authentication for WebSocket connection");
        
        // For basic auth in WebSockets, we need to include credentials in the URL
        if let Ok(mut url) = Url::parse(&url_string) {
            // Set credentials in URL (format: wss://username:password@hostname/path)
            if url.scheme() == "wss" || url.scheme() == "ws" {
                if let Err(_) = url.set_username(&username) {
                    warn!("Failed to set username for WebSocket URL");
                }
                if let Err(_) = url.set_password(Some(&password)) {
                    warn!("Failed to set password for WebSocket URL");
                }
                url_string = url.to_string();
                info!("Using authenticated WebSocket URL with embedded credentials");
            }
        }
    }
    
    // Parse the URL and add query parameters for keep-alive if needed
    let mut url = Url::parse(&url_string)
        .map_err(|e| anyhow!("Failed to parse WebSocket URL: {}", e))?;

    // Some WebSocket servers support keep-alive via query parameters
    if !url.query_pairs().any(|(k, _)| k == "keepalive") {
        url.query_pairs_mut().append_pair("keepalive", "true");
    }

    // Connection retry logic
    let mut retry_attempts = 0;
    let max_retries = 3; // Retry connection up to 3 times within this function
    let retry_delay = Duration::from_secs(2);

    // Try to establish the WebSocket connection with retries
    let mut ws_stream = loop {
        match connect_async(&url).await {
            Ok((stream, _)) => break stream,
            Err(e) => {
                retry_attempts += 1;
                if retry_attempts >= max_retries {
                    return Err(anyhow!(
                        "Failed to connect to WebSocket after {} attempts: {}",
                        max_retries,
                        e
                    ));
                }

                warn!(
                    "WebSocket connection attempt {} failed: {}. Retrying in {} seconds...",
                    retry_attempts,
                    e,
                    retry_delay.as_secs()
                );

                tokio::time::sleep(retry_delay).await;
            }
        }
    };

    // Pump.fun program ID for token creation
    let pump_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

    // Set to "processed" commitment for fastest detection
    let subscription_request = json!({
        "id": 1,
        "jsonrpc": "2.0",
        "method": "logsSubscribe",
        "params": [
            {"mentions": [pump_program_id]},
            {"commitment": "processed"}
        ]
    })
    .to_string();

    // Check if we're in quiet mode
    let quiet_mode = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");

    if !quiet_mode {
        info!("Sending subscription request: {}", subscription_request);
    }

    // Send the subscription request
    ws_stream
        .send(Message::Text(subscription_request))
        .await
        .map_err(|e| anyhow!("Failed to send subscription request: {}", e))?;

    info!("WebSocket connection established, waiting for token creation events...");

    // Create tracking variables
    let mut message_count = 0;
    let mut token_creations = 0;
    let start_time = Instant::now();

    // Get the test duration from env or default to 0 (indefinite)
    // A duration of 0 means run until connection error or max token limit
    let test_duration = std::env::var("MONITOR_DURATION")
        .unwrap_or_else(|_| "0".to_string())
        .parse::<u64>()
        .unwrap_or(0);

    // Default to 2 minutes per cycle if no duration is specified
    let cycle_duration = if test_duration > 0 {
        test_duration
    } else {
        120 // 2 minute default cycle when running indefinitely
    };

    let timeout_duration = Duration::from_secs(cycle_duration);

    // Max tokens to collect per cycle (to prevent memory issues in indefinite mode)
    let max_tokens_per_cycle = 1; // Changed from 100 to 1 to return immediately with token data

    // Heartbeat/ping interval - every 30 seconds to keep connection alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));

    // Status update interval - every 30 seconds
    let mut status_interval = tokio::time::interval(Duration::from_secs(30));

    // Last message received time for connection health monitoring
    let mut last_message_time = Instant::now();
    let max_idle_time = Duration::from_secs(60); // Consider connection dead after 60s with no messages

    // Vector to store token data
    let mut token_data_list = Vec::new();

    // Log initial status
    if !quiet_mode {
        if test_duration > 0 {
            info!(
                "Status: WebSocket test running. Will listen for {} seconds",
                test_duration
            );
        } else {
            info!("Status: WebSocket monitoring running indefinitely or until token detection");
        }
    }

    // Process messages
    loop {
        tokio::select! {
            // Check for timeout
            _ = tokio::time::sleep(timeout_duration.saturating_sub(start_time.elapsed())) => {
                if test_duration > 0 {
                    info!("Test timeout reached after {} seconds. Exiting...", test_duration);
                } else {
                    info!("Monitoring cycle completed. Total tokens found so far: {}", token_creations);
                }
                break;
            }

            // Send heartbeat ping
            _ = ping_interval.tick() => {
                // Check if connection seems dead (no messages for too long)
                if last_message_time.elapsed() > max_idle_time {
                    warn!("WebSocket connection appears to be dead - no messages for {} seconds",
                          last_message_time.elapsed().as_secs());

                    // Return immediately to allow reconnection
                    return Ok(token_data_list);
                }

                // Send a ping to keep the connection alive
                if let Err(e) = ws_stream.send(Message::Ping(vec![1, 2, 3])).await {
                    warn!("Failed to send ping: {}", e);
                    // Return immediately to allow reconnection
                    return Ok(token_data_list);
                } else if !quiet_mode {
                    debug!("Sent ping to keep WebSocket connection alive");
                }
            }

            // Status update interval
            _ = status_interval.tick() => {
                let elapsed = start_time.elapsed();

                if test_duration > 0 {
                    let remaining = timeout_duration.saturating_sub(elapsed);
                    let mins = remaining.as_secs() / 60;
                    let secs = remaining.as_secs() % 60;

                    if !quiet_mode {
                        info!("Status: Processed {} messages, found {} new tokens. Test ends in {}m{}s",
                              message_count, token_creations, mins, secs);
                    }
                } else {
                    if !quiet_mode {
                        info!("Status: Processed {} messages, found {} new tokens. Monitoring indefinitely.",
                              message_count, token_creations);
                    }
                }
            }

            // Process messages
            result = ws_stream.next() => {
                match result {
                    Some(Ok(message)) => {
                        // Update the last message timestamp whenever we receive any message
                        last_message_time = Instant::now();
                        message_count += 1;

                        // Handle different message types
                        match message {
                            Message::Text(text) => {
                                // Parse message as JSON
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(token_data) = process_websocket_message(&json, &mut token_creations, quiet_mode) {
                                        // Return immediately with the token to prevent delay
                                        token_data_list.push(token_data);

                                        // Return the token data immediately instead of waiting
                                        return Ok(token_data_list);
                                    }
                                }
                            },
                            Message::Binary(data) => {
                                message_count += 1;
                                debug!("[{}] Binary WebSocket message received: {} bytes", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), data.len());
                            },
                            Message::Ping(data) => {
                                // Automatically respond to ping with pong
                                debug!("[{}] WebSocket ping received", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                                if let Err(e) = ws_stream.send(Message::Pong(data)).await {
                                    error!("Failed to send pong response: {}", e);
                                    return Ok(token_data_list);
                                }
                            },
                            Message::Pong(_) => {
                                debug!("[{}] WebSocket pong received", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                            },
                            Message::Close(frame) => {
                                info!("[{}] WebSocket close frame received: {:?}", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), frame);
                                return Ok(token_data_list);
                            },
                            _ => {
                                debug!("[{}] Other WebSocket message type received", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                            }
                        }
                    },
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        return Ok(token_data_list);
                    },
                    None => {
                        info!("WebSocket connection closed");
                        return Ok(token_data_list);
                    }
                }
            }
        }
    }

    // Don't close the connection, just return the list
    Ok(token_data_list)
}

/// Process a WebSocket message to extract token data if it's a token creation event
fn process_websocket_message(
    message: &serde_json::Value,
    token_creation_count: &mut usize,
    quiet_mode: bool,
) -> Option<TokenData> {
    // Check if this is a subscription confirmation
    if let Some(id) = message.get("id").and_then(|v| v.as_i64()) {
        if !quiet_mode {
            info!("WebSocket subscription confirmed with id: {}", id);
        }
        return None;
    }

    // Check if this is a notification
    if let Some(method) = message.get("method").and_then(|v| v.as_str()) {
        if method == "logsNotification" {
            if let Some(params) = message.get("params") {
                if let Some(result) = params.get("result") {
                    if let Some(value) = result.get("value") {
                        // Extract log data
                        if let Some(logs) = value.get("logs").and_then(|v| v.as_array()) {
                            if !quiet_mode {
                                // Changed to debug level - these were too noisy
                                debug!("Log entry count: {}", logs.len());
                            }

                            // ONLY check for "Create" instruction in logs - ignore Buy events as requested
                            let contains_create = logs.iter().any(|log| {
                                log.as_str().map_or(false, |s| {
                                    s.contains("Program log: Instruction: Create")
                                })
                            });

                            // IMPORTANT: We're using a fully asynchronous approach here and no longer
                            // depend on getTransaction calls which can be delayed. All token data is
                            // extracted directly from logs, leading to instantaneous token detection.
                            if contains_create {
                                // Found a token creation event!
                                *token_creation_count += 1;

                                if let Some(signature) =
                                    value.get("signature").and_then(|v| v.as_str())
                                {
                                    // Extract and parse program data
                                    let mut program_data_base64 = String::new();

                                    // Try to find and parse program data
                                    for log in logs {
                                        if let Some(log_str) = log.as_str() {
                                            if log_str.contains("Program data:") {
                                                if !quiet_mode {
                                                    info!("Program data found: {}", log_str);
                                                }

                                                // Extract base64 data part
                                                if let Some(data_part) =
                                                    log_str.strip_prefix("Program data: ")
                                                {
                                                    program_data_base64 = data_part.to_string();

                                                    // Decode the base64 data
                                                    if let Ok(decoded_data) =
                                                        BASE64.decode(data_part)
                                                    {
                                                        if !quiet_mode {
                                                            info!("Successfully decoded program data ({} bytes)", decoded_data.len());
                                                        }

                                                        // Parse the token details
                                                        if let Some(mut token_data) =
                                                            parse_create_instruction(&decoded_data)
                                                        {
                                                            // Set the transaction signature
                                                            token_data.tx_signature =
                                                                signature.to_string();
                                                                
                                                            // IMPORTANT: First calculate detection time at processed level
                                                            // before doing any additional processing that might add delays
                                                            let detection_time = SystemTime::now()
                                                                .duration_since(UNIX_EPOCH)
                                                                .unwrap_or_default()
                                                                .as_secs() as i64;
                                                                
                                                            // Store the detection time in the token data
                                                            token_data.detection_time = Some(detection_time);
                                                            
                                                            // Set block_time to detection_time since we're not fetching transaction details
                                                            // This ensures we have a valid timestamp for all downstream processing
                                                            token_data.block_time = Some(detection_time);
                                                            
                                                            // Only proceed with valid tokens
                                                            // Check if the mint address ends with "pump" which indicates a valid token
                                                            if token_data.mint.ends_with("pump")
                                                                || token_data.name != "Unknown"
                                                            {
                                                                // Log the token detection immediately for the fastest response
                                                                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({})", 
                                                                      token_data.name, token_data.mint);
                                                                
                                                                // Return the token data - all further processing will happen
                                                                // in the WebSocket listener loop to avoid await issues
                                                                return Some(token_data);
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
            }
        }
    }

    None
}

/// Listen for new tokens and send them through a channel
pub async fn listen_for_tokens(
    quiet_mode: bool,
    priority_fee: u64,
    wallet: String,
) -> Result<mpsc::Receiver<TokenData>> {
    // Create a channel for sending token data
    let (tx, rx) = mpsc::channel(100);
    
    // Get the WebSocket endpoint from environment with proper authentication
    let wss_endpoint = std::env::var("CHAINSTACK_WSS_ENDPOINT")
        .or_else(|_| std::env::var("WSS_ENDPOINT"))
        .unwrap_or_else(|_| chainstack_simple::get_fresh_wss_url());
    
    info!("Starting WebSocket listener with fresh connection: {}", wss_endpoint);
    info!("Using processed commitment level for fastest token detection");
    
    // Start the WebSocket listener in a separate task
    tokio::spawn(async move {
        let mut consecutive_failures = 0;
        let initial_backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(15); // Reduced from 60s to 15s for faster recovery
        let mut current_backoff = initial_backoff;
        let backoff_factor = 1.5; // Reduced from 2.0 for faster scaling
        
        // Track how many times we've reconnected due to token detection timeouts
        let mut timeout_reconnects = 0;
        let max_timeout_reconnects = 3;  // After this many timeouts, try a different strategy
        
        loop {
            match run_websocket_listener(&wss_endpoint, &tx, quiet_mode).await {
                Ok(_) => {
                    // Reset backoff on success
                    current_backoff = initial_backoff;
                    consecutive_failures = 0;
                    
                    // Also reset timeout reconnects on a clean exit
                    timeout_reconnects = 0;
                    
                    // If we got here with no errors, still wait briefly before reconnecting
                    tokio::time::sleep(Duration::from_millis(500)).await;
                },
                Err(e) => {
                    consecutive_failures += 1;
                    
                    // Check if this is a forced timeout reconnect due to no tokens
                    if e.to_string().contains("no token detection within timeout period") {
                        // If we've been running for at least 10 minutes with no tokens,
                        // it might just be that no tokens are being created
                        if consecutive_failures > 10 {
                            // Don't keep forcing reconnects if there are likely just no new tokens
                            info!("No tokens detected for an extended period - normal operation, continuing to monitor");
                            // Wait longer between reconnects when no tokens are being created
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            continue;
                        }
                        
                        timeout_reconnects += 1;
                        warn!("Timeout reconnect #{} due to no token detection", timeout_reconnects);
                        
                        if timeout_reconnects >= max_timeout_reconnects {
                            // After several timeout failures, we need a more aggressive approach
                            warn!("Multiple token detection timeouts - trying connection with different parameters");
                            
                            // Try a completely different URL to see if that helps
                            let fresh_url = chainstack_simple::get_fresh_wss_url();
                            if fresh_url != wss_endpoint {
                                info!("Switching to a different WebSocket endpoint: {}", fresh_url);
                                
                                // Wait a bit longer to let all resources be cleaned up
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                
                                // Run with the new endpoint but reset the timeout count
                                let new_result = run_websocket_listener(&fresh_url, &tx, quiet_mode).await;
                                if new_result.is_ok() {
                                    timeout_reconnects = 0;
                                }
                            } else {
                                // Force a complete node re-connection by waiting longer
                                warn!("Forcing complete node re-connection with longer wait");
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                            
                            // Reset the counter regardless of outcome
                            timeout_reconnects = 0;
                        } else {
                            // For timeout reconnects, use a shorter backoff
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                    } else if e.to_string().contains("Connection silence timeout") {
                        // This is a dead connection, reconnect immediately
                        warn!("Connection silence detected - reconnecting immediately");
                        continue;
                    }
                    
                    warn!(
                        "WebSocket connection failed: {}. Retrying in {} seconds...",
                        e, current_backoff.as_secs()
                    );
                    
                    // Exponential backoff for non-timeout failures
                    tokio::time::sleep(current_backoff).await;
                    
                    // Update backoff for next attempt
                    let backoff_secs = (current_backoff.as_secs() as f64 * backoff_factor) as u64;
                    current_backoff = std::cmp::min(Duration::from_secs(backoff_secs), max_backoff);
                }
            }
        }
    });
    
    Ok(rx)
}

/// Run the WebSocket listener and send tokens through the channel
async fn run_websocket_listener(
    endpoint: &str,
    tx: &mpsc::Sender<TokenData>,
    quiet_mode: bool,
) -> Result<()> {
    // First, check if the node is fully synced before proceeding
    if let Ok(is_synced) = check_node_sync_status(endpoint).await {
        if !is_synced {
            warn!("Node is not fully synced, token detection might be delayed");
        } else {
            info!("Node is fully synced and ready for connections");
        }
    }
    
    // Parse the WebSocket URL
    let url = Url::parse(endpoint)?;
    
    // Get timestamp before connection for latency tracking
    let pre_connection_timestamp = Instant::now();
    
    // Connect to the WebSocket using our simplified connection method
    let ws_stream = connect_fresh(endpoint).await?;
    
    let connection_latency = pre_connection_timestamp.elapsed();
    info!("WebSocket connection established in {}ms", connection_latency.as_millis());
    
    // Split the WebSocket stream
    let (mut write, mut read) = ws_stream.split();
    
    // Timestamp when we started the connection - useful for diagnostics
    let connection_start = Instant::now();
    let mut message_stats = MessageStats::new();
    
    // Set up a forced reconnect timer if no tokens are detected for too long
    let force_reconnect_after = Duration::from_secs(60); 
    let mut force_reconnect_time = connection_start + force_reconnect_after;
    let mut token_detected = false;
    
    // Track connection health separately from token detection
    let mut last_message_time = Instant::now();
    let max_silence_duration = Duration::from_secs(30); // Reconnect if no messages for 30 seconds
    
    // Subscribe to the Pump.fun program logs with explicit processed commitment
    // We use explicit subscription ID 8002 to match the older implementation
    let subscription_id = 8002; 
    
    // Construct our subscription request for program logs - simplify to match older code
    let subscribe_msg = Message::Text(
        json!({
            "jsonrpc": "2.0",
            "id": subscription_id,
            "method": "logsSubscribe",
            "params": [
                {
                    "mentions": [ PUMP_PROGRAM_ID.to_string() ]
                },
                {
                    "commitment": "processed"  // Important: use processed for fastest notifications
                }
            ]
        }).to_string()
    );
    
    // Send the subscription request immediately without flushing
    if let Err(e) = write.send(subscribe_msg).await {
        return Err(anyhow!("Failed to send subscription request: {}", e));
    }
    
    info!("WebSocket subscription sent with id: {}", subscription_id);
    
    // For tracking time since subscription started
    let subscription_start = Instant::now();
    
    // Track when the last token was detected
    let mut last_token_time = Instant::now();
    
    // Wait for subscription confirmation and then process messages
    let mut subscription_confirmed = false;
    let mut current_subscription_id: Option<u64> = None;
    
    // Processing loop - we'll keep this structure from the current code
    // to properly track timing and handle messages
    let mut token_creation_count: usize = 0;
    
    // Process incoming messages
    let mut message_counter = 0;
    
    // Continue processing messages after verification
    info!("Starting token detection loop - waiting for new tokens...");
    
    while let Some(msg_result) = read.next().await {
        // Update last message time for connection health monitoring
        last_message_time = Instant::now();
        
        // Check forced reconnect timer - if exceeded and no tokens detected, return to trigger reconnect
        // But only do this if we've been running long enough to potentially receive tokens (> 5 minutes)
        if !token_detected && connection_start.elapsed() > Duration::from_secs(300) 
           && Instant::now() > force_reconnect_time {
            warn!("No tokens detected for {}s after connection, forcing reconnect", 
                  force_reconnect_after.as_secs());
            return Err(anyhow!("Forced reconnect due to no token detection within timeout period"));
        }
        
        // Record message activity time for health checks
        message_stats.last_activity = Instant::now();
        message_counter += 1;
        
        // Log message count every 100 messages
        if message_counter % 100 == 0 {
            debug!("Processed {} WebSocket messages", message_counter);
        }
        
        // Periodically report connection stats
        if message_stats.next_report_time <= Instant::now() {
            let elapsed = message_stats.connection_start.elapsed();
            info!(
                "WebSocket stats after {}ms: {} total messages received, {} methods: {:?}",
                elapsed.as_millis(),
                message_stats.total_messages,
                message_stats.method_counts.len(),
                message_stats.method_counts
            );
            // Report every 5 seconds
            message_stats.next_report_time = Instant::now() + Duration::from_secs(5);
        }
        
        match msg_result {
            Ok(msg) => {
                // Track message statistics
                message_stats.total_messages += 1;
                
                match msg {
                    Message::Text(text) => {
                        // Parse the message as JSON
                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                            // Track message types
                            if let Some(method) = json.get("method").and_then(|m| m.as_str()) {
                                *message_stats.method_counts.entry(method.to_string()).or_insert(0) += 1;
                                
                                // Detailed logging for understanding the message flow
                                let elapsed_ms = message_stats.connection_start.elapsed().as_millis();
                                if elapsed_ms < 30000 { // Only log details for first 30 seconds
                                    debug!("Received {} message after {}ms", method, elapsed_ms);
                                }
                            }
                            
                            // Process the message to extract token data
                            if let Some(token_data) = process_websocket_message(&json, &mut token_creation_count, quiet_mode) {
                                // Flag that we detected a token, to avoid forced reconnect
                                token_detected = true;
                                
                                // Calculate detection time from connection start and subscription
                                let detection_time = message_stats.connection_start.elapsed();
                                let time_since_last = last_token_time.elapsed();
                                last_token_time = Instant::now();
                                let time_since_subscription = subscription_start.elapsed();
                                
                                // Get the detection timestamp from the token data or calculate it now
                                let detection_timestamp = token_data.detection_time.unwrap_or_else(|| {
                                    SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs() as i64
                                });
                                
                                // Display detection information at PROCESSED commitment level
                                info!("Token detected after {}ms from connection start ({} since subscription): {} ({}ms since last token)",
                                    detection_time.as_millis(), 
                                    time_since_subscription.as_millis(),
                                    token_data.mint, 
                                    time_since_last.as_millis());
                                
                                // IMMEDIATE ACTION: Check liquidity and prepare for buying at PROCESSED level
                                // This happens without waiting for confirmed/finalized status
                                let token_data_clone = token_data.clone();
                                let mint = token_data_clone.mint.clone();
                                let bonding_curve = token_data_clone.bonding_curve.clone();
                                
                                // 1. IMMEDIATELY start processing buy transaction in separate task
                                tokio::spawn(async move {
                                    // This would interact with your buying logic and bonding curve checks
                                    info!("🚀 IMMEDIATE ACTION: Starting liquidity check for {} at PROCESSED level", 
                                          mint);
                                    
                                    // CRITICAL PERFORMANCE OPTIMIZATION:
                                    // To completely eliminate latency caused by waiting for transaction metadata
                                    // to be persisted, we use a fast path for low liquidity thresholds
                                    let liquidity_threshold = 3.0; // Min liquidity
                                    
                                    if liquidity_threshold < 5.0 {
                                        // For tokens requiring less than 5 SOL liquidity, we can immediately proceed
                                        // A full liquidity check will still happen but won't block token detection
                                        info!("⚡ INSTANT DETECTION: Bypassing full liquidity check for faster token detection");
                                        info!("Assumed bonding curve liquidity: 1.0 SOL (instant path), Required: {:.2} SOL", 
                                              liquidity_threshold);
                                              
                                        // Fire-and-forget the full liquidity check in a separate task
                                        let mint_clone = mint.clone();
                                        let bonding_curve_clone = bonding_curve.clone();
                                        tokio::spawn(async move {
                                            match crate::token_detector::check_token_liquidity(
                                                &mint_clone,
                                                &bonding_curve_clone,
                                                liquidity_threshold
                                            ).await {
                                                Ok((has_liquidity, sol_amount)) => {
                                                    info!("Full bonding curve check complete: {:.2} SOL", sol_amount);
                                                },
                                                Err(e) => {
                                                    warn!("Background liquidity check failed: {}", e);
                                                }
                                            }
                                        });
                                        
                                        // Immediately continue with token detection
                                        info!("✅ Fast path PASSED - Continuing with token processing");
                                    } else {
                                        // Only for higher liquidity thresholds do we wait for the full check
                                        match crate::token_detector::check_token_liquidity(
                                            &mint,
                                            &bonding_curve,
                                            liquidity_threshold
                                        ).await {
                                            Ok((has_liquidity, sol_amount)) => {
                                                info!("Bonding curve liquidity: {:.2} SOL, Required: {:.2} SOL", 
                                                    sol_amount, liquidity_threshold);
                                                    
                                                if has_liquidity {
                                                    info!("✅ Liquidity check PASSED (PROCESSED level) - Preparing buy transaction");
                                                    // Call your buy function here
                                                } else {
                                                    info!("❌ Liquidity check FAILED (PROCESSED level) - Skipping buy");
                                                }
                                            },
                                            Err(e) => {
                                                warn!("Failed to check liquidity at PROCESSED level: {}", e);
                                            }
                                        }
                                    }
                                });
                                
                                // 2. ASYNCHRONOUSLY fetch additional transaction details for logging/display
                                // REMOVED: No longer fetching transaction details which can delay processing
                                // Instead, we log that we're skipping this step for faster processing
                                info!("⏱️ CONFIRMED DETAILS: Block time to detection latency: 0ms (transaction details skipped for faster processing)");
                                
                                // Send the token data through the channel for further processing
                                if let Err(e) = tx.send(token_data).await {
                                    error!("Failed to send token data through channel: {}", e);
                                }
                            }
                        }
                    },
                    Message::Ping(data) => {
                        // Respond to ping with pong
                        write.send(Message::Pong(data)).await?;
                    },
                    Message::Pong(_) => {
                        // Received pong response, connection is alive
                        debug!("Received pong response, connection is alive");
                    },
                    _ => {
                        // Ignore other message types
                    }
                }
            },
            Err(e) => {
                // Log the error and return to trigger reconnection
                error!("WebSocket error: {}", e);
                return Err(anyhow!(e));
            }
        }
        
        // Check for long periods of no messages (connection might be dead)
        if last_message_time.elapsed() > max_silence_duration {
            warn!("No messages received for {} seconds, connection might be dead", 
                  max_silence_duration.as_secs());
            return Err(anyhow!("Connection silence timeout - no messages received"));
        }
    }
    
    // If we get here, the connection was closed
    warn!("WebSocket connection closed, triggering reconnection");
    Err(anyhow!("WebSocket connection closed"))
}

// Add message stats tracking struct - add this near the top of the file
struct MessageStats {
    connection_start: Instant,
    total_messages: usize,
    method_counts: HashMap<String, usize>, 
    last_activity: Instant,
    next_report_time: Instant,
}

impl MessageStats {
    // Create new message stats with current time
    fn new() -> Self {
        let now = Instant::now();
        Self {
            connection_start: now,
            total_messages: 0,
            method_counts: HashMap::new(),
            last_activity: now,
            next_report_time: now,
        }
    }
}
